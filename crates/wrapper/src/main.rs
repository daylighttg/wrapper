//! `wrapper`: one binary for every role.
//!
//! * `wrapper agent` — multi-account supervisor + aggregated API (Linux: in
//!   the QEMU guest, or natively with a user-namespace sandbox).
//! * `wrapper sandbox-exec` — internal helper used by the agent to start lite
//!   inside the rootfs chroot.

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context};
use clap::{Args, Parser, Subcommand, ValueEnum};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};
use wrapper_agent::{Agent, AgentConfig, BusLayer, EventBus, Layout, LiteOptions, LiteRuntime};

#[derive(Parser)]
#[command(name = "wrapper", version, about = "wrapper-lite multi-account manager")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the multi-account agent (Linux only).
    Agent(Box<AgentArgs>),
    /// Internal: run a program chrooted into the wrapper rootfs.
    #[command(hide = true)]
    SandboxExec(SandboxArgs),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum RuntimeKind {
    /// `direct` when /system/bin/lite exists (QEMU guest), else `sandbox`.
    Auto,
    /// Start lite directly; the current root is the wrapper rootfs.
    Direct,
    /// chroot into the rootfs inside a user namespace (no root needed).
    Sandbox,
    /// chroot into the rootfs using real root privileges.
    SandboxPrivileged,
}

#[derive(Args)]
struct AgentArgs {
    /// Address the aggregated API listens on.
    #[arg(long, default_value = "127.0.0.1", env = "WRAPPER_HOST")]
    host: String,
    #[arg(long, default_value_t = 12340, env = "WRAPPER_PORT")]
    port: u16,
    #[arg(long, value_enum, default_value_t = RuntimeKind::Auto)]
    runtime: RuntimeKind,
    /// Wrapper rootfs for the sandbox runtimes [default: <exe dir>/rootfs].
    #[arg(long)]
    rootfs: Option<PathBuf>,
    /// lite binary for the direct runtime.
    #[arg(long, default_value = "/system/bin/lite")]
    lite_bin: PathBuf,
    /// State directory for the direct runtime (sandbox: always <rootfs>/data).
    #[arg(long, default_value = "/data")]
    state_dir: PathBuf,
    /// Proxy for lite and for Apple catalog probes.
    #[arg(long, env = "WRAPPER_PROXY")]
    proxy: Option<String>,
    /// Default lite --device-info for accounts without their own.
    #[arg(long)]
    device_info: Option<String>,
    #[arg(long, default_value = "info")]
    lite_log_level: String,
    #[arg(long, default_value_t = 1800)]
    token_refresh_interval: u32,
    /// Seconds lite waits for a 2FA code (0: lite's default of 60).
    #[arg(long, default_value_t = 300)]
    code_timeout: u32,
    /// Require this bearer token on management endpoints.
    #[arg(long, env = "WRAPPER_API_TOKEN")]
    api_token: Option<String>,
    /// Do not serve wrapper-manager style POST /login and /logout.
    #[arg(long)]
    no_legacy_management: bool,
    /// Concurrent requests forwarded to a single lite instance.
    #[arg(long, default_value_t = 2)]
    instance_concurrency: usize,
    /// Seconds a lite instance may take to become ready.
    #[arg(long, default_value_t = 240)]
    ready_timeout: u64,
    /// lite instances allowed to start at the same time.
    #[arg(long, default_value_t = 2)]
    startup_parallelism: usize,
    /// Agent log level (trace, debug, info, warn, error).
    #[arg(long, default_value = "info", env = "WRAPPER_LOG")]
    log_level: String,
}

#[derive(Args)]
struct SandboxArgs {
    #[arg(long)]
    rootfs: PathBuf,
    #[arg(long)]
    privileged: bool,
    /// Program (path inside the rootfs) and its arguments.
    #[arg(last = true, required = true)]
    command: Vec<OsString>,
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.cmd {
        Cmd::SandboxExec(args) => sandbox_exec(args),
        Cmd::Agent(args) => run_async(run_agent(*args)),
    };
    std::process::exit(code);
}

fn run_async(fut: impl std::future::Future<Output = anyhow::Result<()>>) -> i32 {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    match rt.block_on(fut) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    }
}

