//! `wrapper gui`: serves the web panel, controls the engine (QEMU guest or
//! native agent) and proxies the agent's management API with its token.
//!
//! * `/api/host/*`  — GUI backend (config, engine, console, package download)
//! * `/api/agent/*` — forwarded to the running agent's `/api/*`
//! * everything else — the embedded single-page app

pub mod config;
pub mod engine;
pub mod package;

use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, RawQuery, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use config::{config_path, native_supported, GuiConfig};
use engine::{Engine, EngineState, HostEvent, Paths};

mod assets {
    include!(concat!(env!("OUT_DIR"), "/web_assets.rs"));
}

const COOKIE: &str = "wrapper_gui";

#[derive(Clone, Debug)]
pub struct GuiOptions {
    pub listen: SocketAddr,
    pub paths: Paths,
    pub open_browser: bool,
    /// Required to use the panel (cookie / bearer). Needed when listening on
    /// a non-loopback address.
    pub access_token: Option<String>,
}

pub struct Gui {
    opts: GuiOptions,
    config: Mutex<GuiConfig>,
    pub engine: Arc<Engine>,
    proxy_http: reqwest::Client,
    downloading: AtomicBool,
}

type AppState = Arc<Gui>;

fn reply(status: StatusCode, code: i64, msg: &str, data: Value) -> Response {
    (status, Json(json!({ "code": code, "msg": msg, "data": data }))).into_response()
}

fn ok(data: impl serde::Serialize) -> Response {
    reply(StatusCode::OK, 0, "SUCCESS", serde_json::to_value(data).unwrap_or(Value::Null))
}

fn fail(status: StatusCode, msg: &str) -> Response {
    reply(status, -1, msg, Value::Null)
}

impl Gui {
    pub fn new(opts: GuiOptions) -> Arc<Self> {
        let config = GuiConfig::load(&config_path(&opts.paths.data_dir));
        Arc::new(Self {
            opts,
            config: Mutex::new(config),
            engine: Engine::new(),
            proxy_http: reqwest::Client::builder()
                .no_proxy()
                .connect_timeout(Duration::from_secs(5))
                .build()
                .expect("http client"),
            downloading: AtomicBool::new(false),
        })
    }

    pub fn config(&self) -> GuiConfig {
        self.config.lock().unwrap().clone()
    }

    pub async fn start_engine(&self) -> Result<(), String> {
        self.engine.start(self.config(), self.opts.paths.clone()).await
    }

    pub fn router(self: &Arc<Self>) -> Router {
        Router::new()
            .route("/api/host/info", get(info))
            .route("/api/host/config", get(get_config).put(put_config))
            .route("/api/host/status", get(status))
            .route("/api/host/start", post(start))
            .route("/api/host/stop", post(stop))
            .route("/api/host/restart", post(restart))
            .route("/api/host/console", get(console))
            .route("/api/host/events", get(events))
            .route("/api/host/package/download", post(download))
            .route("/api/agent/{*path}", any(agent_proxy))
            .route("/api/public/{*path}", any(public_proxy))
            .fallback(get(static_file))
            .layer(middleware::from_fn_with_state(self.clone(), access))
            .with_state(self.clone())
    }
}

// --------------------------------------------------------------- access

fn token_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn access(State(g): State<AppState>, req: Request, next: Next) -> Response {
    let Some(expected) = g.opts.access_token.as_deref() else {
        return next.run(req).await;
    };
    let headers = req.headers();
    let from_cookie = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().strip_prefix(&format!("{COOKIE}=")).map(str::to_string))
        .any(|t| token_eq(&t, expected));
    let from_bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| token_eq(t, expected));
    if from_cookie || from_bearer {
        return next.run(req).await;
    }
    // `/?token=...` (the URL printed at start-up) logs the browser in.
    let query_token = req.uri().query().and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("token=")));
    if query_token.is_some_and(|t| token_eq(t, expected)) {
        let mut res = Redirect::to(req.uri().path()).into_response();
        let cookie = format!("{COOKIE}={expected}; Path=/; HttpOnly; SameSite=Strict; Max-Age=31536000");
        if let Ok(v) = HeaderValue::from_str(&cookie) {
            res.headers_mut().insert(header::SET_COOKIE, v);
        }
        return res;
    }
    if req.uri().path().starts_with("/api/") {
        return fail(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    (
        StatusCode::UNAUTHORIZED,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        "<h3>wrapper</h3><p>Open the panel with the URL (including <code>?token=</code>) printed when wrapper started.</p>",
    )
        .into_response()
}

// ----------------------------------------------------------- host API

/// Primary LAN address (the one used for the default route).
fn lan_address() -> Option<String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:80").ok()?;
    Some(s.local_addr().ok()?.ip().to_string())
}

