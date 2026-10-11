//! Boots the wrapper guest (kernel + initramfs with lite and `wrapper agent`)
//! in QEMU and forwards one host port to the agent inside.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::watch;

/// Port the agent listens on inside the guest.
pub const GUEST_PORT: u16 = 12340;
pub const KERNEL: &str = "vmlinuz-lite-qemu";
pub const INITRD: &str = "lite-initramfs.cpio.gz";
/// fw_cfg entry carrying extra `wrapper agent` arguments, one per line.
pub const FW_CFG_ARGS: &str = "opt/wrapper/args";

/// A failed boot within this window counts as "acceleration unavailable".
const ACCEL_PROBE_WINDOW: Duration = Duration::from_secs(20);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(30);

pub type LineSink = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone, Debug)]
pub struct QemuConfig {
    /// Directory holding the kernel, initramfs and `bin/` (bundled QEMU).
    pub assets_dir: PathBuf,
    /// Persistent raw ext4 disk mounted at /data in the guest.
    pub data_img: PathBuf,
    /// Size of a newly created data disk.
    pub disk_size_mb: u64,
    pub qemu_bin: Option<PathBuf>,
    /// `None` picks one per host (see [`auto_accel`]).
    pub accel: Option<String>,
    pub memory_mb: u32,
    pub smp: u32,
    /// Host address the forwarded port binds to.
    pub host: String,
    pub host_port: u16,
    /// Management token for the agent (also used for graceful shutdown).
    pub api_token: Option<String>,
    /// Extra `wrapper agent` arguments.
    pub agent_args: Vec<String>,
    /// Do not open a console window for QEMU (Windows GUI use).
    pub hide_window: bool,
}

impl QemuConfig {
    pub fn new(assets_dir: PathBuf) -> Self {
        Self {
            data_img: assets_dir.join("data.img"),
            assets_dir,
            disk_size_mb: 512,
            qemu_bin: None,
            accel: None,
            memory_mb: 1024,
            smp: 2,
            host: "127.0.0.1".into(),
            host_port: 12340,
            api_token: None,
            agent_args: Vec::new(),
            hide_window: false,
        }
    }

    /// Base URL to reach the forwarded agent from this machine.
    pub fn api_base(&self) -> String {
        let host = match self.host.as_str() {
            "" | "0.0.0.0" => "127.0.0.1",
            "::" => "[::1]",
            h => h,
        };
        format!("http://{host}:{}", self.host_port)
    }

    pub fn bin_dir(&self) -> PathBuf {
        self.assets_dir.join("bin")
    }

    /// All arguments passed to `wrapper agent` in the guest.
    pub fn guest_agent_args(&self) -> Vec<String> {
        let mut args = self.agent_args.clone();
        if let Some(t) = self.api_token.as_deref().filter(|t| !t.is_empty()) {
            args.extend(["--api-token".into(), t.into()]);
        }
        args
    }
}

pub fn qemu_exe_name() -> &'static str {
    if cfg!(windows) {
        "qemu-system-x86_64.exe"
    } else {
        "qemu-system-x86_64"
    }
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|p| p.is_file())
}

/// Explicit path > `QEMU_BIN` > bundled `bin/` > `PATH` > well-known dirs.
pub fn locate_qemu(cfg: &QemuConfig) -> Option<PathBuf> {
    if let Some(p) = &cfg.qemu_bin {
        return Some(p.clone());
    }
    if let Some(p) = std::env::var_os("QEMU_BIN").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let name = qemu_exe_name();
    let bundled = cfg.bin_dir().join(name);
    if bundled.is_file() {
        return Some(bundled);
    }
    if let Some(p) = find_on_path(name) {
        return Some(p);
    }
    let known: &[&str] = if cfg!(windows) {
        &[r"C:\Program Files\qemu", r"C:\ProgramData\chocolatey\bin"]
    } else if cfg!(target_os = "macos") {
        &["/opt/homebrew/bin", "/usr/local/bin"]
    } else {
        &["/usr/bin", "/usr/local/bin", "/data/data/com.termux/files/usr/bin"]
    };
    known.iter().map(|d| Path::new(d).join(name)).find(|p| p.is_file())
}

