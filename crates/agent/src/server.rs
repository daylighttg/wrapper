//! HTTP surface of the agent.
//!
//! * wrapper-lite compatible resource API (`/m3u8`, `/key`, `/lyrics`,
//!   `/webplayback`, `/license`) routed across accounts, and an aggregated
//!   `/status` in wrapper-manager's shape;
//! * wrapper-manager compatible `POST /login` / `POST /logout`;
//! * the management API under `/api` used by the GUI.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, Query, RawQuery, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use serde::Deserialize;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::accounts::{account_id, is_valid_id};
use crate::agent::Agent;
use crate::api::{
    CodeRequest, Envelope, InstanceState, LiteBody, LoginRequest, LoginState, LoginView, LogoutRequest, CODE_NEED_2FA,
};
use crate::health::PREFETCH_KEY_URI;
use crate::login::LoginError;

type AppState = Arc<Agent>;

pub fn router(agent: Arc<Agent>) -> Router {
    let public = Router::new()
        .route("/status", get(status))
        .route("/health", get(health))
        .route("/m3u8", get(m3u8))
        .route("/webplayback", get(webplayback))
        .route("/key", get(key))
        .route("/lyrics", get(lyrics))
        .route("/license", post(license));

    let mut mgmt = Router::new()
        .route("/api/overview", get(overview))
        .route("/api/accounts", get(list_accounts))
        .route("/api/accounts/login", post(api_login))
        .route("/api/accounts/{id}", axum::routing::delete(api_remove))
        .route("/api/accounts/{id}/start", post(api_start))
        .route("/api/accounts/{id}/stop", post(api_stop))
        .route("/api/accounts/{id}/restart", post(api_restart))
        .route("/api/logins", get(list_logins))
        .route("/api/logins/{id}", get(get_login).delete(cancel_login))
        .route("/api/logins/{id}/code", post(api_code))
        .route("/api/system/shutdown", post(system_shutdown))
        .route("/api/logs", get(logs))
        .route("/api/events", get(events));
    if agent.cfg.legacy_management {
        mgmt = mgmt.route("/login", post(legacy_login)).route("/logout", post(legacy_logout));
    }
    let mgmt = mgmt.layer(middleware::from_fn_with_state(agent.clone(), require_token));

    public
        .merge(mgmt)
        .fallback(|| async { Envelope::fail("not found").status(StatusCode::NOT_FOUND) })
        .layer(middleware::from_fn(cors))
        .with_state(agent)
}

/// Serves the router until `shutdown` resolves.
pub async fn serve(
    agent: Arc<Agent>,
    listener: tokio::net::TcpListener,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, router(agent)).with_graceful_shutdown(shutdown).await
}

// ---------------------------------------------------------------- middleware

async fn cors(req: Request, next: Next) -> Response {
    let mut res = if req.method() == Method::OPTIONS {
        StatusCode::NO_CONTENT.into_response()
    } else {
        next.run(req).await
    };
    let h = res.headers_mut();
    h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    h.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("GET, POST, DELETE, OPTIONS"));
    h.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("Authorization, Content-Type, *"));
    h.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("86400"));
    h.insert("access-control-allow-private-network", HeaderValue::from_static("true"));
    res
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Whether the request carries the management token (always true when none
/// is configured).
fn authorized(agent: &Agent, headers: &HeaderMap, query: Option<&str>) -> bool {
    let Some(expected) = agent.cfg.api_token.as_deref() else {
        return true;
    };
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    // EventSource cannot set headers, so SSE passes ?token=.
    let query = query.and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("token=")));
    [bearer, query].into_iter().flatten().any(|t| constant_time_eq(t.as_bytes(), expected.as_bytes()))
}

async fn require_token(State(agent): State<AppState>, req: Request, next: Next) -> Response {
    if !authorized(&agent, req.headers(), req.uri().query()) {
        return Envelope::fail("unauthorized").status(StatusCode::UNAUTHORIZED).into_response();
    }
    next.run(req).await
}

// ------------------------------------------------------------ resource API