async fn info(State(g): State<AppState>) -> Response {
    let cfg = g.config();
    let qcfg = engine::qemu_config(&cfg, &g.opts.paths);
    ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "nativeSupported": native_supported(),
        "rootfsReady": g.opts.paths.rootfs.join("system/bin/lite").is_file(),
        "qemu": crate::qemu::check_assets(&qcfg),
        "packageName": package::package_name(),
        "packageUrl": package::package_url(&cfg.package_base_url, cfg.mirror),
        "downloading": g.downloading.load(Ordering::Relaxed),
        "dataDir": g.opts.paths.data_dir,
        "assetsDir": g.opts.paths.assets_dir,
        "lanAddress": lan_address(),
        "accessProtected": g.opts.access_token.is_some(),
    }))
}

async fn get_config(State(g): State<AppState>) -> Response {
    ok(g.config())
}

/// Merges the given fields into the current settings (partial updates keep
/// everything else, including the token; an empty token regenerates it).
async fn put_config(State(g): State<AppState>, body: Result<Json<Value>, axum::extract::rejection::JsonRejection>) -> Response {
    let Json(patch) = match body {
        Ok(b) => b,
        Err(e) => return fail(StatusCode::BAD_REQUEST, &e.body_text()),
    };
    let Value::Object(patch) = patch else {
        return fail(StatusCode::BAD_REQUEST, "expected a JSON object");
    };
    let mut merged = serde_json::to_value(g.config()).unwrap_or_default();
    if let Value::Object(m) = &mut merged {
        m.extend(patch);
    }
    let mut new: GuiConfig = match serde_json::from_value(merged) {
        Ok(c) => c,
        Err(e) => return fail(StatusCode::BAD_REQUEST, &format!("invalid settings: {e}")),
    };
    if new.api_token.trim().is_empty() {
        new.api_token = config::random_token();
    }
    if let Err(e) = new.validate() {
        return fail(StatusCode::BAD_REQUEST, &e);
    }
    if let Err(e) = new.save(&config_path(&g.opts.paths.data_dir)) {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, &format!("cannot save settings: {e}"));
    }
    let changed = {
        let mut cur = g.config.lock().unwrap();
        let changed = *cur != new;
        *cur = new.clone();
        changed
    };
    let running = matches!(g.engine.status().state, EngineState::Running | EngineState::Starting);
    ok(json!({ "config": new, "restartRequired": changed && running }))
}

async fn status(State(g): State<AppState>) -> Response {
    ok(g.engine.status())
}

async fn start(State(g): State<AppState>) -> Response {
    match g.start_engine().await {
        Ok(()) => ok(g.engine.status()),
        Err(e) => fail(StatusCode::BAD_REQUEST, &e),
    }
}

async fn stop(State(g): State<AppState>) -> Response {
    g.engine.stop().await;
    ok(g.engine.status())
}

async fn restart(State(g): State<AppState>) -> Response {
    g.engine.stop().await;
    match g.start_engine().await {
        Ok(()) => ok(g.engine.status()),
        Err(e) => fail(StatusCode::BAD_REQUEST, &e),
    }
}

#[derive(Deserialize)]
struct ConsoleQuery {
    after: Option<u64>,
    limit: Option<usize>,
}

async fn console(State(g): State<AppState>, Query(q): Query<ConsoleQuery>) -> Response {
    ok(g.engine.console(q.after, q.limit.unwrap_or(1000).min(4000)))
}