#[cfg(target_os = "linux")]
fn sandbox_exec(args: SandboxArgs) -> i32 {
    let mut command = args.command.into_iter();
    let spec = wrapper_agent::sandbox::SandboxSpec {
        rootfs: std::fs::canonicalize(&args.rootfs).unwrap_or(args.rootfs),
        privileged: args.privileged,
        program: command.next().expect("clap requires a command"),
        args: command.collect(),
    };
    match wrapper_agent::sandbox::run(&spec) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("sandbox-exec: {e}");
            126
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn sandbox_exec(_args: SandboxArgs) -> i32 {
    eprintln!("sandbox-exec is only available on Linux");
    126
}

fn init_logging(level: &str) -> std::sync::Arc<EventBus> {
    let bus = EventBus::new(3000);
    let filter = || EnvFilter::try_new(format!("{level},hyper=warn,reqwest=warn")).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
                .with_writer(std::io::stderr)
                .with_filter(filter()),
        )
        .with(BusLayer::new(bus.clone()).with_filter(filter()))
        .init();
    bus
}

fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn resolve_runtime(args: &AgentArgs) -> anyhow::Result<(LiteRuntime, Layout)> {
    if !cfg!(target_os = "linux") {
        bail!("the agent runs lite and therefore needs Linux; use the QEMU launcher on this OS");
    }
    let kind = match args.runtime {
        RuntimeKind::Auto if args.lite_bin.exists() => RuntimeKind::Direct,
        RuntimeKind::Auto => RuntimeKind::Sandbox,
        k => k,
    };
    Ok(match kind {
        RuntimeKind::Direct => {
            if !args.lite_bin.exists() {
                bail!("lite binary {} not found", args.lite_bin.display());
            }
            let state = args.state_dir.clone();
            let lite_root = state.to_string_lossy().into_owned();
            (LiteRuntime::Direct { lite_bin: args.lite_bin.clone() }, Layout { state_dir: state, lite_root })
        }
        _ => {
            let rootfs = match &args.rootfs {
                Some(r) => r.clone(),
                None => [exe_dir().join("rootfs"), PathBuf::from("rootfs")]
                    .into_iter()
                    .find(|p| p.join("system/bin/lite").exists())
                    .context("rootfs not found next to the binary; pass --rootfs")?,
            };
            let rootfs = std::fs::canonicalize(&rootfs).with_context(|| format!("rootfs {}", rootfs.display()))?;
            let helper = std::env::current_exe().context("cannot locate own executable")?;
            let layout = Layout { state_dir: rootfs.join("data"), lite_root: "/data".into() };
            let privileged = kind == RuntimeKind::SandboxPrivileged;
            (LiteRuntime::Sandbox { helper, rootfs, privileged }, layout)
        }
    })
}

async fn run_agent(args: AgentArgs) -> anyhow::Result<()> {
    let bus = init_logging(&args.log_level);
    let (runtime, layout) = resolve_runtime(&args)?;
    let listen: SocketAddr = format!("{}:{}", args.host, args.port)
        .parse()
        .with_context(|| format!("invalid listen address {}:{}", args.host, args.port))?;

    let mut cfg = AgentConfig::new(listen, layout, runtime);
    cfg.lite = LiteOptions {
        proxy: args.proxy.clone().filter(|p| !p.is_empty()),
        device_info: args.device_info.clone().filter(|d| !d.is_empty()),
        log_level: args.lite_log_level.clone(),
        token_refresh_interval: args.token_refresh_interval,
        code_timeout: args.code_timeout,
    };
    cfg.api_token = args.api_token.clone().filter(|t| !t.is_empty());
    cfg.legacy_management = !args.no_legacy_management;
    cfg.instance_concurrency = args.instance_concurrency;
    cfg.ready_timeout = Duration::from_secs(args.ready_timeout);
    cfg.startup_parallelism = args.startup_parallelism;

    let listener = tokio::net::TcpListener::bind(listen).await.with_context(|| format!("cannot listen on {listen}"))?;
    let agent = Agent::new(cfg, bus)?;
    tracing::info!("wrapper agent {} listening on http://{listen}", env!("CARGO_PKG_VERSION"));
    agent.boot().await;

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(wrapper_agent::server::serve(agent.clone(), listener, async {
        let _ = stop_rx.await;
    }));
    shutdown_signal().await;
    tracing::info!("shutting down");
    agent.shutdown().await;
    let _ = stop_tx.send(());
    // Open SSE streams never finish on their own; do not wait for them forever.
    let _ = tokio::time::timeout(Duration::from_secs(3), server).await;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}
