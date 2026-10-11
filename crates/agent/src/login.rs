//! Account login: one-shot `lite --login` process with a two-phase 2FA flow.
//!
//! 1. [`Agent::begin_login`] spawns lite and returns once the login settles or
//!    lite starts waiting for `2fa.txt` (`LoginState::Need2fa`).
//! 2. [`Agent::submit_code`] writes the code to `2fa.txt` and waits again.
//!
//! On success the account is (re)registered and its service instance started.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};

use crate::accounts::{account_id, normalize_username, AccountRecord};
use crate::agent::{describe_exit, terminate, Agent};
use crate::api::{InstanceState, LoginRequest, LoginState, LoginView};
use crate::events::{now_millis, Event};
use crate::lite::{pump_output, Signal};

/// How long an HTTP call waits for the login to settle before returning the
/// in-progress state.
const SETTLE_WAIT: Duration = Duration::from_secs(150);
/// Finished sessions stay queryable for a while.
const SESSION_RETENTION: Duration = Duration::from_secs(600);

#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("{0}")]
    Invalid(String),
    #[error("a login for this account is already in progress")]
    Busy,
    #[error("no pending login for this account; start the login again")]
    NoSession,
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

pub struct LoginSession {
    pub id: String,
    pub username: String,
    started_at: i64,
    state: watch::Sender<(LoginState, Option<String>)>,
    cancel: Mutex<Option<oneshot::Sender<()>>>,
}

impl LoginSession {
    pub fn view(&self) -> LoginView {
        let (state, message) = self.state.borrow().clone();
        LoginView {
            login_id: self.id.clone(),
            username: self.username.clone(),
            state,
            message,
            started_at: self.started_at,
        }
    }

    fn is_active(&self) -> bool {
        matches!(self.state.borrow().0, LoginState::Running | LoginState::Need2fa)
    }
}

impl Agent {
    fn set_login(&self, s: &LoginSession, state: LoginState, msg: Option<String>) {
        s.state.send_replace((state, msg));
        self.bus.emit(Event::Login { login: s.view() });
    }

    pub fn login_sessions(&self) -> Vec<LoginView> {
        self.logins.lock().unwrap().values().map(|s| s.view()).collect()
    }

    pub fn login_session(&self, id: &str) -> Option<LoginView> {
        self.logins.lock().unwrap().get(id).map(|s| s.view())
    }

    pub async fn begin_login(self: &Arc<Self>, req: LoginRequest) -> Result<LoginView, LoginError> {
        let username = normalize_username(&req.username);
        if username.is_empty() || req.password.is_empty() {
            return Err(LoginError::Invalid("username and password are required".into()));
        }
        if username.contains(':') || username.contains('\n') || req.password.contains('\n') {
            return Err(LoginError::Invalid("username must not contain ':' or line breaks".into()));
        }
        let id = account_id(&username);

        let session = {
            let mut logins = self.logins.lock().unwrap();
            if let Some(s) = logins.get(&id) {
                if s.is_active() {
                    // Repeating the first step while a code is awaited is harmless.
                    if s.state.borrow().0 == LoginState::Need2fa {
                        return Ok(s.view());
                    }
                    return Err(LoginError::Busy);
                }
            }
            let (state, _) = watch::channel((LoginState::Running, Some("signing in".to_string())));
            let s = Arc::new(LoginSession {
                id: id.clone(),
                username: username.clone(),
                started_at: now_millis(),
                state,
                cancel: Mutex::new(None),
            });
            logins.insert(id.clone(), s.clone());
            s
        };
        self.bus.emit(Event::Login { login: session.view() });

        // The service instance shares the base dir; it must not run meanwhile.
        let existing = self.instance(&id);
        if let Some(inst) = &existing {
            self.stop_instance(inst).await;
            inst.set_state(InstanceState::LoggingIn, None);
        }

        let layout = self.layout().clone();
        let started = (|| -> std::io::Result<tokio::process::Child> {
            let dir = layout.ensure_account_dir(&id)?;
            let _ = std::fs::remove_file(layout.code_file(&id));
            // Pass the credentials through a file instead of argv so they do
            // not show up in the process list. lite reads it via LITE_ARGS_FILE.
            let device_info = req.device_info.as_deref().filter(|d| !d.is_empty());
            let args = self.cfg.lite.login_args(&layout.lite_base_dir(&id), &username, &req.password, device_info);
            write_private(&dir.join(".login-args"), &(args.join("\n") + "\n"))?;
            let mut cmd = self.cfg.runtime.command(&[]);
            cmd.env("LITE_ARGS_FILE", format!("{}/.login-args", layout.lite_base_dir(&id)));
            cmd.spawn()
        })();
        let mut child = match started {
            Ok(c) => c,
            Err(e) => {
                let msg = format!("failed to start lite: {e}");
                self.finish_failed(&session, existing.is_none(), msg.clone()).await;
                return Ok(session.view());
            }
        };
        tracing::info!(source = %format!("login:{id}"), "login started for {username}");

        let (sig_tx, sig_rx) = mpsc::unbounded_channel();
        let source = format!("login:{id}");
        if let Some(out) = child.stdout.take() {
            pump_output(out, source.clone(), sig_tx.clone());
        }
        if let Some(err) = child.stderr.take() {
            pump_output(err, source, sig_tx);
        }
        let (cancel_tx, cancel_rx) = oneshot::channel();
        *session.cancel.lock().unwrap() = Some(cancel_tx);

        let agent = self.clone();
        let s = session.clone();
        let device_info = req.device_info.filter(|d| !d.is_empty());
        tokio::spawn(async move {
            agent.drive_login(s, child, sig_rx, cancel_rx, existing.is_none(), device_info).await;
        });

        Ok(wait_settled(&session).await)
    }

