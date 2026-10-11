//! Active health checks of lite instances.
//!
//! `/status` only proves that lite's HTTP server is up: it answers from the
//! token cache and never touches FairPlay. The failures that matter most
//! (`KD*` / `CKC*`: a stale key context, or Apple refusing the key) leave lite
//! "healthy" by that measure, and in-process recovery does not work, so the
//! only cure is a restart.
//!
//! The check that exercises the real path is a `/key` request for the prefetch
//! key (`adamId=0`), the same thing a client does before playing anything.
//! Log keywords are only a cheap *suspicion* here; the probe is the judge:
//!
//! ```text
//! probe ok                              -> healthy (clears the FairPlay circuit)
//! probe failed, Apple unreachable       -> network problem, restarting cannot help
//! probe failed twice, Apple reachable   -> lite is broken: restart (rate limited)
//! restart budget exhausted              -> fall back to the circuit breaker
//! ```

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::agent::{Agent, Instance};
use crate::api::{InstanceState, CODE_FAIL};
use crate::events::now_millis;
use crate::router::{InFlight, SLOT_WAIT};

/// The key lite derives its shared FairPlay context from; requesting it needs
/// no catalog item and exercises the whole key path.
pub const PREFETCH_KEY_URI: &str = "skd://itunes.apple.com/P000000000/s1/e1";

/// Consecutive failed probes (with Apple reachable) before restarting.
const CONFIRMATIONS: u32 = 2;
/// Automatic health restarts allowed per account within [`RESTART_WINDOW`];
/// past that the circuit breaker excludes the account instead, so a problem a
/// restart does not fix cannot turn into a restart loop.
const MAX_HEALTH_RESTARTS: usize = 3;
const RESTART_WINDOW: Duration = Duration::from_secs(3600);
/// `/health?probe=1` reuses a probe younger than this.
const PROBE_REUSE: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Verdict {
    /// lite delivered a key.
    Ok,
    /// Every request slot was taken: real traffic, nothing learned.
    Busy,
    /// lite is not answering at all (the process supervision owns this).
    LiteDown,
    /// The probe failed and Apple's key server is unreachable too.
    NetworkDown,
    /// The probe failed while Apple is reachable: lite itself is broken.
    Broken,
}

impl Verdict {
    pub fn is_healthy(self) -> bool {
        matches!(self, Verdict::Ok | Verdict::Busy)
    }
}

#[derive(Clone, Debug)]
pub struct ProbeReport {
    pub verdict: Verdict,
    pub detail: String,
    pub took: Duration,
}

#[derive(Clone)]
struct ProbeRecord {
    at: i64,
    checked: Instant,
    verdict: Verdict,
    detail: String,
    took_ms: u64,
}

#[derive(Default)]
pub(crate) struct HealthState {
    last: Option<ProbeRecord>,
    consecutive_failures: u32,
    last_key_ok: Option<(Instant, i64)>,
    restarts: VecDeque<Instant>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthView {
    /// `None` until the first probe.
    pub verdict: Option<Verdict>,
    pub detail: Option<String>,
    /// Unix ms of the last probe.
    pub checked_at: Option<i64>,
    pub took_ms: Option<u64>,
    pub consecutive_failures: u32,
    /// Unix ms of the last `/key` that real traffic or a probe got through.
    pub last_key_ok_at: Option<i64>,
    /// Restarts this module triggered within the last hour.
    pub health_restarts: usize,
}

impl Instance {
    /// Stores a probe result; returns the number of consecutive failed
    /// probes (network problems and busy slots do not count).
    pub(crate) fn record_probe(&self, report: &ProbeReport) -> u32 {
        let fails = {
            let mut h = self.health.lock().unwrap();
            h.last = Some(ProbeRecord {
                at: now_millis(),
                checked: Instant::now(),
                verdict: report.verdict,
                detail: report.detail.clone(),
                took_ms: report.took.as_millis() as u64,
            });
            match report.verdict {
                Verdict::Ok => {
                    h.consecutive_failures = 0;
                    h.last_key_ok = Some((Instant::now(), now_millis()));
                }
                Verdict::Broken => h.consecutive_failures += 1,
                _ => {}
            }
            h.consecutive_failures
        };
        self.notify();
        fails
    }

    /// A real `/key` request succeeded: nothing is wrong with this instance.
    pub(crate) fn note_key_ok(&self) {
        {
            let mut h = self.health.lock().unwrap();
            h.last_key_ok = Some((Instant::now(), now_millis()));
            h.consecutive_failures = 0;
        }
        self.circuit_reset();
    }

    pub(crate) fn health_failures(&self) -> u32 {
        self.health.lock().unwrap().consecutive_failures
    }

    pub(crate) fn key_ok_within(&self, d: Duration) -> bool {
        self.health.lock().unwrap().last_key_ok.is_some_and(|(t, _)| t.elapsed() < d)
    }

    fn probed_within(&self, d: Duration) -> bool {
        self.health.lock().unwrap().last.as_ref().is_some_and(|r| r.checked.elapsed() < d)
    }