fn param<'a>(q: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    q.get(name).map(|s| s.trim()).filter(|s| !s.is_empty())
}

fn is_cacheable(path: &str) -> bool {
    matches!(path, "/m3u8" | "/webplayback" | "/key" | "/lyrics")
}

async fn route(agent: &Agent, path: &str, adam_id: &str, method: Method, raw: Option<String>, body: Bytes) -> Response {
    let candidates = agent.candidates(adam_id).await;
    match agent.forward_with_retry(candidates, method, path, raw.as_deref(), body).await {
        Ok(f) => LiteBody { body: f.body, code: f.code, cacheable: is_cacheable(path) }.into_response(),
        Err(e) => Envelope::fail(e.to_string()).into_response(),
    }
}

async fn m3u8(State(a): State<AppState>, RawQuery(raw): RawQuery, Query(q): Query<HashMap<String, String>>) -> Response {
    match param(&q, "adamId") {
        Some(adam) => route(&a, "/m3u8", adam, Method::GET, raw, Bytes::new()).await,
        None => Envelope::fail("missing adamId").into_response(),
    }
}

async fn webplayback(
    State(a): State<AppState>,
    RawQuery(raw): RawQuery,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    match param(&q, "adamId") {
        Some(adam) => route(&a, "/webplayback", adam, Method::GET, raw, Bytes::new()).await,
        None => Envelope::fail("missing adamId").into_response(),
    }
}

async fn key(State(a): State<AppState>, RawQuery(raw): RawQuery, Query(q): Query<HashMap<String, String>>) -> Response {
    let Some(adam) = param(&q, "adamId") else {
        return Envelope::fail("missing adamId").into_response();
    };
    let Some(uri) = param(&q, "uri") else {
        return Envelope::fail("missing uri").into_response();
    };
    if adam != "0" && uri == PREFETCH_KEY_URI {
        return Envelope::fail("invalid uri for adamId").into_response();
    }
    route(&a, "/key", adam, Method::GET, raw, Bytes::new()).await
}

async fn lyrics(State(a): State<AppState>, RawQuery(raw): RawQuery, Query(q): Query<HashMap<String, String>>) -> Response {
    let Some(adam) = param(&q, "adamId") else {
        return Envelope::fail("missing adamId").into_response();
    };
    let language = param(&q, "language").unwrap_or("en");
    let script = param(&q, "script").unwrap_or("");
    if let Some(inst) = a.lyrics_candidate(adam, language, script).await {
        if let Ok(f) = a.forward(&inst, Method::GET, "/lyrics", raw.as_deref(), Bytes::new()).await {
            if f.code == crate::api::CODE_OK {
                return LiteBody { body: f.body, code: f.code, cacheable: true }.into_response();
            }
        }
    }
    route(&a, "/lyrics", adam, Method::GET, raw, Bytes::new()).await
}

async fn license(State(a): State<AppState>, body: Bytes) -> Response {
    #[derive(Deserialize)]
    struct Payload {
        #[serde(rename = "adamId", default)]
        adam_id: String,
        #[serde(default)]
        challenge: String,
        #[serde(default)]
        uri: String,
    }
    let Ok(p) = serde_json::from_slice::<Payload>(&body) else {
        return Envelope::fail("invalid JSON body").into_response();
    };
    if p.adam_id.is_empty() || p.challenge.is_empty() || p.uri.is_empty() {
        return Envelope::fail("missing adamId, challenge, or uri").into_response();
    }
    route(&a, "/license", &p.adam_id, Method::POST, None, body).await
}

async fn status(State(a): State<AppState>) -> Envelope {
    let all = a.instances();
    let running: Vec<_> = all.iter().filter(|i| i.state() == InstanceState::Running).collect();
    let mut regions: Vec<String> = Vec::new();
    for i in &running {
        let r = i.region();
        if !r.is_empty() && !regions.contains(&r) {
            regions.push(r);
        }
    }
    Envelope::ok(serde_json::json!({
        "status": !running.is_empty(),
        "regions": regions,
        "clientCount": running.len(),
        "ready": a.is_ready(),
        "degraded": running.iter().filter(|i| i.circuit_open()).count(),
        "accounts": all.len(),
    }))
}

