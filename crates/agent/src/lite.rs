//! Spawning wrapper-lite processes and making sense of their output.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::LazyLock;

use regex::Regex;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::events::Level;

/// How lite processes are started.
#[derive(Clone, Debug)]
pub enum LiteRuntime {
    /// lite runs in our own root filesystem (QEMU guest initramfs).
    Direct { lite_bin: PathBuf },
    /// lite runs chrooted into `rootfs` through `<helper> sandbox-exec`
    /// (user namespaces unless `privileged`).
    Sandbox { helper: PathBuf, rootfs: PathBuf, privileged: bool },
}

impl LiteRuntime {
    pub fn describe(&self) -> String {
        match self {
            LiteRuntime::Direct { lite_bin } => format!("direct ({})", lite_bin.display()),
            LiteRuntime::Sandbox { rootfs, privileged, .. } => format!(
                "sandbox ({}, {})",
                rootfs.display(),
                if *privileged { "privileged" } else { "rootless" }
            ),
        }
    }

    /// Builds the command for `lite <args>` with piped output. The child is
    /// killed when the handle is dropped and, on Linux, when we die.
    pub fn command(&self, args: &[String]) -> Command {
        let mut cmd = match self {
            LiteRuntime::Direct { lite_bin } => {
                let mut c = Command::new(lite_bin);
                c.args(args);
                c
            }
            LiteRuntime::Sandbox { helper, rootfs, privileged } => {
                let mut c = Command::new(helper);
                c.arg("sandbox-exec").arg("--rootfs").arg(rootfs);
                if *privileged {
                    c.arg("--privileged");
                }
                c.arg("--").arg("/system/bin/lite").args(args);
                c
            }
        };
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(target_os = "linux")]
        unsafe {
            cmd.pre_exec(|| {
                // Do not leave orphaned lite processes behind if the agent dies.
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
        cmd
    }
}

/// Options shared by every lite invocation.
#[derive(Clone, Debug)]
pub struct LiteOptions {
    pub proxy: Option<String>,
    /// Default `--device-info` for accounts without their own override.
    pub device_info: Option<String>,
    pub log_level: String,
    pub token_refresh_interval: u32,
    /// Seconds lite waits for `2fa.txt` (0: lite's default).
    pub code_timeout: u32,
}

impl Default for LiteOptions {
    fn default() -> Self {
        Self {
            proxy: None,
            device_info: None,
            log_level: "info".into(),
            token_refresh_interval: 1800,
            code_timeout: 300,
        }
    }
}

impl LiteOptions {
    fn common(&self, base_dir: &str, device_info: Option<&str>) -> Vec<String> {
        let mut args = vec!["--base-dir".to_string(), base_dir.to_string()];
        if let Some(p) = self.proxy.as_deref().filter(|p| !p.is_empty()) {
            args.extend(["--proxy".into(), p.into()]);
        }
        if let Some(d) = device_info.or(self.device_info.as_deref()).filter(|d| !d.is_empty()) {
            args.extend(["--device-info".into(), d.into()]);
        }
        args.extend(["--log-level".into(), self.log_level.clone()]);
        args
    }

    pub fn login_args(&self, base_dir: &str, username: &str, password: &str, device_info: Option<&str>) -> Vec<String> {
        let mut args = vec!["--login".to_string(), format!("{username}:{password}"), "--code-from-file".into()];
        // 0 = leave lite's default (and stay compatible with builds that
        // predate --code-timeout).
        if self.code_timeout > 0 {
            args.extend(["--code-timeout".into(), self.code_timeout.to_string()]);
        }
        args.extend(self.common(base_dir, device_info));
        args
    }

    pub fn service_args(&self, base_dir: &str, port: u16, device_info: Option<&str>) -> Vec<String> {
        let mut args = vec![
            "--host".to_string(),
            "127.0.0.1".into(),
            "--port".into(),
            port.to_string(),
            "--token-refresh-interval".into(),
            self.token_refresh_interval.to_string(),
        ];
        args.extend(self.common(base_dir, device_info));
        args
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiteLine {
    pub level: Level,
    pub msg: String,
}

static LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3} \[(DEBUG|INFO |WARN |ERROR)\] (.*)$").unwrap()
});

/// Parses `2026-01-01 00:00:00.000 [INFO ] message`; anything else is kept
/// verbatim at info level (QEMU/kernel output, launcher errors, ...).
pub fn parse_line(raw: &str) -> LiteLine {
    let raw = raw.trim_end_matches(['\r', '\n']);
    if let Some(c) = LINE_RE.captures(raw) {
        let level = match &c[1] {
            "DEBUG" => Level::Debug,
            "WARN " => Level::Warn,
            "ERROR" => Level::Error,
            _ => Level::Info,
        };
        return LiteLine { level, msg: c[2].to_string() };
    }
    LiteLine { level: Level::Info, msg: raw.to_string() }
}

/// Events recognised in lite output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Signal {
    /// lite is waiting for `2fa.txt`.
    Need2fa,
    /// Why the login failed. `primary` marks Apple's own user-facing
    /// message, which beats the technical details that follow it.
    LoginError { reason: String, primary: bool },
    /// "No Active Subscription": the account cannot serve anything.
    SubscriptionDead,
    /// Apple no longer accepts the session; a fresh login is required.
    SessionInvalid,
    /// Apple refused to process a content key on this instance.
    Fairplay,
}