    /// Spends one automatic restart if the budget allows it.
    pub(crate) fn take_restart_budget(&self, max: usize, window: Duration) -> bool {
        let mut h = self.health.lock().unwrap();
        let now = Instant::now();
        while h.restarts.front().is_some_and(|t| now.duration_since(*t) > window) {
            h.restarts.pop_front();
        }
        if h.restarts.len() >= max {
            return false;
        }
        h.restarts.push_back(now);
        true
    }

    pub fn health_view(&self) -> HealthView {
        let h = self.health.lock().unwrap();
        HealthView {
            verdict: h.last.as_ref().map(|r| r.verdict),
            detail: h.last.as_ref().map(|r| r.detail.clone()),
            checked_at: h.last.as_ref().map(|r| r.at),
            took_ms: h.last.as_ref().map(|r| r.took_ms),
            consecutive_failures: h.consecutive_failures,
            last_key_ok_at: h.last_key_ok.map(|(_, at)| at),
            health_restarts: h.restarts.iter().filter(|t| t.elapsed() <= RESTART_WINDOW).count(),
        }
    }

    /// Whether the account can serve right now, as far as is known.
    pub fn is_healthy(&self) -> bool {
        if self.state() != InstanceState::Running || self.circuit_open() {
            return false;
        }
        self.health.lock().unwrap().last.as_ref().map_or(true, |r| r.verdict.is_healthy())
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountHealth {
    pub id: String,
    pub username: String,
    pub region: String,
    pub state: InstanceState,
    pub degraded: bool,
    pub healthy: bool,
    pub health: HealthView,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthReport {
    /// At least one account can serve.
    pub healthy: bool,
    /// `ok` (every enabled account healthy), `degraded`, `down`, `starting`.
    pub status: &'static str,
    pub ready: bool,
    pub accounts: Vec<AccountHealth>,
}

impl Agent {
    /// Requests the prefetch key from one instance and judges the outcome.
    pub async fn probe_instance(&self, inst: &Arc<Instance>) -> ProbeReport {
        let started = Instant::now();
        let (verdict, detail) = self.run_probe(inst).await;
        let report = ProbeReport { verdict, detail, took: started.elapsed() };
        inst.record_probe(&report);
        match verdict {
            Verdict::Ok | Verdict::Busy => {
                tracing::debug!(source = %inst.id, "health probe: {:?} ({}ms)", verdict, report.took.as_millis())
            }
            _ => tracing::warn!(source = %inst.id, "health probe: {:?}: {}", verdict, report.detail),
        }
        report
    }

    async fn run_probe(&self, inst: &Arc<Instance>) -> (Verdict, String) {
        let port = match inst.port() {
            Some(p) if inst.state() == InstanceState::Running => p,
            _ => return (Verdict::LiteDown, "instance is not running".into()),
        };
        let _permit = match tokio::time::timeout(SLOT_WAIT, inst.sem.acquire()).await {
            Ok(Ok(p)) => p,
            _ => return (Verdict::Busy, "all request slots are busy".into()),
        };
        inst.in_flight.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let _guard = InFlight(inst);

        // A little longer than a client request may take, so a probe does not
        // fail just because lite is slow (QEMU TCG).
        let timeout = self.cfg.upstream_timeout + Duration::from_secs(15);
        let sent = self
            .upstream
            .get(format!("http://127.0.0.1:{port}/key"))
            .query(&[("adamId", "0"), ("uri", PREFETCH_KEY_URI)])
            .timeout(timeout)
            .send()
            .await;
        let failure = match sent {
            Ok(resp) => {
                let body = resp.bytes().await.unwrap_or_default();
                match key_outcome(&body) {
                    Ok(()) => return (Verdict::Ok, "key delivered".into()),
                    Err(why) => why,
                }
            }
            Err(e) if e.is_connect() => return (Verdict::LiteDown, format!("cannot reach lite: {e}")),
            Err(e) if e.is_timeout() => format!("no key within {}s", timeout.as_secs()),
            Err(e) => format!("request failed: {e}"),
        };
        // Same proxy as lite: tells "Apple cannot be reached" from "lite is broken".
        if self.apple.reachable().await {
            (Verdict::Broken, failure)
        } else {
            (Verdict::NetworkDown, format!("{failure}; Apple's key server is unreachable from here"))
        }
    }

    /// Probes every running instance (skipping ones probed a moment ago).
    /// A broken instance wakes its monitor, which confirms and restarts it.
    pub async fn probe_all(&self) {
        let running: Vec<_> = self.instances().into_iter().filter(|i| i.state() == InstanceState::Running).collect();
        let probes = running.iter().filter(|i| !i.probed_within(PROBE_REUSE)).map(|inst| async move {
            if self.probe_instance(inst).await.verdict == Verdict::Broken {
                inst.suspect.notify_one();
            }
        });
        futures_util::future::join_all(probes).await;
    }

    pub fn health_report(&self) -> HealthReport {
        let all = self.instances();
        let accounts: Vec<AccountHealth> = all
            .iter()
            .map(|i| {
                let rec = i.record();
                AccountHealth {
                    id: rec.id,
                    username: rec.username,
                    region: rec.region,
                    state: i.state(),
                    degraded: i.circuit_open(),
                    healthy: i.is_healthy(),
                    health: i.health_view(),
                }
            })
            .collect();
        let enabled: Vec<_> =
            all.iter().zip(&accounts).filter(|(i, _)| i.record().enabled).map(|(_, a)| a.healthy).collect();
        let good = enabled.iter().filter(|h| **h).count();
        let ready = self.is_ready();
        let status = match (good, enabled.len()) {
            (0, _) if !ready => "starting",
            (0, _) => "down",
            (g, n) if g == n => "ok",
            _ => "degraded",
        };
        HealthReport { healthy: good > 0, status, ready, accounts }
    }

    /// Watches one running instance and returns why it should be restarted.
    /// Runs for as long as the instance does; the supervisor aborts it.
    ///
    /// Wakes on a FairPlay log signal ([`Instance::suspect`]) and, unless
    /// disabled, every `probe_interval` when real traffic has not shown a
    /// working `/key` in the meantime.
    pub(crate) async fn health_monitor(self: Arc<Self>, inst: Arc<Instance>) -> String {
        let interval = self.cfg.probe_interval;
        let mut wait = interval;
        let mut last_probe: Option<Instant> = None;
        loop {
            let periodic = tokio::select! {
                _ = inst.suspect.notified() => false,
                _ = sleep_or_never(wait) => true,
            };
            // A burst of log lines (or a confirmation) must not hammer Apple.
            if let Some(rest) = last_probe.and_then(|t| self.cfg.probe_confirm_gap.checked_sub(t.elapsed())) {
                tokio::time::sleep(rest).await;
            }
            if periodic && inst.health_failures() == 0 && !inst.circuit_open() && inst.key_ok_within(interval) {
                // Real traffic already proved the key path works.
                wait = interval;
                continue;
            }

            let report = self.probe_instance(&inst).await;
            last_probe = Some(Instant::now());
            match report.verdict {
                Verdict::Ok => {
                    inst.circuit_reset();
                    wait = interval;
                }
                Verdict::Busy | Verdict::LiteDown | Verdict::NetworkDown => wait = self.cfg.probe_recheck,
                Verdict::Broken => {
                    let fails = inst.health_failures();
                    if fails < CONFIRMATIONS {
                        wait = self.cfg.probe_confirm_gap;
                    } else if inst.take_restart_budget(MAX_HEALTH_RESTARTS, RESTART_WINDOW) {
                        return format!("key probe failed {fails}x with Apple reachable ({})", report.detail);
                    } else {
                        tracing::warn!(
                            source = %inst.id,
                            "still failing after {MAX_HEALTH_RESTARTS} health restarts within {}min; \
                             excluding the account instead",
                            RESTART_WINDOW.as_secs() / 60
                        );
                        inst.circuit_trip();
                        wait = self.cfg.probe_recheck;
                    }
                }
            }
        }
    }
}

async fn sleep_or_never(d: Duration) {
    if d.is_zero() {
        std::future::pending::<()>().await;
    } else {
        tokio::time::sleep(d).await;
    }
}

/// `Ok` when the lite envelope carries a content key.
fn key_outcome(body: &[u8]) -> Result<(), String> {
    let v: serde_json::Value = serde_json::from_slice(body).map_err(|_| "lite sent an unparsable response".to_string())?;
    let code = v.get("code").and_then(|c| c.as_i64()).unwrap_or(CODE_FAIL);
    let has_key = v.pointer("/data/contentKey").and_then(|k| k.as_str()).is_some_and(|k| !k.is_empty());
    if code == 0 && has_key {
        return Ok(());
    }
    let msg = v.get("msg").and_then(|m| m.as_str()).unwrap_or("no message");
    Err(format!("lite answered code {code}: {msg}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_outcome_needs_code_and_key() {
        assert!(key_outcome(br#"{"code":0,"msg":"SUCCESS","data":{"contentKey":"abc"}}"#).is_ok());
        assert!(key_outcome(br#"{"code":0,"msg":"SUCCESS","data":{"contentKey":""}}"#).is_err());
        let e = key_outcome(br#"{"code":500,"msg":"key retrieval failed","data":null}"#).unwrap_err();
        assert!(e.contains("500") && e.contains("key retrieval failed"));
        assert!(key_outcome(b"<html>").is_err());
    }

    #[test]
    fn only_ok_and_busy_count_as_healthy() {
        assert!(Verdict::Ok.is_healthy() && Verdict::Busy.is_healthy());
        assert!(!Verdict::Broken.is_healthy() && !Verdict::NetworkDown.is_healthy() && !Verdict::LiteDown.is_healthy());
    }
}