    async fn drive_login(
        self: Arc<Self>,
        session: Arc<LoginSession>,
        mut child: tokio::process::Child,
        mut signals: mpsc::UnboundedReceiver<Signal>,
        mut cancel: oneshot::Receiver<()>,
        is_new: bool,
        device_info: Option<String>,
    ) {
        let id = session.id.clone();
        let source = format!("login:{id}");
        let mut reason: Option<String> = None;
        let mut have_primary = false;
        let status = loop {
            tokio::select! {
                st = child.wait() => break st,
                Some(sig) = signals.recv() => match sig {
                    Signal::Need2fa => self.set_login(
                        &session,
                        LoginState::Need2fa,
                        Some("enter the verification code Apple sent to your devices".into()),
                    ),
                    Signal::LoginError { reason: r, primary } if primary || !have_primary => {
                        reason = Some(r);
                        have_primary |= primary;
                    }
                    _ => {}
                },
                _ = &mut cancel => {
                    terminate(&mut child).await;
                    reason = Some("login cancelled".into());
                    break child.wait().await;
                }
            }
        };
        let layout = self.layout().clone();
        let dir = layout.account_dir(&id);
        let _ = std::fs::remove_file(dir.join(".login-args"));
        let _ = std::fs::remove_file(layout.code_file(&id));

        let exited_ok = status.as_ref().is_ok_and(|s| s.success());
        if exited_ok && layout.has_tokens(&id) {
            tracing::info!(source = %source, "login succeeded; starting the service");
            self.set_login(&session, LoginState::Running, Some("logged in, starting the service".into()));
            let record = AccountRecord {
                id: id.clone(),
                username: session.username.clone(),
                region: String::new(),
                enabled: true,
                device_info: device_info.clone(),
                created_at: now_millis(),
            };
            let (inst, _) = self.insert_instance(record);
            inst.update_record(|r| {
                r.username = session.username.clone();
                r.enabled = true;
                if device_info.is_some() {
                    r.device_info = device_info.clone();
                }
            });
            self.save_registry();
            self.start_instance(&inst);
            let msg = self.wait_service(&inst).await;
            self.set_login(&session, LoginState::Success, Some(msg));
        } else {
            let msg = reason.unwrap_or_else(|| {
                if exited_ok {
                    "login finished but no tokens were written".to_string()
                } else {
                    format!("login failed ({})", describe_exit(status))
                }
            });
            self.finish_failed(&session, is_new, msg).await;
        }

        let agent = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(SESSION_RETENTION).await;
            let mut logins = agent.logins.lock().unwrap();
            if logins.get(&id).is_some_and(|s| Arc::ptr_eq(s, &session)) {
                logins.remove(&id);
            }
        });
    }

    async fn finish_failed(self: &Arc<Self>, session: &LoginSession, is_new: bool, msg: String) {
        let id = &session.id;
        tracing::warn!(source = %format!("login:{id}"), "login failed: {msg}");
        if is_new && self.instance(id).is_none() {
            let _ = std::fs::remove_dir_all(self.layout().account_dir(id));
        } else if let Some(inst) = self.instance(id) {
            // Bring the previous session back; its tokens may still be valid.
            if inst.record().enabled {
                self.start_instance(&inst);
            } else {
                inst.set_state(InstanceState::Stopped, None);
            }
        }
        self.set_login(session, LoginState::Failed, Some(msg));
    }

    /// Waits for a freshly started instance to leave the startup phase.
    async fn wait_service(&self, inst: &crate::agent::Instance) -> String {
        let deadline = tokio::time::Instant::now() + self.cfg.ready_timeout + Duration::from_secs(10);
        loop {
            match inst.state() {
                InstanceState::Running => return format!("logged in, region {}", inst.region()),
                InstanceState::NeedsLogin => {
                    return format!(
                        "logged in, but the service stopped: {}",
                        inst.view().last_error.unwrap_or_default()
                    )
                }
                _ if tokio::time::Instant::now() >= deadline => {
                    return "logged in; the service is still starting".into();
                }
                _ => tokio::time::sleep(Duration::from_millis(500)).await,
            }
        }
    }

    pub async fn submit_code(&self, id: &str, code: &str) -> Result<LoginView, LoginError> {
        let code = code.trim();
        if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
            return Err(LoginError::Invalid("the verification code must be 6 digits".into()));
        }
        let session = self.logins.lock().unwrap().get(id).cloned().ok_or(LoginError::NoSession)?;
        if !session.is_active() {
            return Err(LoginError::NoSession);
        }
        write_private(&self.layout().code_file(id), code)?;
        self.set_login(&session, LoginState::Running, Some("verifying the code".into()));
        tracing::info!(source = %format!("login:{id}"), "verification code submitted");
        Ok(wait_settled(&session).await)
    }

    pub fn cancel_login(&self, id: &str) -> bool {
        let session = self.logins.lock().unwrap().get(id).cloned();
        match session.and_then(|s| s.cancel.lock().unwrap().take()) {
            Some(tx) => tx.send(()).is_ok(),
            None => false,
        }
    }

    /// Stops the account, forgets it and deletes its data.
    pub async fn remove_account(&self, id: &str) -> bool {
        self.cancel_login(id);
        let Some(inst) = self.instance(id) else {
            return false;
        };
        self.stop_instance(&inst).await;
        self.remove_instance(id);
        self.save_registry();
        if let Err(e) = std::fs::remove_dir_all(self.layout().account_dir(id)) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("failed to delete data of {id}: {e}");
            }
        }
        tracing::info!("account {} removed", inst.record().username);
        true
    }

    pub async fn set_enabled(self: &Arc<Self>, id: &str, enabled: bool) -> bool {
        let Some(inst) = self.instance(id) else {
            return false;
        };
        if inst.update_record(|r| r.enabled = enabled) {
            self.save_registry();
        }
        if enabled {
            self.start_instance(&inst);
        } else {
            self.stop_instance(&inst).await;
        }
        true
    }

    pub async fn restart_account(self: &Arc<Self>, id: &str) -> bool {
        let Some(inst) = self.instance(id) else {
            return false;
        };
        self.stop_instance(&inst).await;
        self.start_instance(&inst);
        true
    }
}

async fn wait_settled(session: &LoginSession) -> LoginView {
    let mut rx = session.state.subscribe();
    let _ = tokio::time::timeout(SETTLE_WAIT, async {
        loop {
            if rx.borrow_and_update().0 != LoginState::Running {
                return;
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    })
    .await;
    session.view()
}

fn write_private(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
        f.write_all(content.as_bytes())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, content)
    }
}
