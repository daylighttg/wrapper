//! HTTP-level tests against fake lite upstreams (no real lite needed).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

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
    let dir = std::env::temp_dir().join(format!("wrapper-agent-http-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let layout = Layout { state_dir: dir, lite_root: "/data".into() };
    let listen: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let mut cfg = AgentConfig::new(listen, layout, LiteRuntime::Direct { lite_bin: "/nonexistent".into() });
    cfg.api_token = token.map(str::to_string);
    Agent::new(cfg, EventBus::new(100)).unwrap()
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
