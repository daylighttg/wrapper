//! Picks an account able to serve a request and forwards it to its lite
//! instance, retrying on another account when the failure may be
//! instance-specific.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use rand::seq::SliceRandom;
use reqwest::Method;

use crate::agent::{Agent, Instance};
use crate::api::{envelope_code, InstanceState, CODE_OK};
use crate::apple::{region_supports_lang, Kind};

const MAX_ATTEMPTS: usize = 3;
const RETRY_BUDGET: Duration = Duration::from_secs(45);
pub(crate) const SLOT_WAIT: Duration = Duration::from_secs(15);

#[derive(Debug)]
pub enum RouteError {
    NoInstance,
    Upstream(String),
}

impl std::fmt::Display for RouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RouteError::NoInstance => f.write_str("no available instance"),
            RouteError::Upstream(e) => write!(f, "upstream error: {e}"),
        }
    }
}

pub struct Forwarded {
    pub body: Bytes,
    pub code: i64,
}

/// Decrements the in-flight counter on drop.
pub(crate) struct InFlight<'a>(pub(crate) &'a Instance);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Whether a lite business failure is worth retrying on another account.
/// Malformed requests and "no lyrics in that language" are the same
/// everywhere; anything else may be instance-specific.
pub fn should_retry(path: &str, code: i64) -> bool {
    !(code == 400 || (path == "/lyrics" && code == 404))
}

impl Agent {
    /// Running accounts that are not temporarily excluded.
    pub fn serving(&self) -> Vec<Arc<Instance>> {
        self.instances()
            .into_iter()
            .filter(|i| i.state() == InstanceState::Running && i.port().is_some() && !i.circuit_open())
            .collect()
    }

    /// Accounts whose storefront can serve `adam_id`, least busy first.
    pub async fn candidates(&self, adam_id: &str) -> Vec<Arc<Instance>> {
        let mut serving = self.serving();
        serving.shuffle(&mut rand::thread_rng());
        let regions: HashSet<String> = serving.iter().map(|i| i.region()).collect();
        // With a single storefront there is nothing to choose between.
        let mut picked = if regions.len() <= 1 || adam_id == "0" {
            serving
        } else {
            self.filter_by_catalog(serving, &regions, adam_id).await
        };
        picked.sort_by_key(|i| i.in_flight.load(Ordering::Relaxed));
        picked
    }

    async fn filter_by_catalog(
        &self,
        serving: Vec<Arc<Instance>>,
        regions: &HashSet<String>,
        adam_id: &str,
    ) -> Vec<Arc<Instance>> {
        for kind in [Kind::Song, Kind::MusicVideo] {
            let probes = regions.iter().map(|r| async move { (r.clone(), self.apple.available(kind, r, adam_id).await) });
            let results: Vec<_> = futures_util::future::join_all(probes).await;
            let errors = results.iter().filter(|(_, r)| r.is_err()).count();
            let ok: HashSet<String> = results.into_iter().filter_map(|(r, v)| v.ok().filter(|v| *v).map(|_| r)).collect();
            if !ok.is_empty() {
                return serving.into_iter().filter(|i| ok.contains(&i.region())).collect();
            }
            if errors == regions.len() {
                // Apple probing is unavailable (no network to amp-api, token
                // scrape failing...): fall back to trying everyone.
                tracing::debug!("catalog probes failed for {adam_id}; trying every account");
                return serving;
            }
        }
        Vec::new()
    }

