//! The agent: account registry + one supervised lite process per account.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use tokio::process::Child;
use tokio::sync::{mpsc, watch, Notify, Semaphore};
use tokio::task::JoinHandle;

use crate::accounts::{AccountRecord, Layout};
use crate::api::{AccountView, InstanceState};
use crate::apple::AppleClient;
use crate::events::{now_millis, Event, EventBus};
use crate::lite::{pump_output, LiteOptions, LiteRuntime, Signal};
use crate::login::LoginSession;

#[derive(Clone, Debug)]
pub struct AgentConfig {
    pub listen: SocketAddr,
    pub layout: Layout,
    pub runtime: LiteRuntime,
    pub lite: LiteOptions,
    /// Required (as `Authorization: Bearer`) on management endpoints when set.
    pub api_token: Option<String>,
    /// Serve wrapper-manager's `POST /login` and `POST /logout`.
    pub legacy_management: bool,
    pub instance_concurrency: usize,
    /// How long a freshly started lite may take to report a region. Generous
    /// because lite under QEMU TCG starts slowly.
    pub ready_timeout: Duration,
    pub upstream_timeout: Duration,
    /// How many lite processes may be starting at the same time.
    pub startup_parallelism: usize,
}

impl AgentConfig {
    pub fn new(listen: SocketAddr, layout: Layout, runtime: LiteRuntime) -> Self {
        Self {
            listen,
            layout,
            runtime,
            lite: LiteOptions::default(),
            api_token: None,
            legacy_management: true,
            instance_concurrency: 2,
            ready_timeout: Duration::from_secs(240),
            upstream_timeout: Duration::from_secs(30),
            startup_parallelism: 2,
        }
    }
}

const FAIRPLAY_THRESHOLD: u32 = 5;
const FAIRPLAY_COOLDOWN: Duration = Duration::from_secs(300);
const HEALTH_INTERVAL: Duration = Duration::from_secs(30);
const HEALTH_MAX_FAILS: u32 = 3;
const BACKOFF_MIN: Duration = Duration::from_secs(2);
const BACKOFF_MAX: Duration = Duration::from_secs(120);

#[derive(Default)]
struct Circuit {
    fails: u32,
    open_until: Option<Instant>,
}

#[derive(Clone)]
struct RunState {
    state: InstanceState,
    port: Option<u16>,
    pid: Option<u32>,
    last_error: Option<String>,
    restarts: u32,
    running_since: Option<i64>,
}

pub struct Instance {
    pub id: String,
    record: Mutex<AccountRecord>,
    run: Mutex<RunState>,
    stop_tx: watch::Sender<bool>,
    task: Mutex<Option<JoinHandle<()>>>,
    pub(crate) sem: Semaphore,
    pub(crate) in_flight: AtomicUsize,
    circuit: Mutex<Circuit>,
    bus: Arc<EventBus>,
}

impl Instance {
    fn new(record: AccountRecord, concurrency: usize, bus: Arc<EventBus>) -> Arc<Self> {
        let (stop_tx, _) = watch::channel(true);
        Arc::new(Self {
            id: record.id.clone(),
            record: Mutex::new(record),
            run: Mutex::new(RunState {
                state: InstanceState::Stopped,
                port: None,
                pid: None,
                last_error: None,
                restarts: 0,
                running_since: None,
            }),
            stop_tx,
            task: Mutex::new(None),
            sem: Semaphore::new(concurrency.max(1)),
            in_flight: AtomicUsize::new(0),
            circuit: Mutex::new(Circuit::default()),
            bus,
        })
    }

    pub fn record(&self) -> AccountRecord {
        self.record.lock().unwrap().clone()
    }

    pub fn region(&self) -> String {
        self.record.lock().unwrap().region.clone()
    }

    pub fn state(&self) -> InstanceState {
        self.run.lock().unwrap().state
    }

    pub fn port(&self) -> Option<u16> {
        self.run.lock().unwrap().port
    }

    pub fn view(&self) -> AccountView {
        let rec = self.record();
        let run = self.run.lock().unwrap().clone();
        AccountView {
            id: rec.id,
            username: rec.username,
            region: rec.region,
            enabled: rec.enabled,
            state: run.state,
            port: run.port,
            pid: run.pid,
            last_error: run.last_error,
            restarts: run.restarts,
            running_since: run.running_since,
            in_flight: self.in_flight.load(Ordering::Relaxed),
            degraded: self.circuit_open(),
            created_at: rec.created_at,
        }
    }