/// `GET /health`: whether any account can really serve a key.
///
/// By default this reports what the health monitor last learned (no Apple
/// traffic, cheap enough for a Docker/uptime check). `?probe=1` first runs a
/// live `/key` request against every running account and answers after they
/// finish (up to ~45 s); it needs the management token when one is set,
/// because each probe occupies a lite request slot. HTTP 503 when no account
/// is healthy.
async fn health(State(a): State<AppState>, headers: HeaderMap, RawQuery(raw): RawQuery) -> Response {
    let wants_probe = raw
        .as_deref()
        .is_some_and(|q| q.split('&').any(|kv| matches!(kv, "probe=1" | "probe=true" | "deep=1" | "deep=true")));
    if wants_probe {
        if !authorized(&a, &headers, raw.as_deref()) {
            return Envelope::fail("unauthorized").status(StatusCode::UNAUTHORIZED).into_response();
        }
        a.probe_all().await;
    }
    let report = a.health_report();
    if report.healthy {
        Envelope::ok(report).into_response()
    } else {
        let msg = format!("unhealthy: {}", report.status);
        Envelope::with_code(crate::api::CODE_FAIL, msg, report).status(StatusCode::SERVICE_UNAVAILABLE).into_response()
    }
}

// ---------------------------------------------------------- management API

fn bad_request(msg: impl Into<String>) -> Envelope {
    Envelope::fail(msg).status(StatusCode::BAD_REQUEST)
}

fn not_found(what: &str) -> Envelope {
    Envelope::fail(format!("{what} not found")).status(StatusCode::NOT_FOUND)
}

fn login_error(e: LoginError) -> Envelope {
    match e {
        LoginError::Busy => Envelope::fail(e.to_string()).status(StatusCode::CONFLICT),
        LoginError::Io(_) => Envelope::fail(e.to_string()).status(StatusCode::INTERNAL_SERVER_ERROR),
        _ => bad_request(e.to_string()),
    }
}

async fn overview(State(a): State<AppState>) -> Envelope {
    Envelope::ok(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "ready": a.is_ready(),
        "runtime": a.cfg.runtime.describe(),
        "listen": a.cfg.listen.to_string(),
        "authRequired": a.cfg.api_token.is_some(),
        "accounts": a.views(),
        "logins": a.login_sessions(),
    }))
}

async fn list_accounts(State(a): State<AppState>) -> Envelope {
    Envelope::ok(a.views())
}

async fn api_login(State(a): State<AppState>, body: Result<Json<LoginRequest>, JsonRejection>) -> Envelope {
    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => return bad_request(e.body_text()),
    };
    match a.begin_login(req).await {
        Ok(v) => Envelope::ok(v),
        Err(e) => login_error(e),
    }
}

async fn api_code(
    State(a): State<AppState>,
    Path(id): Path<String>,
    body: Result<Json<CodeRequest>, JsonRejection>,
) -> Envelope {
    let Json(req) = match body {
        Ok(b) => b,
        Err(e) => return bad_request(e.body_text()),
    };
    match a.submit_code(&id, &req.code).await {
        Ok(v) => Envelope::ok(v),
        Err(e) => login_error(e),
    }
}

async fn list_logins(State(a): State<AppState>) -> Envelope {
    Envelope::ok(a.login_sessions())
}

async fn get_login(State(a): State<AppState>, Path(id): Path<String>) -> Envelope {
    match a.login_session(&id) {
        Some(v) => Envelope::ok(v),
        None => not_found("login"),
    }
}

async fn cancel_login(State(a): State<AppState>, Path(id): Path<String>) -> Envelope {
    if a.cancel_login(&id) {
        Envelope::ok(serde_json::Value::Null)
    } else {
        not_found("active login")
    }
}

async fn api_remove(State(a): State<AppState>, Path(id): Path<String>) -> Envelope {
    if !is_valid_id(&id) {
        return bad_request("invalid account id");
    }
    if a.remove_account(&id).await {
        Envelope::ok(serde_json::json!({ "id": id }))
    } else {
        not_found("account")
    }
}