    /// Prefers an account whose storefront has lyrics for the song in
    /// `language`; `None` when probing finds nothing.
    ///
    /// Two tiers, like wrapper-manager: regions whose store lists the
    /// language (a region without it may serve the lyrics but not their
    /// translations) are probed first, the others only if none of those has
    /// the lyrics.
    pub async fn lyrics_candidate(&self, adam_id: &str, language: &str, script: &str) -> Option<Arc<Instance>> {
        let serving = self.serving();
        if serving.len() <= 1 {
            return None;
        }
        // One probe per region, using any account of that region.
        let mut by_region: HashMap<String, Vec<Arc<Instance>>> = HashMap::new();
        for i in serving {
            by_region.entry(i.region()).or_default().push(i);
        }
        let langs = self.apple.storefront_langs().await;
        let (preferred, others): (Vec<&String>, Vec<&String>) = by_region
            .keys()
            .partition(|r| langs.as_deref().is_some_and(|l| region_supports_lang(l, r, language)));
        for tier in [preferred, others] {
            let probes = tier.into_iter().map(|region| {
                let insts = &by_region[region];
                async move {
                    let token = self.layout().read_token(&insts[0].id, "MUSIC_TOKEN");
                    let ok = !token.is_empty() && self.apple.has_lyrics(region, adam_id, language, script, &token).await;
                    (region, ok)
                }
            });
            let hits: Vec<&String> = futures_util::future::join_all(probes)
                .await
                .into_iter()
                .filter_map(|(r, ok)| ok.then_some(r))
                .collect();
            if let Some(region) = hits.choose(&mut rand::thread_rng()) {
                return by_region[*region].iter().min_by_key(|i| i.in_flight.load(Ordering::Relaxed)).cloned();
            }
        }
        None
    }

    /// Sends one request to one lite instance.
    pub async fn forward(
        &self,
        inst: &Instance,
        method: Method,
        path: &str,
        query: Option<&str>,
        body: Bytes,
    ) -> Result<Forwarded, String> {
        let port = inst.port().ok_or("instance is not running")?;
        let _permit = tokio::time::timeout(SLOT_WAIT, inst.sem.acquire())
            .await
            .map_err(|_| "instance busy".to_string())?
            .map_err(|_| "instance closed".to_string())?;
        inst.in_flight.fetch_add(1, Ordering::Relaxed);
        let _guard = InFlight(inst);

        let mut url = format!("http://127.0.0.1:{port}{path}");
        if let Some(q) = query.filter(|q| !q.is_empty()) {
            url.push('?');
            url.push_str(q);
        }
        let mut req = self.upstream.request(method, url);
        if !body.is_empty() {
            req = req.header("Content-Type", "application/json").body(body);
        }
        let resp = req.send().await.map_err(|e| e.to_string())?;
        let body = resp.bytes().await.map_err(|e| e.to_string())?;
        let code = envelope_code(&body);
        if code == CODE_OK && path == "/key" {
            // Real traffic proves the key path works: no probe needed.
            inst.note_key_ok();
        }
        Ok(Forwarded { body, code })
    }

    /// Forwards to the given candidates in order with the retry policy.
    pub async fn forward_with_retry(
        &self,
        candidates: Vec<Arc<Instance>>,
        method: Method,
        path: &str,
        query: Option<&str>,
        body: Bytes,
    ) -> Result<Forwarded, RouteError> {
        if candidates.is_empty() {
            return Err(RouteError::NoInstance);
        }
        let deadline = Instant::now() + RETRY_BUDGET;
        let mut last: Option<Forwarded> = None;
        let mut last_err = None;
        let n = candidates.len();
        for (i, inst) in candidates.into_iter().enumerate().take(MAX_ATTEMPTS) {
            if i > 0 && Instant::now() > deadline {
                break;
            }
            match self.forward(&inst, method.clone(), path, query, body.clone()).await {
                Ok(f) if f.code == CODE_OK || i + 1 == n || !should_retry(path, f.code) => return Ok(f),
                Ok(f) => {
                    tracing::info!(source = %inst.id, "{path} failed with code {}; trying another account", f.code);
                    last = Some(f);
                }
                Err(e) => {
                    tracing::warn!(source = %inst.id, "{path} transport error: {e}");
                    last_err = Some(e);
                }
            }
        }
        match (last, last_err) {
            (Some(f), _) => Ok(f),
            (None, Some(e)) => Err(RouteError::Upstream(e)),
            (None, None) => Err(RouteError::NoInstance),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_policy() {
        // wrapper-manager's retry_policy_test.go: caller errors and "no lyrics
        // in that language" are the same on every account, the rest is not.
        for (path, code, want) in [
            ("/lyrics", 404, false),
            ("/lyrics", 400, false),
            ("/m3u8", 400, false),
            ("/key", 400, false),
            ("/lyrics", 500, true),
            ("/key", 500, true),
            ("/m3u8", 404, true),
            ("/m3u8", 500, true),
            ("/webplayback", 404, true),
            ("/webplayback", 500, true),
        ] {
            assert_eq!(should_retry(path, code), want, "{path} {code}");
        }
    }
}