    fn notify(&self) {
        self.bus.emit(Event::Account { account: self.view() });
    }

    fn update_run(&self, f: impl FnOnce(&mut RunState)) {
        f(&mut self.run.lock().unwrap());
        self.notify();
    }

    pub(crate) fn set_state(&self, state: InstanceState, error: Option<String>) {
        self.update_run(|r| {
            r.state = state;
            if error.is_some() {
                r.last_error = error;
            }
            if state != InstanceState::Running {
                r.running_since = None;
            }
            if matches!(state, InstanceState::Stopped | InstanceState::NeedsLogin | InstanceState::LoggingIn) {
                r.port = None;
                r.pid = None;
            }
        });
    }

    /// Updates the persisted part of the record; returns whether it changed.
    pub(crate) fn update_record(&self, f: impl FnOnce(&mut AccountRecord)) -> bool {
        let changed = {
            let mut rec = self.record.lock().unwrap();
            let before = rec.clone();
            f(&mut rec);
            *rec != before
        };
        if changed {
            self.notify();
        }
        changed
    }

    pub fn circuit_open(&self) -> bool {
        let mut c = self.circuit.lock().unwrap();
        match c.open_until {
            Some(t) if Instant::now() < t => true,
            Some(_) => {
                *c = Circuit::default();
                false
            }
            None => false,
        }
    }

    /// Counts a FairPlay failure; returns true when the circuit just opened.
    fn record_fairplay_failure(&self) -> bool {
        let mut c = self.circuit.lock().unwrap();
        if c.open_until.is_some_and(|t| Instant::now() < t) {
            return false;
        }
        c.fails += 1;
        if c.fails >= FAIRPLAY_THRESHOLD {
            c.fails = 0;
            c.open_until = Some(Instant::now() + FAIRPLAY_COOLDOWN);
            return true;
        }
        false
    }

    fn is_supervised(&self) -> bool {
        self.task.lock().unwrap().as_ref().is_some_and(|t| !t.is_finished())
    }

    #[cfg(test)]
    pub(crate) fn set_running_for_test(&self, port: u16) {
        self.update_run(|r| {
            r.state = InstanceState::Running;
            r.port = Some(port);
        });
    }

    #[cfg(test)]
    pub(crate) fn open_circuit_for_test(&self) {
        for _ in 0..FAIRPLAY_THRESHOLD {
            self.record_fairplay_failure();
        }
    }
}

pub struct Agent {
    pub cfg: AgentConfig,
    pub bus: Arc<EventBus>,
    pub apple: AppleClient,
    pub(crate) upstream: reqwest::Client,
    instances: RwLock<HashMap<String, Arc<Instance>>>,
    pub(crate) logins: Mutex<HashMap<String, Arc<LoginSession>>>,
    save_lock: Mutex<()>,
    startup: Arc<Semaphore>,
    ready: AtomicBool,
    shutdown: Notify,
}

enum RunEnd {
    Stop,
    Restart(String),
    NeedsLogin(String),
}

impl Agent {
    pub fn new(cfg: AgentConfig, bus: Arc<EventBus>) -> anyhow::Result<Arc<Self>> {
        let apple = AppleClient::new(cfg.lite.proxy.as_deref())?;
        let upstream = reqwest::Client::builder().no_proxy().timeout(cfg.upstream_timeout).build()?;
        let startup = Arc::new(Semaphore::new(cfg.startup_parallelism.max(1)));
        Ok(Arc::new(Self {
            cfg,
            bus,
            apple,
            upstream,
            instances: RwLock::new(HashMap::new()),
            logins: Mutex::new(HashMap::new()),
            save_lock: Mutex::new(()),
            startup,
            ready: AtomicBool::new(false),
            shutdown: Notify::new(),
        }))
    }

    pub fn layout(&self) -> &Layout {
        &self.cfg.layout
    }

