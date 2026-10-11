//! Wire types: the wrapper-lite response envelope and the management views.

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Business codes shared with wrapper-lite / wrapper-manager.
pub const CODE_OK: i64 = 0;
pub const CODE_NEED_2FA: i64 = 2;
pub const CODE_FAIL: i64 = -1;

const CACHE_PUBLIC: &str = "public, max-age=3600";
const CACHE_NO_STORE: &str = "no-store";

/// `{"code":..,"msg":..,"data":..}`. Like wrapper-lite the HTTP status is 200
/// unless a management endpoint asks for a specific one.
#[derive(Debug)]
pub struct Envelope {
    pub status: StatusCode,
    pub code: i64,
    pub msg: String,
    pub data: Value,
}

impl Envelope {
    pub fn ok(data: impl Serialize) -> Self {
        Self {
            status: StatusCode::OK,
            code: CODE_OK,
            msg: "SUCCESS".into(),
            data: serde_json::to_value(data).unwrap_or(Value::Null),
        }
    }

    pub fn fail(msg: impl Into<String>) -> Self {
        Self { status: StatusCode::OK, code: CODE_FAIL, msg: msg.into(), data: Value::Null }
    }

    pub fn with_code(code: i64, msg: impl Into<String>, data: impl Serialize) -> Self {
        Self {
            status: StatusCode::OK,
            code,
            msg: msg.into(),
            data: serde_json::to_value(data).unwrap_or(Value::Null),
        }
    }

    pub fn status(mut self, status: StatusCode) -> Self {
        self.status = status;
        self
    }
}

impl IntoResponse for Envelope {
    fn into_response(self) -> Response {
        let body = serde_json::json!({ "code": self.code, "msg": self.msg, "data": self.data });
        let mut res = (self.status, axum::Json(body)).into_response();
        res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static(CACHE_NO_STORE));
        res
    }
}

/// A lite instance's response body, passed through unchanged.
pub struct LiteBody {
    pub body: Bytes,
    pub code: i64,
    pub cacheable: bool,
}

impl IntoResponse for LiteBody {
    fn into_response(self) -> Response {
        let cache = if self.cacheable && self.code == CODE_OK { CACHE_PUBLIC } else { CACHE_NO_STORE };
        let mut res = (StatusCode::OK, self.body).into_response();
        let h = res.headers_mut();
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
        res
    }
}

/// Extracts the business `code` from a lite envelope (-1 when unparsable).
pub fn envelope_code(body: &[u8]) -> i64 {
    #[derive(Deserialize)]
    struct Code {
        code: i64,
    }
    serde_json::from_slice::<Code>(body).map(|c| c.code).unwrap_or(CODE_FAIL)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum InstanceState {
    /// Not running and not scheduled (disabled, or stopped by the user).
    Stopped,
    /// Process spawned, waiting for `/status` to report a region.
    Starting,
    Running,
    /// Exited or failed health checks; waiting before the next restart.
    Backoff,
    /// A login for this account is in progress.
    LoggingIn,
    /// Apple rejected the session or subscription; needs a fresh login.
    NeedsLogin,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountView {
    pub id: String,
    pub username: String,
    pub region: String,
    pub enabled: bool,
    pub state: InstanceState,
    pub port: Option<u16>,
    pub pid: Option<u32>,
    pub last_error: Option<String>,
    pub restarts: u32,
    /// Unix ms when the current process became ready.
    pub running_since: Option<i64>,
    pub in_flight: usize,
    /// Temporarily excluded after repeated FairPlay failures.
    pub degraded: bool,
    /// Result of the active `/key` probe (see `/health`).
    pub health: crate::health::HealthView,
    pub created_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LoginState {
    Running,
    Need2fa,
    Success,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginView {
    /// Same as the account id.
    pub login_id: String,
    pub username: String,
    pub state: LoginState,
    pub message: Option<String>,
    pub started_at: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub device_info: Option<String>,
    /// wrapper-manager style: present on the second `/login` call.
    #[serde(default)]
    pub code: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CodeRequest {
    pub code: String,
}

/// wrapper-manager's `POST /logout` body: the Apple account name or the
/// account (instance) UUID; `id` wins when both are given.
#[derive(Debug, Default, Deserialize)]
pub struct LogoutRequest {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub id: String,
}

impl LogoutRequest {
    /// The account id to log out.
    pub fn resolve_id(&self) -> Result<String, &'static str> {
        let id = self.id.trim();
        if !id.is_empty() {
            return uuid::Uuid::parse_str(id)
                .map(|u| u.to_string())
                .map_err(|_| "invalid id (expected a UUID)");
        }
        if self.username.trim().is_empty() {
            return Err("missing username or id");
        }
        Ok(crate::accounts::account_id(&self.username))
    }
}
