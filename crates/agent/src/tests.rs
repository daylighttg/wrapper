//! HTTP-level tests against fake lite upstreams (no real lite needed).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::Router;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::accounts::{account_id, AccountRecord, Layout};
use crate::agent::{Agent, AgentConfig};
use crate::api::InstanceState;
use crate::events::EventBus;
use crate::health::Verdict;
use crate::lite::LiteRuntime;

/// Starts a fake lite that answers `/m3u8` with `code` and counts hits.
async fn fake_lite(code: i64, hits: Arc<AtomicUsize>) -> u16 {
    let app = Router::new()
        .route(
            "/m3u8",
            get(move |q: axum::extract::RawQuery| {
                let hits = hits.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    axum::Json(json!({"code": code, "msg": "x", "data": {"query": q.0}}))
                }
            }),
        )
        .route("/status", get(|| async { axum::Json(json!({"code":0,"msg":"SUCCESS","data":{"regions":["jp"]}})) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    port
}

fn test_agent(name: &str, token: Option<&str>) -> Arc<Agent> {
    test_agent_with(name, token, |_| {})
}

fn test_agent_with(name: &str, token: Option<&str>, tweak: impl FnOnce(&mut AgentConfig)) -> Arc<Agent> {
    let dir = std::env::temp_dir().join(format!("wrapper-agent-http-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let layout = Layout { state_dir: dir, lite_root: "/data".into() };
    let listen: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = AgentConfig::new(listen, layout, LiteRuntime::Direct { lite_bin: "/nonexistent".into() });
    cfg.api_token = token.map(str::to_string);
    tweak(&mut cfg);
    Agent::new(cfg, EventBus::new(100)).unwrap()
}

/// A fake lite whose `/key` either delivers a key or fails like lite does
/// when the FairPlay context is broken.
async fn fake_key_lite(works: bool, hits: Arc<AtomicUsize>) -> u16 {
    let app = Router::new().route(
        "/key",
        get(move || {
            let hits = hits.clone();
            async move {
                hits.fetch_add(1, Ordering::SeqCst);
                if works {
                    axum::Json(json!({"code":0,"msg":"SUCCESS","data":{"contentKey":"k"}}))
                } else {
                    axum::Json(json!({"code":500,"msg":"key retrieval failed","data":null}))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    port
}

/// Points the agent's "can we reach Apple" check at a local server (up) or a
/// closed port (down).
async fn set_apple_reachable(agent: &Agent, up: bool) {
    let port = if up {
        fake_lite(0, Arc::new(AtomicUsize::new(0))).await
    } else {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    agent.apple.set_reach_url(&format!("http://127.0.0.1:{port}/"));
}

fn add_running(agent: &Agent, user: &str, region: &str, port: u16) -> String {
    let rec = AccountRecord {
        id: account_id(user),
        username: user.into(),
        region: region.into(),
        enabled: true,
        device_info: None,
        created_at: 0,
    };
    let (inst, _) = agent.insert_instance(rec);
    inst.set_running_for_test(port);
    inst.id.clone()
}

async fn call(agent: &Arc<Agent>, req: Request<Body>) -> (StatusCode, Value) {
    let res = crate::server::router(agent.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn get_req(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

#[tokio::test]
async fn forwards_and_passes_envelope_through() {
    let agent = test_agent("forward", None);
    let hits = Arc::new(AtomicUsize::new(0));
    let port = fake_lite(0, hits.clone()).await;
    add_running(&agent, "a@x.y", "jp", port);

    let (status, body) = call(&agent, get_req("/m3u8?adamId=123&extra=%20a")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["code"], 0);
    // The raw query string is forwarded untouched.
    assert_eq!(body["data"]["query"], "adamId=123&extra=%20a");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn retries_on_instance_failure_but_not_on_bad_request() {
    let agent = test_agent("retry", None);
    let bad_hits = Arc::new(AtomicUsize::new(0));
    let good_hits = Arc::new(AtomicUsize::new(0));
    let bad = fake_lite(500, bad_hits.clone()).await;
    let good = fake_lite(0, good_hits.clone()).await;
    add_running(&agent, "bad@x.y", "jp", bad);
    add_running(&agent, "good@x.y", "jp", good);

    for _ in 0..6 {
        let (_, body) = call(&agent, get_req("/m3u8?adamId=1")).await;
        assert_eq!(body["code"], 0, "a 500 from one instance must fall through to the other");
    }
    assert_eq!(good_hits.load(Ordering::SeqCst), 6);

    let agent = test_agent("noretry", None);
    let h1 = Arc::new(AtomicUsize::new(0));
    let h2 = Arc::new(AtomicUsize::new(0));
    add_running(&agent, "a@x.y", "jp", fake_lite(400, h1.clone()).await);
    add_running(&agent, "b@x.y", "jp", fake_lite(400, h2.clone()).await);
    let (_, body) = call(&agent, get_req("/m3u8?adamId=1")).await;
    assert_eq!(body["code"], 400);
    assert_eq!(h1.load(Ordering::SeqCst) + h2.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn validates_requests_without_touching_instances() {
    let agent = test_agent("validate", None);
    let (_, body) = call(&agent, get_req("/m3u8")).await;
    assert_eq!(body["msg"], "missing adamId");
    let (_, body) = call(&agent, get_req("/key?adamId=1")).await;
    assert_eq!(body["msg"], "missing uri");
    let (_, body) = call(&agent, get_req("/key?adamId=1&uri=skd://itunes.apple.com/P000000000/s1/e1")).await;
    assert_eq!(body["msg"], "invalid uri for adamId");
    let (_, body) = call(&agent, get_req("/m3u8?adamId=1")).await;
    assert_eq!(body["msg"], "no available instance");
    // wrapper-manager's key_request_test.go: the rest of the /key contract.
    let (_, body) = call(&agent, get_req("/key")).await;
    assert_eq!(body["msg"], "missing adamId");
    let (_, body) = call(&agent, get_req("/key?uri=skd%3A%2F%2Fx")).await;
    assert_eq!(body["msg"], "missing adamId");
    // The prefetch URI is fine with adamId=0 (validation passes, nobody serves), as is any other uri.
    let prefetch = "/key?adamId=0&uri=skd%3A%2F%2Fitunes.apple.com%2FP000000000%2Fs1%2Fe1";
    let (_, body) = call(&agent, get_req(prefetch)).await;
    assert_eq!(body["msg"], "no available instance");
    let (_, body) = call(&agent, get_req("/key?adamId=0&uri=skd%3A%2F%2Fother")).await;
    assert_eq!(body["msg"], "no available instance");
    let req = Request::post("/license").body(Body::from("{}")).unwrap();
    let (_, body) = call(&agent, req).await;
    assert_eq!(body["msg"], "missing adamId, challenge, or uri");
}

#[tokio::test]
async fn aggregates_status() {
    let agent = test_agent("status", None);
    let hits = Arc::new(AtomicUsize::new(0));
    add_running(&agent, "a@x.y", "jp", fake_lite(0, hits.clone()).await);
    add_running(&agent, "b@x.y", "us", fake_lite(0, hits.clone()).await);
    add_running(&agent, "c@x.y", "jp", fake_lite(0, hits).await);
    let (_, body) = call(&agent, get_req("/status")).await;
    assert_eq!(body["data"]["clientCount"], 3);
    assert_eq!(body["data"]["status"], true);
    let mut regions: Vec<String> = serde_json::from_value(body["data"]["regions"].clone()).unwrap();
    regions.sort();
    assert_eq!(regions, ["jp", "us"]);
}

#[tokio::test]
async fn management_requires_token_when_configured() {
    let agent = test_agent("auth", Some("s3cret"));
    let (status, _) = call(&agent, get_req("/api/accounts")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let req = Request::get("/api/accounts").header("Authorization", "Bearer s3cret").body(Body::empty()).unwrap();
    let (status, body) = call(&agent, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["code"], 0);
    let (status, _) = call(&agent, get_req("/api/logs?token=s3cret")).await;
    assert_eq!(status, StatusCode::OK);
    let req = Request::post("/logout").body(Body::from(r#"{"username":"x"}"#)).unwrap();
    let (status, _) = call(&agent, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // The resource API stays open.
    let (status, _) = call(&agent, get_req("/status")).await;
    assert_eq!(status, StatusCode::OK);
}

#[test]
fn logout_resolves_username_or_uuid() {
    use crate::api::LogoutRequest;
    let by_name = |u: &str| LogoutRequest { username: u.into(), ..Default::default() };
    let by_id = |i: &str| LogoutRequest { id: i.into(), ..Default::default() };

    assert_eq!(by_name("acct@example.com").resolve_id().unwrap(), account_id("acct@example.com"));
    // A raw UUID is used as-is (normalized to lower case).
    let raw = uuid::Uuid::new_v4().to_string();
    assert_eq!(by_id(&raw).resolve_id().unwrap(), raw);
    assert_eq!(by_id(&raw.to_uppercase()).resolve_id().unwrap(), raw);
    // id wins when both are given.
    let both = LogoutRequest { username: "acct@example.com".into(), id: raw.clone() };
    assert_eq!(both.resolve_id().unwrap(), raw);
    assert!(by_id("not-a-uuid").resolve_id().is_err());
    assert!(LogoutRequest::default().resolve_id().is_err());
}

#[tokio::test]
async fn logout_by_id_removes_the_account() {
    let agent = test_agent("logout-id", None);
    let id = add_running(&agent, "a@x.y", "jp", 1);
    let post = |body: String| {
        Request::post("/logout").header("Content-Type", "application/json").body(Body::from(body)).unwrap()
    };
    let (_, body) = call(&agent, post(r#"{"id":"not-a-uuid"}"#.into())).await;
    assert_eq!(body["msg"], "invalid id (expected a UUID)");
    let (_, body) = call(&agent, post("{}".into())).await;
    assert_eq!(body["msg"], "missing username or id");
    let (_, body) = call(&agent, post(format!(r#"{{"id":"{id}"}}"#))).await;
    assert_eq!(body["code"], 0);
    assert_eq!(body["data"]["id"], id);
    assert!(agent.instance(&id).is_none());
    let (_, body) = call(&agent, post(format!(r#"{{"id":"{id}"}}"#))).await;
    assert_eq!(body["msg"], "no such account");
}

#[tokio::test]
async fn legacy_endpoints_validate_input() {
    let agent = test_agent("legacy", None);
    let req = Request::post("/logout")
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"username":"ghost@x.y"}"#))
        .unwrap();
    let (_, body) = call(&agent, req).await;
    assert_eq!(body["msg"], "no such account");

    let req = Request::post("/login")
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"username":"a@x.y","password":"p","code":"123456"}"#))
        .unwrap();
    let (_, body) = call(&agent, req).await;
    assert_eq!(body["code"], -1);
    assert!(body["msg"].as_str().unwrap().contains("no pending login"));

    let port = fake_lite(0, Arc::new(AtomicUsize::new(0))).await;
    add_running(&agent, "a@x.y", "jp", port);
    let req = Request::post("/login")
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"username":"A@x.y","password":"p"}"#))
        .unwrap();
    let (_, body) = call(&agent, req).await;
    assert_eq!(body["msg"], "already login");
}

#[tokio::test]
async fn degraded_instances_are_skipped() {
    let agent = test_agent("degraded", None);
    let h1 = Arc::new(AtomicUsize::new(0));
    let h2 = Arc::new(AtomicUsize::new(0));
    let id1 = add_running(&agent, "a@x.y", "jp", fake_lite(0, h1.clone()).await);
    add_running(&agent, "b@x.y", "jp", fake_lite(0, h2.clone()).await);
    agent.instance(&id1).unwrap().open_circuit_for_test();
    for _ in 0..4 {
        call(&agent, get_req("/m3u8?adamId=1")).await;
    }
    assert_eq!(h1.load(Ordering::SeqCst), 0);
    assert_eq!(h2.load(Ordering::SeqCst), 4);
    assert_eq!(agent.instance(&id1).unwrap().state(), InstanceState::Running);
}

// ------------------------------------------------------------------ health

#[tokio::test]
async fn probe_tells_broken_lite_from_network_trouble() {
    let agent = test_agent("probe", None);
    let hits = Arc::new(AtomicUsize::new(0));
    let good = add_running(&agent, "good@x.y", "jp", fake_key_lite(true, hits.clone()).await);
    let bad = add_running(&agent, "bad@x.y", "jp", fake_key_lite(false, hits.clone()).await);
    let good = agent.instance(&good).unwrap();
    let bad = agent.instance(&bad).unwrap();

    set_apple_reachable(&agent, true).await;
    assert_eq!(agent.probe_instance(&good).await.verdict, Verdict::Ok);
    let r = agent.probe_instance(&bad).await;
    assert_eq!(r.verdict, Verdict::Broken, "Apple reachable and no key: lite is the problem");
    assert!(r.detail.contains("key retrieval failed"), "{}", r.detail);
    assert_eq!(bad.health_view().consecutive_failures, 1);

    set_apple_reachable(&agent, false).await;
    let r = agent.probe_instance(&bad).await;
    assert_eq!(r.verdict, Verdict::NetworkDown, "Apple unreachable: restarting would not help");
    assert_eq!(bad.health_view().consecutive_failures, 1, "network trouble must not count against lite");

    // The probe asks for the prefetch key, never for a real song.
    assert_eq!(hits.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn health_endpoint_reports_and_gates_live_probes() {
    let agent = test_agent("health-ep", Some("s3cret"));
    // Nothing configured: nobody can serve.
    let (status, body) = call(&agent, get_req("/health")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["data"]["healthy"], false);

    let hits = Arc::new(AtomicUsize::new(0));
    let good = add_running(&agent, "good@x.y", "jp", fake_key_lite(true, hits.clone()).await);
    let bad = add_running(&agent, "bad@x.y", "us", fake_key_lite(false, hits.clone()).await);
    set_apple_reachable(&agent, true).await;

    // Cached view: open (like /status), and no Apple traffic.
    let (status, body) = call(&agent, get_req("/health")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["status"], "ok");
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    // Without the token it does not name the accounts.
    assert!(body["data"].get("accounts").is_none(), "{body}");
    assert!(!body.to_string().contains("good@x.y"));

    // A live probe needs the token...
    let (status, _) = call(&agent, get_req("/health?probe=1")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    // ...and then finds the broken account.
    let req = Request::get("/health?probe=1").header("Authorization", "Bearer s3cret").body(Body::empty()).unwrap();
    let (status, body) = call(&agent, req).await;
    assert_eq!(status, StatusCode::OK, "one healthy account is enough to serve");
    assert_eq!(body["data"]["status"], "degraded");
    let accounts = body["data"]["accounts"].as_array().unwrap();
    let find = |id: &str| accounts.iter().find(|a| a["id"] == id).unwrap();
    assert_eq!(find(&good)["healthy"], true);
    assert_eq!(find(&good)["health"]["verdict"], "ok");
    assert_eq!(find(&bad)["healthy"], false);
    assert_eq!(find(&bad)["health"]["verdict"], "broken");

    // Account views carry the same data for the panel.
    let (_, body) = call(&agent, get_req("/api/accounts?token=s3cret")).await;
    let view = body["data"].as_array().unwrap().iter().find(|a| a["id"] == bad).unwrap().clone();
    assert_eq!(view["health"]["verdict"], "broken");

    // The panel's way in: a management endpoint behind the token.
    let (status, _) = call(&agent, Request::post("/api/health/probe").body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let req = Request::post("/api/health/probe").header("Authorization", "Bearer s3cret").body(Body::empty()).unwrap();
    let (status, body) = call(&agent, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["code"], 0);
    assert_eq!(body["data"]["status"], "degraded");
    assert_eq!(body["data"]["accounts"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn monitor_restarts_only_a_confirmed_break() {
    let fast = |c: &mut AgentConfig| {
        c.probe_interval = Duration::from_millis(60);
        c.probe_confirm_gap = Duration::from_millis(30);
        c.probe_recheck = Duration::from_millis(60);
    };

    // Broken with Apple reachable: confirmed twice, then a restart is asked for.
    let agent = test_agent_with("mon-broken", None, fast);
    let id = add_running(&agent, "a@x.y", "jp", fake_key_lite(false, Arc::new(AtomicUsize::new(0))).await);
    set_apple_reachable(&agent, true).await;
    let inst = agent.instance(&id).unwrap();
    let why = tokio::time::timeout(Duration::from_secs(5), agent.clone().health_monitor(inst.clone()))
        .await
        .expect("a broken instance must be reported");
    assert!(why.contains("key probe failed 2x"), "{why}");

    // Same failure but Apple unreachable: a restart cannot fix it, so none is requested.
    let agent = test_agent_with("mon-net", None, fast);
    let id = add_running(&agent, "a@x.y", "jp", fake_key_lite(false, Arc::new(AtomicUsize::new(0))).await);
    set_apple_reachable(&agent, false).await;
    let inst = agent.instance(&id).unwrap();
    let monitor = tokio::spawn(agent.clone().health_monitor(inst.clone()));
    // (Refused connections can take ~2s to fail on Windows, hence the polling.)
    wait_until(|| inst.health_view().verdict == Some(Verdict::NetworkDown)).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!monitor.is_finished(), "no restart while the network is down");
    assert_eq!(inst.health_view().consecutive_failures, 0);
    monitor.abort();

    // A healthy probe closes a circuit that log lines opened.
    let agent = test_agent_with("mon-ok", None, fast);
    let id = add_running(&agent, "a@x.y", "jp", fake_key_lite(true, Arc::new(AtomicUsize::new(0))).await);
    let inst = agent.instance(&id).unwrap();
    inst.open_circuit_for_test();
    assert!(inst.circuit_open());
    let monitor = tokio::spawn(agent.clone().health_monitor(inst.clone()));
    wait_until(|| !inst.circuit_open()).await;
    assert_eq!(inst.health_view().verdict, Some(Verdict::Ok), "the probe proved the key path works");
    assert!(!monitor.is_finished());
    monitor.abort();
}

#[tokio::test]
async fn a_failure_found_by_health_probe_is_confirmed_after_the_gap() {
    let agent = test_agent_with("mon-probe-all", None, |c| {
        c.probe_interval = Duration::ZERO;
        c.probe_confirm_gap = Duration::from_millis(600);
    });
    let hits = Arc::new(AtomicUsize::new(0));
    let id = add_running(&agent, "a@x.y", "jp", fake_key_lite(false, hits.clone()).await);
    set_apple_reachable(&agent, true).await;
    let inst = agent.instance(&id).unwrap();
    let monitor = tokio::spawn(agent.clone().health_monitor(inst.clone()));

    // `/health?probe=1` finds the break and wakes the monitor...
    agent.probe_all().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    // ...which must not count an immediate second probe as the confirmation.
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    assert!(!monitor.is_finished());
    let why = tokio::time::timeout(Duration::from_secs(5), monitor).await.expect("confirmed later").unwrap();
    assert!(why.contains("2x"), "{why}");
    assert_eq!(hits.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_new_process_starts_with_a_clean_health_record() {
    let agent = test_agent("new-process", None);
    let id = add_running(&agent, "a@x.y", "jp", fake_key_lite(false, Arc::new(AtomicUsize::new(0))).await);
    set_apple_reachable(&agent, true).await;
    let inst = agent.instance(&id).unwrap();
    agent.probe_instance(&inst).await;
    agent.probe_instance(&inst).await;
    assert!(inst.take_restart_budget(3, Duration::from_secs(3600)));
    assert!(!inst.is_healthy());

    inst.health_new_process();
    let v = inst.health_view();
    assert_eq!((v.verdict, v.consecutive_failures), (None, 0), "the old process's results are gone");
    assert!(inst.is_healthy(), "a fresh process is not reported broken");
    assert_eq!(v.health_restarts, 1, "the restart budget survives restarts");
}

async fn wait_until(mut cond: impl FnMut() -> bool) {
    for _ in 0..200 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("condition not met within 10s");
}

#[tokio::test]
async fn suspicion_triggers_a_probe_even_when_periodic_probing_is_off() {
    let agent = test_agent_with("mon-suspect", None, |c| {
        c.probe_interval = Duration::ZERO;
        c.probe_confirm_gap = Duration::from_millis(10);
    });
    let hits = Arc::new(AtomicUsize::new(0));
    let id = add_running(&agent, "a@x.y", "jp", fake_key_lite(true, hits.clone()).await);
    let inst = agent.instance(&id).unwrap();
    let monitor = tokio::spawn(agent.clone().health_monitor(inst.clone()));

    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(hits.load(Ordering::SeqCst), 0, "interval 0 disables periodic probes");
    inst.suspect.notify_one();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    monitor.abort();
}

#[tokio::test]
async fn real_key_traffic_counts_as_health_evidence() {
    let agent = test_agent("key-evidence", None);
    let id = add_running(&agent, "a@x.y", "jp", fake_key_lite(true, Arc::new(AtomicUsize::new(0))).await);
    let inst = agent.instance(&id).unwrap();
    assert!(inst.health_view().last_key_ok_at.is_none());
    let (_, body) = call(&agent, get_req("/key?adamId=1&uri=skd://itunes.apple.com/P1/s1/e1")).await;
    assert_eq!(body["code"], 0);
    assert!(inst.health_view().last_key_ok_at.is_some());

    // ...and it clears a circuit that FairPlay log lines had opened.
    inst.open_circuit_for_test();
    assert!(inst.circuit_open());
    inst.note_key_ok();
    assert!(!inst.circuit_open());
}

#[tokio::test]
async fn restart_budget_is_rate_limited() {
    let agent = test_agent("budget", None);
    let id = add_running(&agent, "a@x.y", "jp", 1);
    let inst = agent.instance(&id).unwrap();
    let window = Duration::from_millis(200);
    assert!(inst.take_restart_budget(2, window));
    assert!(inst.take_restart_budget(2, window));
    assert!(!inst.take_restart_budget(2, window), "third restart inside the window is refused");
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(inst.take_restart_budget(2, window), "the window slides");
}