    /// Whether the accounts persisted at startup have all been brought up
    /// (or given up on).
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }

    pub fn instance(&self, id: &str) -> Option<Arc<Instance>> {
        self.instances.read().unwrap().get(id).cloned()
    }

    pub fn instances(&self) -> Vec<Arc<Instance>> {
        let mut list: Vec<_> = self.instances.read().unwrap().values().cloned().collect();
        list.sort_by_key(|i| (i.record.lock().unwrap().created_at, i.id.clone()));
        list
    }

    pub fn views(&self) -> Vec<AccountView> {
        self.instances().iter().map(|i| i.view()).collect()
    }

    pub(crate) fn save_registry(&self) {
        let _g = self.save_lock.lock().unwrap();
        let records: Vec<AccountRecord> = self.instances().iter().map(|i| i.record()).collect();
        if let Err(e) = self.layout().save(&records) {
            tracing::error!("failed to save accounts.json: {e}");
        }
    }

    /// Inserts (or returns the existing) instance for a record.
    pub(crate) fn insert_instance(&self, record: AccountRecord) -> (Arc<Instance>, bool) {
        let mut map = self.instances.write().unwrap();
        if let Some(existing) = map.get(&record.id) {
            return (existing.clone(), false);
        }
        let inst = Instance::new(record, self.cfg.instance_concurrency, self.bus.clone());
        map.insert(inst.id.clone(), inst.clone());
        drop(map);
        inst.notify();
        (inst, true)
    }

    pub(crate) fn remove_instance(&self, id: &str) -> Option<Arc<Instance>> {
        let removed = self.instances.write().unwrap().remove(id);
        if removed.is_some() {
            self.bus.emit(Event::AccountRemoved { id: id.to_string() });
        }
        removed
    }

    /// Loads the registry (migrating a legacy single-account layout) and
    /// starts every enabled account.
    pub async fn boot(self: &Arc<Self>) {
        let layout = self.layout().clone();
        // Android app data dir expected by the Apple libraries (ANDROID_DATA=/data).
        if let Err(e) = std::fs::create_dir_all(layout.state_dir.join("data/com.apple.android.music")) {
            tracing::warn!("cannot create {}: {e}", layout.state_dir.display());
        }
        let mut records = layout.load();
        if records.is_empty() {
            match layout.migrate_legacy(now_millis()) {
                Ok(Some(rec)) => {
                    tracing::info!("migrated the existing single-account data to account {}", rec.id);
                    records.push(rec);
                }
                Ok(None) => {}
                Err(e) => tracing::warn!("legacy data migration failed: {e}"),
            }
        }
        for rec in records {
            self.insert_instance(rec);
        }
        self.save_registry();

        let to_start: Vec<_> = self.instances().into_iter().filter(|i| i.record().enabled).collect();
        tracing::info!(
            "agent starting {} of {} account(s) with runtime {}",
            to_start.len(),
            self.instances().len(),
            self.cfg.runtime.describe()
        );
        for inst in &to_start {
            self.start_instance(inst);
        }
        // Readiness = every initial instance left the startup phase.
        let agent = self.clone();
        tokio::spawn(async move {
            loop {
                let pending = to_start
                    .iter()
                    .any(|i| matches!(i.state(), InstanceState::Starting) && i.view().restarts == 0);
                if !pending {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            agent.ready.store(true, Ordering::Relaxed);
            tracing::info!("agent ready");
        });
    }

    /// Starts supervising an instance (no-op if already supervised).
    pub fn start_instance(self: &Arc<Self>, inst: &Arc<Instance>) {
        let mut task = inst.task.lock().unwrap();
        if task.as_ref().is_some_and(|t| !t.is_finished()) {
            return;
        }
        inst.stop_tx.send_replace(false);
        let rx = inst.stop_tx.subscribe();
        // Shown immediately; the supervisor confirms it.
        inst.set_state(InstanceState::Starting, None);
        *task = Some(tokio::spawn(supervise(self.clone(), inst.clone(), rx)));
    }

    /// Stops an instance and waits for its process to exit.
    pub async fn stop_instance(&self, inst: &Arc<Instance>) {
        inst.stop_tx.send_replace(true);
        let handle = inst.task.lock().unwrap().take();
        if let Some(mut h) = handle {
            if tokio::time::timeout(Duration::from_secs(20), &mut h).await.is_err() {
                h.abort();
            }
        }
        if inst.state() != InstanceState::NeedsLogin {
            inst.set_state(InstanceState::Stopped, None);
        }
    }

    /// Asks the process embedding the agent to shut down (see
    /// [`Agent::shutdown_requested`]); used by `POST /api/system/shutdown`.
    pub fn request_shutdown(&self) {
        self.shutdown.notify_one();
    }

    pub async fn shutdown_requested(&self) {
        self.shutdown.notified().await;
    }

    pub async fn shutdown(&self) {
        for inst in self.instances() {
            inst.stop_tx.send_replace(true);
        }
        for inst in self.instances() {
            self.stop_instance(&inst).await;
        }
    }

    /// Queries a lite instance's `/status` and returns its first region.
    pub(crate) async fn lite_region(&self, port: u16) -> anyhow::Result<Option<String>> {
        let v: serde_json::Value = self
            .upstream
            .get(format!("http://127.0.0.1:{port}/status"))
            .timeout(Duration::from_secs(5))
            .send()
            .await?
            .json()
            .await?;
        Ok(v.pointer("/data/regions/0")
            .and_then(|r| r.as_str())
            .filter(|r| !r.is_empty())
            .map(|r| r.to_lowercase()))
    }

    async fn run_once(&self, inst: &Arc<Instance>, stop: &mut watch::Receiver<bool>) -> RunEnd {
        let permit = tokio::select! {
            p = self.startup.clone().acquire_owned() => p.expect("semaphore closed"),
            _ = stopped(stop) => return RunEnd::Stop,
        };
        let id = inst.id.clone();
        let rec = inst.record();
        if let Err(e) = self.layout().ensure_account_dir(&id) {
            return RunEnd::Restart(format!("cannot create account dir: {e}"));
        }
        let port = match free_port() {
            Ok(p) => p,
            Err(e) => return RunEnd::Restart(format!("no free port: {e}")),
        };
        let args = self.cfg.lite.service_args(&self.layout().lite_base_dir(&id), port, rec.device_info.as_deref());
        let mut child = match self.cfg.runtime.command(&args).spawn() {
            Ok(c) => c,
            Err(e) => return RunEnd::Restart(format!("failed to spawn lite: {e}")),
        };
        let pid = child.id();
        inst.update_run(|r| {
            r.state = InstanceState::Starting;
            r.port = Some(port);
            r.pid = pid;
        });
        tracing::info!(source = %id, "lite started (pid {}, port {port})", pid.unwrap_or_default());

        let (sig_tx, mut sig_rx) = mpsc::unbounded_channel();
        if let Some(out) = child.stdout.take() {
            pump_output(out, id.clone(), sig_tx.clone());
        }
        if let Some(err) = child.stderr.take() {
            pump_output(err, id.clone(), sig_tx);
        }

        // Phase 1: wait for /status to report a region.
        let deadline = tokio::time::sleep(self.cfg.ready_timeout);
        tokio::pin!(deadline);
        let mut poll = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = stopped(stop) => {
                    terminate(&mut child).await;
                    return RunEnd::Stop;
                }
                st = child.wait() => {
                    return RunEnd::Restart(format!("lite exited during startup ({})", describe_exit(st)));
                }
                Some(sig) = sig_rx.recv() => {
                    if let Some(end) = self.on_signal(inst, sig) {
                        terminate(&mut child).await;
                        return end;
                    }
                }
                _ = &mut deadline => {
                    terminate(&mut child).await;
                    return RunEnd::Restart(format!(
                        "lite did not report a region within {}s",
                        self.cfg.ready_timeout.as_secs()
                    ));
                }
                _ = poll.tick() => {
                    if let Ok(Some(region)) = self.lite_region(port).await {
                        self.mark_running(inst, region);
                        break;
                    }
                }
            }
        }
        drop(permit);

        // Phase 2: serve, with periodic health checks.
        let mut health = tokio::time::interval_at(tokio::time::Instant::now() + HEALTH_INTERVAL, HEALTH_INTERVAL);
        let mut fails = 0;
        loop {
            tokio::select! {
                _ = stopped(stop) => {
                    terminate(&mut child).await;
                    return RunEnd::Stop;
                }
                st = child.wait() => {
                    return RunEnd::Restart(format!("lite exited ({})", describe_exit(st)));
                }
                Some(sig) = sig_rx.recv() => {
                    if let Some(end) = self.on_signal(inst, sig) {
                        terminate(&mut child).await;
                        return end;
                    }
                }
                _ = health.tick() => {
                    match self.lite_region(port).await {
                        Ok(Some(region)) => {
                            fails = 0;
                            if inst.update_record(|r| r.region = region) {
                                self.save_registry();
                            }
                        }
                        Ok(None) => fails += 1,
                        Err(e) => {
                            fails += 1;
                            tracing::debug!(source = %id, "health check failed: {e}");
                        }
                    }
                    if fails >= HEALTH_MAX_FAILS {
                        terminate(&mut child).await;
                        return RunEnd::Restart(format!("{HEALTH_MAX_FAILS} consecutive health checks failed"));
                    }
                }
            }
        }
    }

    fn mark_running(&self, inst: &Instance, region: String) {
        tracing::info!(source = %inst.id, "ready, region {region}");
        if inst.update_record(|r| r.region = region) {
            self.save_registry();
        }
        inst.update_run(|r| {
            r.state = InstanceState::Running;
            r.running_since = Some(now_millis());
            r.last_error = None;
        });
    }

    fn on_signal(&self, inst: &Instance, sig: Signal) -> Option<RunEnd> {
        match sig {
            Signal::SubscriptionDead => Some(RunEnd::NeedsLogin("no active Apple Music subscription".into())),
            Signal::SessionInvalid => Some(RunEnd::NeedsLogin("Apple ended the session; log in again".into())),
            Signal::Fairplay => {
                if inst.record_fairplay_failure() {
                    tracing::warn!(
                        source = %inst.id,
                        "{FAIRPLAY_THRESHOLD} FairPlay failures; excluding this account for {}s",
                        FAIRPLAY_COOLDOWN.as_secs()
                    );
                    inst.notify();
                }
                None
            }
            Signal::Need2fa | Signal::LoginError { .. } => None,
        }
    }
}