async fn events(State(g): State<AppState>) -> impl IntoResponse {
    let stream = BroadcastStream::new(g.engine.subscribe()).filter_map(|ev| {
        let ev = ev.ok()?;
        Some(Ok::<_, Infallible>(SseEvent::default().json_data(ev).ok()?))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn download(State(g): State<AppState>) -> Response {
    let cfg = g.config();
    let Some(url) = package::package_url(&cfg.package_base_url, cfg.mirror) else {
        return fail(StatusCode::BAD_REQUEST, "no prebuilt package for this platform");
    };
    if matches!(g.engine.status().state, EngineState::Running | EngineState::Starting) {
        return fail(StatusCode::CONFLICT, "stop the engine before updating the package");
    }
    if g.downloading.swap(true, Ordering::SeqCst) {
        return fail(StatusCode::CONFLICT, "a download is already running");
    }
    let dest = crate::exe_dir();
    let gui = g.clone();
    let task_url = url.clone();
    tokio::spawn(async move {
        let url = task_url;
        let engine = gui.engine.clone();
        engine.console_line(&format!("[gui] downloading {url}"));
        let ev = engine.clone();
        let proxy = Some(cfg.proxy.clone()).filter(|p| !p.is_empty());
        let result = package::download_and_install(&url, &dest, proxy.as_deref(), move |p| {
            ev.emit(HostEvent::Download { progress: p });
        })
        .await;
        if let Err(e) = result {
            let msg = format!("{e:#}");
            engine.console_line(&format!("[gui] download failed: {msg}"));
            engine.emit(HostEvent::Download {
                progress: package::DownloadProgress {
                    status: "error",
                    downloaded: 0,
                    total: None,
                    speed: 0,
                    message: Some(msg),
                },
            });
        } else {
            engine.console_line("[gui] package installed");
        }
        gui.downloading.store(false, Ordering::SeqCst);
    });
    ok(json!({ "url": url }))
}

// ----------------------------------------------------------- agent proxy

/// `/api/agent/*` -> the agent's management API, with its token.
async fn agent_proxy(
    State(g): State<AppState>,
    Path(path): Path<String>,
    method: Method,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    forward(&g, &format!("api/{path}"), true, method, query, headers, body).await
}

/// `/api/public/*` -> the public resource API (used by the API tester, so it
/// also works when the panel is opened from another machine).
async fn public_proxy(
    State(g): State<AppState>,
    Path(path): Path<String>,
    method: Method,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if path.starts_with("api/") || path == "login" || path == "logout" {
        return fail(StatusCode::FORBIDDEN, "use /api/agent for management endpoints");
    }
    forward(&g, &path, false, method, query, headers, body).await
}

async fn forward(
    g: &Gui,
    path: &str,
    with_token: bool,
    method: Method,
    query: Option<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some((base, token)) = g.engine.current_api() else {
        return fail(StatusCode::SERVICE_UNAVAILABLE, "the engine is not running");
    };
    let mut url = format!("{base}/{path}");
    if let Some(q) = query.filter(|q| !q.is_empty()) {
        url.push('?');
        url.push_str(&q);
    }
    let Ok(m) = reqwest::Method::from_bytes(method.as_str().as_bytes()) else {
        return fail(StatusCode::METHOD_NOT_ALLOWED, "bad method");
    };
    let mut req = g.proxy_http.request(m, url);
    if with_token {
        req = req.bearer_auth(token);
    }
    if !body.is_empty() {
        let ct = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("application/json");
        req = req.header("Content-Type", ct).body(body);
    }
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            let msg = if e.is_connect() { "the agent is not reachable yet".to_string() } else { e.to_string() };
            return fail(StatusCode::BAD_GATEWAY, &msg);
        }
    };
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let ct = resp.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| HeaderValue::from_bytes(v.as_bytes()).ok());
    let mut out = Response::new(Body::from_stream(resp.bytes_stream()));
    *out.status_mut() = status;
    if let Some(ct) = ct {
        out.headers_mut().insert(header::CONTENT_TYPE, ct);
    }
    out.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    out
}

// ----------------------------------------------------------- static files

fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "json" | "webmanifest" => "application/json",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

async fn static_file(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let (name, bytes) = match assets::ASSETS.iter().find(|(p, _)| *p == path) {
        Some((p, b)) => (*p, *b),
        // Single-page app: unknown paths render the shell.
        None if !path.starts_with("api/") && !path.starts_with("assets/") => match assets::ASSETS.iter().find(|(p, _)| *p == "index.html") {
            Some((p, b)) => (*p, *b),
            None => return fail(StatusCode::NOT_FOUND, "not found"),
        },
        None => return fail(StatusCode::NOT_FOUND, "not found"),
    };
    // Hashed bundle files can be cached forever; the shell must revalidate.
    let cache = if name.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
    ([(header::CONTENT_TYPE, mime(name)), (header::CACHE_CONTROL, cache)], bytes).into_response()
}

// ----------------------------------------------------------------- run

fn open_browser(url: &str) {
    let mut cmd = if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/c", "start", "", url]);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    } else {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };
    let _ = cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
}

/// Runs the GUI until `shutdown` resolves, then stops the engine.
pub async fn run(opts: GuiOptions, shutdown: impl std::future::Future<Output = ()> + Send + 'static) -> anyhow::Result<()> {
    let listener = match tokio::net::TcpListener::bind(opts.listen).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("cannot listen on {} ({e}); using a free port", opts.listen);
            tokio::net::TcpListener::bind(SocketAddr::new(opts.listen.ip(), 0)).await?
        }
    };
    let addr = listener.local_addr()?;
    let gui = Gui::new(opts.clone());
    let host = if addr.ip().is_unspecified() { "127.0.0.1".to_string() } else { addr.ip().to_string() };
    let mut url = format!("http://{host}:{}/", addr.port());
    if let Some(t) = &opts.access_token {
        url.push_str(&format!("?token={t}"));
    }
    println!("wrapper {} panel: {url}", env!("CARGO_PKG_VERSION"));
    if opts.open_browser {
        open_browser(&url);
    }
    if gui.config().autostart {
        if let Err(e) = gui.start_engine().await {
            tracing::warn!("autostart failed: {e}");
        }
    }
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let router = gui.router();
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = stop_rx.await;
            })
            .await
    });
    shutdown.await;
    println!("stopping...");
    gui.engine.stop().await;
    let _ = stop_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(3), server).await;
    Ok(())
}

/// Default data directory: next to the executable when writable.
pub fn default_data_dir() -> PathBuf {
    crate::exe_dir()
}