pub fn classify(msg: &str) -> Option<Signal> {
    let l = msg.to_lowercase();
    if l.contains("enter your 2fa code into") {
        return Some(Signal::Need2fa);
    }
    let failed = |reason: String, primary: bool| Some(Signal::LoginError { reason, primary });
    if let Some(rest) = after(msg, &l, "server message:") {
        return failed(rest, true);
    }
    if let Some(rest) = after(msg, &l, "auth error:") {
        // "code=-5000, message=..." -> keep the message part when present.
        let reason = rest.split_once("message=").map(|(_, m)| m.trim().to_string()).unwrap_or(rest);
        return failed(reason, false);
    }
    if l.contains("2fa code timeout") {
        return failed("timed out waiting for the 2FA code".into(), true);
    }
    if let Some(arg) = l.strip_prefix("unknown argument:") {
        return failed(
            format!("lite does not understand `{}`: the lite build (guest image or rootfs) is older than this wrapper", arg.trim()),
            true,
        );
    }
    if l.contains("invalid login format") {
        return failed("invalid login format".into(), true);
    }
    if l.contains("auth failed") {
        return failed("authentication failed".into(), false);
    }
    if l.contains("no active subscription") {
        return Some(Signal::SubscriptionDead);
    }
    const SESSION: [&str; 3] = [
        "check the account information you entered and try again",
        "your session has ended",
        "sign in to continue",
    ];
    if SESSION.iter().any(|s| l.contains(s)) {
        return Some(Signal::SessionInvalid);
    }
    if l.contains("fairplay error") || l.contains("kdcanprocessckc") {
        return Some(Signal::Fairplay);
    }
    None
}

fn after(msg: &str, lower: &str, needle: &str) -> Option<String> {
    let i = lower.find(needle)?;
    let rest = msg[i + needle.len()..].trim();
    (!rest.is_empty()).then(|| rest.to_string())
}

/// Logs every line of a child's output under `source` and forwards
/// recognised [`Signal`]s.
pub fn pump_output<R>(reader: R, source: String, signals: mpsc::UnboundedSender<Signal>)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(raw)) = lines.next_line().await {
            if raw.trim().is_empty() {
                continue;
            }
            let line = parse_line(&raw);
            // Health checks would drown everything else.
            if line.msg.starts_with("request: GET /status") {
                continue;
            }
            match line.level {
                Level::Debug => tracing::debug!(source = %source, "{}", line.msg),
                Level::Info => tracing::info!(source = %source, "{}", line.msg),
                Level::Warn => tracing::warn!(source = %source, "{}", line.msg),
                Level::Error => tracing::error!(source = %source, "{}", line.msg),
            }
            if let Some(sig) = classify(&line.msg) {
                let _ = signals.send(sig);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lite_log_lines() {
        let l = parse_line("2026-10-11 12:00:00.123 [WARN ] Enter your 2FA code into /data/x/2fa.txt\n");
        assert_eq!(l.level, Level::Warn);
        assert_eq!(l.msg, "Enter your 2FA code into /data/x/2fa.txt");
        let raw = parse_line("[init] booting");
        assert_eq!(raw, LiteLine { level: Level::Info, msg: "[init] booting".into() });
    }

    #[test]
    fn classifies_signals() {
        assert_eq!(classify("Enter your 2FA code into /data/2fa.txt"), Some(Signal::Need2fa));
        assert_eq!(
            classify("server message: Your Apple ID or password was incorrect."),
            Some(Signal::LoginError { reason: "Your Apple ID or password was incorrect.".into(), primary: true })
        );
        assert_eq!(
            classify("auth error: code=-5000, message=Bad creds"),
            Some(Signal::LoginError { reason: "Bad creds".into(), primary: false })
        );
        assert_eq!(classify("dialog: No Active Subscription"), Some(Signal::SubscriptionDead));
        assert_eq!(classify("KDCanProcessCKC failed -42"), Some(Signal::Fairplay));
        assert_eq!(classify("request: GET /m3u8"), None);
        assert!(matches!(
            classify("unknown argument: --code-timeout"),
            Some(Signal::LoginError { primary: true, ref reason }) if reason.contains("--code-timeout")
        ));
    }

    #[test]
    fn builds_args() {
        let o = LiteOptions { proxy: Some("http://p:1".into()), ..Default::default() };
        let a = o.service_args("/data/accounts/x", 4000, None);
        assert!(a.windows(2).any(|w| w == ["--port", "4000"]));
        assert!(a.windows(2).any(|w| w == ["--proxy", "http://p:1"]));
        assert!(!a.contains(&"--device-info".to_string()));
        let l = o.login_args("/b", "u", "p:w", Some("dev"));
        assert_eq!(l[1], "u:p:w");
        assert!(l.windows(2).any(|w| w == ["--device-info", "dev"]));
    }
}