async fn api_start(State(a): State<AppState>, Path(id): Path<String>) -> Envelope {
    if a.set_enabled(&id, true).await {
        Envelope::ok(a.instance(&id).map(|i| i.view()))
    } else {
        not_found("account")
    }
}

async fn api_stop(State(a): State<AppState>, Path(id): Path<String>) -> Envelope {
    if a.set_enabled(&id, false).await {
        Envelope::ok(a.instance(&id).map(|i| i.view()))
    } else {
        not_found("account")
    }
}

async fn api_restart(State(a): State<AppState>, Path(id): Path<String>) -> Envelope {
    if a.restart_account(&id).await {
        Envelope::ok(a.instance(&id).map(|i| i.view()))
    } else {
        not_found("account")
    }
}

async fn system_shutdown(State(a): State<AppState>) -> Envelope {
    tracing::info!("shutdown requested over the API");
    a.request_shutdown();
    Envelope::ok(serde_json::Value::Null)
}

#[derive(Deserialize)]
struct LogQuery {
    source: Option<String>,
    after: Option<u64>,
    limit: Option<usize>,
}

async fn logs(State(a): State<AppState>, Query(q): Query<LogQuery>) -> Envelope {
    let limit = q.limit.unwrap_or(500).min(5000);
    Envelope::ok(a.bus.recent_logs(q.source.as_deref().filter(|s| !s.is_empty()), q.after, limit))
}

async fn events(State(a): State<AppState>) -> impl IntoResponse {
    let stream = BroadcastStream::new(a.bus.subscribe()).filter_map(|ev| {
        // A lagging client just misses events; it can refetch /api/overview.
        let ev = ev.ok()?;
        Some(Ok::<_, Infallible>(SseEvent::default().json_data(ev).ok()?))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

// -------------------------------------------- wrapper-manager compatibility

fn legacy_login_envelope(v: LoginView) -> Envelope {
    let data = serde_json::json!({ "loginId": v.login_id });
    match v.state {
        LoginState::Success => Envelope::ok(data),
        LoginState::Need2fa => Envelope::with_code(CODE_NEED_2FA, "2fa code require", data),
        LoginState::Failed => Envelope::fail(v.message.unwrap_or_else(|| "login failed".into())),
        LoginState::Running => Envelope::fail("login still in progress"),
    }
}

async fn legacy_login(State(a): State<AppState>, body: Result<Json<LoginRequest>, JsonRejection>) -> Envelope {
    let Json(req) = match body {
        Ok(b) => b,
        Err(_) => return Envelope::fail("invalid JSON body"),
    };
    let id = account_id(&req.username);
    let result = match req.code.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        Some(code) => {
            let code = code.to_string();
            a.submit_code(&id, &code).await
        }
        None => {
            let pending = a.login_session(&id).is_some_and(|s| s.state == LoginState::Need2fa);
            if !pending && a.instance(&id).is_some_and(|i| i.state() == InstanceState::Running) {
                return Envelope::fail("already login");
            }
            a.begin_login(req).await
        }
    };
    match result {
        Ok(v) => legacy_login_envelope(v),
        Err(e) => Envelope::fail(e.to_string()),
    }
}

async fn legacy_logout(State(a): State<AppState>, body: Result<Json<LogoutRequest>, JsonRejection>) -> Envelope {
    let Ok(Json(req)) = body else {
        return Envelope::fail("invalid JSON body");
    };
    let id = match req.resolve_id() {
        Ok(id) => id,
        Err(e) => return Envelope::fail(e),
    };
    if !a.remove_account(&id).await {
        return Envelope::fail("no such account");
    }
    // Echo what the caller sent, like wrapper-manager.
    let mut data = serde_json::Map::new();
    if !req.id.is_empty() {
        data.insert("id".into(), req.id.into());
    }
    if !req.username.is_empty() {
        data.insert("username".into(), req.username.into());
    }
    Envelope::ok(data)
}