pub fn auto_accel() -> &'static str {
    #[cfg(target_os = "linux")]
    {
        let kvm = std::fs::OpenOptions::new().read(true).write(true).open("/dev/kvm").is_ok();
        if kvm {
            "kvm"
        } else {
            "tcg"
        }
    }
    #[cfg(windows)]
    {
        "whpx"
    }
    // HVF only accelerates x86_64 guests on Intel Macs.
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "hvf"
    }
    #[cfg(not(any(target_os = "linux", windows, all(target_os = "macos", target_arch = "x86_64"))))]
    {
        "tcg"
    }
}

fn accel_args(accel: &str, smp: u32) -> [String; 4] {
    let (accel_opt, cpu) = match accel {
        "whpx" => ("whpx,kernel-irqchip=off".to_string(), "qemu64-v1"),
        "kvm" | "hvf" => (accel.to_string(), "host"),
        "tcg" if smp > 1 => ("tcg,thread=multi,tb-size=128".to_string(), "Westmere"),
        "tcg" => ("tcg,tb-size=128".to_string(), "Westmere"),
        other => (other.to_string(), "max"),
    };
    ["-accel".into(), accel_opt, "-cpu".into(), cpu.into()]
}

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[derive(Debug)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub envs: Vec<(String, OsString)>,
}

/// The complete QEMU invocation.
pub fn build_launch(cfg: &QemuConfig, qemu: &Path, accel: &str, args_file: &Path, args_content: &str) -> Launch {
    let mut args: Vec<OsString> = Vec::new();
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut envs = Vec::new();
    let bin = cfg.bin_dir();
    if bin.is_dir() {
        // Bundled firmware (SeaBIOS, option ROMs).
        args.extend(["-L".into(), bin.clone().into_os_string()]);
        #[cfg(unix)]
        {
            // The bundled QEMU may link against libraries shipped next to it.
            let mut dirs = vec![bin.clone()];
            if cfg.assets_dir.join("lib").is_dir() {
                dirs.push(cfg.assets_dir.join("lib"));
            }
            if let Some(qdir) = qemu.parent().filter(|d| *d != bin && d.is_dir()) {
                dirs.push(qdir.to_path_buf());
            }
            if let Some(existing) = std::env::var_os("LD_LIBRARY_PATH") {
                dirs.extend(std::env::split_paths(&existing));
            }
            if let Ok(joined) = std::env::join_paths(dirs) {
                envs.push(("LD_LIBRARY_PATH".to_string(), joined));
            }
            envs.push(("QEMU_MODULE_DIR".to_string(), bin.clone().into_os_string()));
        }
    }
    args.extend(accel_args(accel, cfg.smp).map(OsString::from));
    let mut append = "console=ttyS0 quiet loglevel=3 net.ifnames=0 biosdevname=0 panic=-1".to_string();
    if !args_content.is_empty() {
        // Fallback for QEMU builds without fw_cfg file support.
        append.push_str(" wrapper_args_b64=");
        append.push_str(&base64(args_content.as_bytes()));
    }
    let hostfwd = format!("user,model=e1000,hostfwd=tcp:{}:{}-:{GUEST_PORT}", cfg.host, cfg.host_port);
    let drive = format!("file={},format=raw,if=virtio,cache=writeback", cfg.data_img.display());
    for a in [
        "-m".to_string(),
        cfg.memory_mb.to_string(),
        "-smp".into(),
        cfg.smp.to_string(),
        "-kernel".into(),
        cfg.assets_dir.join(KERNEL).display().to_string(),
        "-initrd".into(),
        cfg.assets_dir.join(INITRD).display().to_string(),
        "-append".into(),
        append,
        "-display".into(),
        "none".into(),
        "-monitor".into(),
        "none".into(),
        "-serial".into(),
        "stdio".into(),
        "-no-reboot".into(),
        "-nic".into(),
        hostfwd,
        "-drive".into(),
        drive,
    ] {
        args.push(a.into());
    }
    if !args_content.is_empty() {
        args.push("-fw_cfg".into());
        args.push(format!("name={FW_CFG_ARGS},file={}", args_file.display()).into());
    }
    Launch { program: qemu.to_path_buf(), args, envs }
}

