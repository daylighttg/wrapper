//! Starts and stops the agent: in a QEMU guest, or as a native child process.
//! Collects its console output and broadcasts status changes to the UI.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::sync::{broadcast, watch};
use tokio::task::JoinHandle;

use super::config::{EngineKind, GuiConfig};
use super::package::DownloadProgress;
use crate::qemu::{self, QemuConfig};

const CONSOLE_CAPACITY: usize = 4000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EngineState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStatus {
    pub state: EngineState,
    pub engine: Option<EngineKind>,
    /// Unix ms of the last state change.
    pub since: i64,
    pub api_url: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ConsoleLine {
    pub seq: u64,
    pub ts: i64,
    pub text: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum HostEvent {
    Console(ConsoleLine),
    Status { status: EngineStatus },
    Download { progress: DownloadProgress },
    /// A newer release was found (`/api/host/info` has the details).
    Update { version: String },
}

#[derive(Clone, Debug)]
pub struct Paths {
    pub data_dir: PathBuf,
    pub assets_dir: PathBuf,
    /// Wrapper rootfs for the native engine.
    pub rootfs: PathBuf,
}

struct Running {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

pub struct Engine {
    status: Mutex<EngineStatus>,
    console: Mutex<VecDeque<ConsoleLine>>,
    seq: AtomicU64,
    events: broadcast::Sender<HostEvent>,
    running: tokio::sync::Mutex<Option<Running>>,
    /// API endpoint + token of the running agent, for the proxy.
    current: Mutex<Option<(String, String)>>,
    http: reqwest::Client,
}

pub fn now_millis() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or_default()
}

/// Removes ANSI escape sequences (colored logs from older guests, QEMU).
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

impl Engine {
    pub fn new() -> Arc<Self> {
        let (events, _) = broadcast::channel(2048);
        Arc::new(Self {
            status: Mutex::new(EngineStatus {
                state: EngineState::Stopped,
                engine: None,
                since: now_millis(),
                api_url: None,
                last_error: None,
            }),
            console: Mutex::new(VecDeque::with_capacity(CONSOLE_CAPACITY)),
            seq: AtomicU64::new(0),
            events,
            running: tokio::sync::Mutex::new(None),
            current: Mutex::new(None),
            http: reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(3)).build().expect("http client"),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<HostEvent> {
        self.events.subscribe()
    }

    pub fn emit(&self, ev: HostEvent) {
        let _ = self.events.send(ev);
    }

    pub fn status(&self) -> EngineStatus {
        self.status.lock().unwrap().clone()
    }

    /// `(api_url, token)` of the running agent.
    pub fn current_api(&self) -> Option<(String, String)> {
        self.current.lock().unwrap().clone()
    }

    fn set_status(&self, f: impl FnOnce(&mut EngineStatus)) {
        let snapshot = {
            let mut s = self.status.lock().unwrap();
            let before = s.state;
            f(&mut s);
            if s.state != before {
                s.since = now_millis();
            }
            s.clone()
        };
        self.emit(HostEvent::Status { status: snapshot });
    }

    pub fn console_line(&self, text: &str) {
        let line = ConsoleLine {
            seq: self.seq.fetch_add(1, Ordering::Relaxed) + 1,
            ts: now_millis(),
            text: strip_ansi(text),
        };
        {
            let mut c = self.console.lock().unwrap();
            if c.len() >= CONSOLE_CAPACITY {
                c.pop_front();
            }
            c.push_back(line.clone());
        }
        tracing::debug!(target: "console", "{}", line.text);
        self.emit(HostEvent::Console(line));
    }

    pub fn console(&self, after: Option<u64>, limit: usize) -> Vec<ConsoleLine> {
        let c = self.console.lock().unwrap();
        let mut v: Vec<_> = c.iter().rev().filter(|l| after.map_or(true, |a| l.seq > a)).take(limit).cloned().collect();
        v.reverse();
        v
    }

    pub async fn start(self: &Arc<Self>, cfg: GuiConfig, paths: Paths) -> Result<(), String> {
        cfg.validate()?;
        let mut running = self.running.lock().await;
        if running.as_ref().is_some_and(|r| !r.task.is_finished()) {
            return Err("the engine is already running".into());
        }
        let (stop_tx, stop_rx) = watch::channel(false);
        let api_url = cfg.api_url();
        self.set_status(|s| {
            s.state = EngineState::Starting;
            s.engine = Some(cfg.engine);
            s.api_url = Some(api_url.clone());
            s.last_error = None;
        });
        *self.current.lock().unwrap() = Some((api_url.clone(), cfg.api_token.clone()));

        let engine = self.clone();
        let task = match cfg.engine {
            EngineKind::Qemu => {
                let qcfg = qemu_config(&cfg, &paths);
                let status = qemu::check_assets(&qcfg);
                if !status.ready() {
                    let msg = if status.qemu.is_none() {
                        "QEMU was not found; download the QEMU package first".to_string()
                    } else {
                        format!("guest files are missing from {}; download the QEMU package first", paths.assets_dir.display())
                    };
                    self.fail(msg.clone());
                    return Err(msg);
                }
                tokio::spawn(async move {
                    let sink_engine = engine.clone();
                    let sink: qemu::LineSink = Arc::new(move |l: &str| sink_engine.console_line(l));
                    let result = qemu::run_vm(&qcfg, stop_rx, sink).await;
                    engine.finished(result.map(|st| st.success()).map_err(|e| format!("{e:#}")));
                })
            }
            EngineKind::Native => {
                let rootfs = paths.rootfs.clone();
                if !rootfs.join("system/bin/lite").is_file() {
                    let msg = format!("{} does not contain system/bin/lite", rootfs.display());
                    self.fail(msg.clone());
                    return Err(msg);
                }
                tokio::spawn(async move {
                    let result = engine.run_native(&cfg, rootfs, stop_rx).await;
                    engine.finished(result);
                })
            }
        };
        // Starting -> Running once the agent answers.
        let probe = self.clone();
        tokio::spawn(async move { probe.wait_reachable(&api_url).await });

        *running = Some(Running { stop: stop_tx, task });
        Ok(())
    }

    async fn wait_reachable(&self, url: &str) {
        loop {
            if self.status().state != EngineState::Starting {
                return;
            }
            if let Ok(r) = self.http.get(format!("{url}/status")).send().await {
                if r.status().is_success() {
                    self.set_status(|s| {
                        if s.state == EngineState::Starting {
                            s.state = EngineState::Running;
                        }
                    });
                    return;
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    fn fail(&self, msg: String) {
        self.console_line(&format!("[gui] {msg}"));
        *self.current.lock().unwrap() = None;
        self.set_status(|s| {
            s.state = EngineState::Failed;
            s.last_error = Some(msg);
        });
    }

    /// Called when the engine task ends; `Ok(true)` is a clean exit.
    fn finished(&self, result: Result<bool, String>) {
        let stopping = self.status().state == EngineState::Stopping;
        match result {
            Ok(true) => {}
            _ if stopping => {}
            Ok(false) => return self.fail("the engine exited unexpectedly; see the console".into()),
            Err(e) => return self.fail(e),
        }
        *self.current.lock().unwrap() = None;
        self.console_line("[gui] engine stopped");
        self.set_status(|s| s.state = EngineState::Stopped);
    }

    pub async fn stop(&self) {
        let running = self.running.lock().await.take();
        let Some(r) = running else {
            return;
        };
        if r.task.is_finished() {
            return;
        }
        self.set_status(|s| s.state = EngineState::Stopping);
        let _ = r.stop.send(true);
        let mut task = r.task;
        if tokio::time::timeout(Duration::from_secs(45), &mut task).await.is_err() {
            task.abort();
            self.finished(Ok(true));
        }
    }

    async fn run_native(self: &Arc<Self>, cfg: &GuiConfig, rootfs: PathBuf, mut stop: watch::Receiver<bool>) -> Result<bool, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut cmd = tokio::process::Command::new(exe);
        cmd.arg("agent")
            .arg("--runtime")
            .arg("sandbox")
            .arg("--rootfs")
            .arg(&rootfs)
            .args(["--host", &cfg.api_host, "--port", &cfg.api_port.to_string()])
            .args(["--api-token", &cfg.api_token])
            .args(cfg.agent_args())
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(target_os = "linux")]
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                Ok(())
            });
        }
        self.console_line(&format!("[gui] starting the native agent on {}", cfg.api_url()));
        let mut child = cmd.spawn().map_err(|e| format!("failed to start the agent: {e}"))?;
        if let Some(o) = child.stdout.take() {
            self.pump(o);
        }
        if let Some(e) = child.stderr.take() {
            self.pump(e);
        }
        let status = tokio::select! {
            st = child.wait() => st,
            _ = async { while !*stop.borrow_and_update() { if stop.changed().await.is_err() { std::future::pending::<()>().await } } } => {
                #[cfg(unix)]
                if let Some(pid) = child.id() {
                    unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM); }
                }
                match tokio::time::timeout(Duration::from_secs(30), child.wait()).await {
                    Ok(st) => st,
                    Err(_) => {
                        let _ = child.kill().await;
                        child.wait().await
                    }
                }
            }
        };
        status.map(|s| s.success()).map_err(|e| e.to_string())
    }

    fn pump<R: AsyncRead + Unpin + Send + 'static>(self: &Arc<Self>, r: R) {
        let me = self.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(r).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if !line.trim().is_empty() {
                    me.console_line(line.trim_end_matches('\r'));
                }
            }
        });
    }
}

pub fn qemu_config(cfg: &GuiConfig, paths: &Paths) -> QemuConfig {
    let mut q = QemuConfig::new(paths.assets_dir.clone());
    q.data_img = paths.assets_dir.join("data.img");
    q.disk_size_mb = cfg.disk_size_mb;
    q.accel = Some(cfg.accel.clone()).filter(|a| a != "auto");
    q.memory_mb = cfg.memory_mb;
    q.smp = cfg.smp;
    q.host = cfg.api_host.clone();
    q.host_port = cfg.api_port;
    q.api_token = Some(cfg.api_token.clone()).filter(|t| !t.is_empty());
    q.agent_args = cfg.agent_args();
    q.hide_window = true;
    q
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ansi() {
        assert_eq!(strip_ansi("\u{1b}[2m2026\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m hi"), "2026  INFO hi");
        assert_eq!(strip_ansi("plain"), "plain");
    }
}