async fn supervise(agent: Arc<Agent>, inst: Arc<Instance>, mut stop: watch::Receiver<bool>) {
    let mut backoff = BACKOFF_MIN;
    loop {
        let started = Instant::now();
        match agent.run_once(&inst, &mut stop).await {
            RunEnd::Stop => {
                inst.set_state(InstanceState::Stopped, None);
                break;
            }
            RunEnd::NeedsLogin(reason) => {
                tracing::warn!(source = %inst.id, "{reason}");
                inst.set_state(InstanceState::NeedsLogin, Some(reason));
                break;
            }
            RunEnd::Restart(reason) => {
                if started.elapsed() > Duration::from_secs(300) {
                    backoff = BACKOFF_MIN;
                }
                tracing::warn!(source = %inst.id, "{reason}; restarting in {}s", backoff.as_secs());
                inst.update_run(|r| {
                    r.state = InstanceState::Backoff;
                    r.last_error = Some(reason);
                    r.restarts += 1;
                    r.port = None;
                    r.pid = None;
                    r.running_since = None;
                });
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = stopped(&mut stop) => {
                        inst.set_state(InstanceState::Stopped, None);
                        break;
                    }
                }
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
}

/// Resolves once a stop has been requested (or the sender is gone).
pub(crate) async fn stopped(rx: &mut watch::Receiver<bool>) {
    loop {
        if *rx.borrow_and_update() {
            return;
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

/// SIGTERM, then SIGKILL after a grace period.
pub(crate) async fn terminate(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
        if tokio::time::timeout(Duration::from_secs(5), child.wait()).await.is_ok() {
            return;
        }
    }
    let _ = child.kill().await;
}

pub(crate) fn describe_exit(st: std::io::Result<std::process::ExitStatus>) -> String {
    match st {
        Ok(s) => s.to_string(),
        Err(e) => e.to_string(),
    }
}

fn free_port() -> std::io::Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

impl Agent {
    /// Whether an account is currently managed by a supervisor task.
    pub fn is_supervised(&self, id: &str) -> bool {
        self.instance(id).is_some_and(|i| i.is_supervised())
    }
}