#[derive(Debug, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetStatus {
    pub qemu: Option<PathBuf>,
    pub kernel: bool,
    pub initrd: bool,
    pub data_img: bool,
}

impl AssetStatus {
    pub fn ready(&self) -> bool {
        self.qemu.is_some() && self.kernel && self.initrd
    }
}

pub fn check_assets(cfg: &QemuConfig) -> AssetStatus {
    AssetStatus {
        qemu: locate_qemu(cfg),
        kernel: cfg.assets_dir.join(KERNEL).is_file(),
        initrd: cfg.assets_dir.join(INITRD).is_file(),
        data_img: cfg.data_img.is_file(),
    }
}

/// Creates a blank (sparse where supported) data disk; the guest formats it on
/// first boot. Returns whether a new image was created.
pub fn ensure_data_image(path: &Path, size_mb: u64) -> std::io::Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let f = std::fs::File::create(path)?;
    f.set_len(size_mb.max(64) * 1024 * 1024)?;
    Ok(true)
}

fn pump<R: AsyncRead + Unpin + Send + 'static>(r: R, sink: LineSink) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(r).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let line = line.trim_end_matches('\r');
            if !line.trim().is_empty() {
                sink(line);
            }
        }
    });
}

fn spawn(launch: &Launch, hide_window: bool) -> std::io::Result<Child> {
    let mut cmd = Command::new(&launch.program);
    cmd.args(&launch.args)
        .envs(launch.envs.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    if hide_window {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = hide_window;
    #[cfg(target_os = "linux")]
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
    cmd.spawn()
}

async fn stopped(rx: &mut watch::Receiver<bool>) {
    loop {
        if *rx.borrow_and_update() {
            return;
        }
        if rx.changed().await.is_err() {
            // Sender gone: nobody can ask us to stop any more.
            std::future::pending::<()>().await;
        }
    }
}

/// Asks the agent to stop its lite processes and power the guest off, then
/// kills QEMU if it does not exit in time.
async fn graceful_shutdown(cfg: &QemuConfig, child: &mut Child, sink: &LineSink) -> std::io::Result<ExitStatus> {
    sink("[qemu] stopping the guest");
    let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build();
    if let Ok(client) = client {
        let mut req = client.post(format!("{}/api/system/shutdown", cfg.api_base()));
        if let Some(t) = cfg.api_token.as_deref().filter(|t| !t.is_empty()) {
            req = req.bearer_auth(t);
        }
        if let Err(e) = req.send().await {
            sink(&format!("[qemu] shutdown request failed ({e}); killing QEMU"));
            child.kill().await?;
        }
    }
    match tokio::time::timeout(SHUTDOWN_GRACE, child.wait()).await {
        Ok(st) => st,
        Err(_) => {
            sink("[qemu] guest did not power off in time; killing QEMU");
            child.kill().await?;
            child.wait().await
        }
    }
}

/// Runs the VM until it exits or `stop` becomes true. Falls back to TCG when
/// the automatically chosen accelerator fails right away.
pub async fn run_vm(cfg: &QemuConfig, mut stop: watch::Receiver<bool>, sink: LineSink) -> anyhow::Result<ExitStatus> {
    let qemu = locate_qemu(cfg).context("qemu-system-x86_64 not found (install QEMU or download the QEMU package)")?;
    for f in [KERNEL, INITRD] {
        if !cfg.assets_dir.join(f).is_file() {
            bail!("{} is missing from {}", f, cfg.assets_dir.display());
        }
    }
    if ensure_data_image(&cfg.data_img, cfg.disk_size_mb)? {
        sink(&format!("[qemu] created a {} MB data disk at {}", cfg.disk_size_mb, cfg.data_img.display()));
    }
    let agent_args = cfg.guest_agent_args();
    if agent_args.iter().any(|a| a.contains('\n')) {
        bail!("agent arguments must not contain line breaks");
    }
    let content = if agent_args.is_empty() { String::new() } else { agent_args.join("\n") + "\n" };
    let args_file = cfg.data_img.with_file_name(".wrapper-agent-args");
    std::fs::write(&args_file, &content).with_context(|| format!("cannot write {}", args_file.display()))?;

    let forced = cfg.accel.as_deref().is_some_and(|a| !a.is_empty() && a != "auto");
    let mut accel = cfg.accel.clone().filter(|_| forced).unwrap_or_else(|| auto_accel().to_string());
    let result = loop {
        let launch = build_launch(cfg, &qemu, &accel, &args_file, &content);
        sink(&format!(
            "[qemu] starting guest: accel={accel}, memory={} MB, smp={}, {} -> guest:{GUEST_PORT}",
            cfg.memory_mb,
            cfg.smp,
            cfg.api_base()
        ));
        let mut child = match spawn(&launch, cfg.hide_window) {
            Ok(c) => c,
            Err(e) => break Err(anyhow::anyhow!("failed to start {}: {e}", qemu.display())),
        };
        if let Some(out) = child.stdout.take() {
            pump(out, sink.clone());
        }
        if let Some(err) = child.stderr.take() {
            pump(err, sink.clone());
        }
        let started = Instant::now();
        let status = tokio::select! {
            st = child.wait() => st,
            _ = stopped(&mut stop) => break graceful_shutdown(cfg, &mut child, &sink).await.map_err(Into::into),
        };
        let status = match status {
            Ok(s) => s,
            Err(e) => break Err(e.into()),
        };
        if !status.success() && !forced && accel != "tcg" && started.elapsed() < ACCEL_PROBE_WINDOW {
            sink(&format!("[qemu] {accel} acceleration unavailable; falling back to tcg"));
            accel = "tcg".into();
            continue;
        }
        break Ok(status);
    };
    // It may contain the API token.
    let _ = std::fs::remove_file(&args_file);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_reference() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"--proxy\nhttp://x\n"), "LS1wcm94eQpodHRwOi8veAo=");
    }

    #[test]
    fn launch_has_forwarding_disk_and_args() {
        let mut cfg = QemuConfig::new(PathBuf::from("assets"));
        cfg.host = "0.0.0.0".into();
        cfg.host_port = 23456;
        let l = build_launch(&cfg, Path::new("qemu"), "tcg", Path::new("args"), "--x\n");
        let args: Vec<String> = l.args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(args.contains(&"user,model=e1000,hostfwd=tcp:0.0.0.0:23456-:12340".to_string()));
        assert!(args.iter().any(|a| a.starts_with("name=opt/wrapper/args,file=")));
        assert!(args.windows(2).any(|w| w[0] == "-accel" && w[1].starts_with("tcg,thread=multi")));
        assert!(args.iter().any(|a| a.contains("wrapper_args_b64=LS14Cg==")));
        assert_eq!(cfg.api_base(), "http://127.0.0.1:23456");

        let l = build_launch(&cfg, Path::new("qemu"), "kvm", Path::new("args"), "");
        let args: Vec<String> = l.args.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(!args.iter().any(|a| a == "-fw_cfg"));
        assert!(args.windows(2).any(|w| w == ["-cpu", "host"]));
    }

    #[test]
    fn guest_args_include_token() {
        let mut cfg = QemuConfig::new(PathBuf::from("a"));
        cfg.agent_args = vec!["--proxy".into(), "http://p".into()];
        cfg.api_token = Some("t".into());
        assert_eq!(cfg.guest_agent_args(), ["--proxy", "http://p", "--api-token", "t"]);
    }
}
