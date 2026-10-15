//! Request counters (`<state>/stats.json`): per account and endpoint, plus
//! per-day totals of the last [`KEEP_DAYS`] days. Kept in memory and written
//! out every minute and at shutdown; a crash loses at most that minute.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::events::now_millis;

/// Endpoints that are counted, in display order.
pub const ENDPOINTS: [&str; 5] = ["m3u8", "key", "lyrics", "webplayback", "license"];
const DAY_MS: i64 = 86_400_000;
pub const KEEP_DAYS: i64 = 30;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Counter {
    pub ok: u64,
    pub fail: u64,
}

/// Endpoint name (without the slash) -> counter.
pub type Counts = BTreeMap<String, Counter>;

fn bump(counts: &mut Counts, endpoint: &str, ok: bool) {
    let c = counts.entry(endpoint.to_string()).or_default();
    if ok {
        c.ok += 1;
    } else {
        c.fail += 1;
    }
}

/// Days since the Unix epoch (UTC).
pub fn day_of(ms: i64) -> i64 {
    ms.div_euclid(DAY_MS)
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct AccountStats {
    pub total: Counts,
    /// The day `today` counts (see [`day_of`]).
    pub day: i64,
    pub today: Counts,
    /// Unix ms of the last request this account served (successfully or not).
    pub last_used: Option<i64>,
}

impl AccountStats {
    /// The record as of `day`: `today` is empty when it belongs to an earlier day.
    fn as_of(&self, day: i64) -> Self {
        let mut s = self.clone();
        if s.day != day {
            s.day = day;
            s.today.clear();
        }
        s
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct StatsData {
    /// Unix ms when counting started.
    pub since: i64,
    /// Every request, including those no account could serve.
    pub total: Counts,
    pub daily: BTreeMap<i64, Counts>,
    pub accounts: HashMap<String, AccountStats>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayCounts {
    pub day: i64,
    pub counts: Counts,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsSummary {
    pub since: i64,
    pub total: Counts,
    pub today: Counts,
    /// The last [`KEEP_DAYS`] days, oldest first, including empty ones.
    pub daily: Vec<DayCounts>,
}

pub struct Stats {
    path: PathBuf,
    data: Mutex<StatsData>,
    dirty: AtomicBool,
}

impl Stats {
    pub fn load(path: PathBuf) -> Self {
        let mut data: StatsData = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).map_err(|e| tracing::warn!("stats.json is invalid ({e}); starting over")).ok())
            .unwrap_or_default();
        if data.since == 0 {
            data.since = now_millis();
        }
        Self { path, data: Mutex::new(data), dirty: AtomicBool::new(false) }
    }

    pub fn record(&self, account: Option<&str>, endpoint: &str, ok: bool) {
        self.record_at(now_millis(), account, endpoint, ok);
    }

    pub(crate) fn record_at(&self, now: i64, account: Option<&str>, endpoint: &str, ok: bool) {
        let day = day_of(now);
        let mut d = self.data.lock().unwrap();
        bump(&mut d.total, endpoint, ok);
        bump(d.daily.entry(day).or_default(), endpoint, ok);
        if d.daily.len() > KEEP_DAYS as usize {
            d.daily.retain(|k, _| *k > day - KEEP_DAYS);
        }
        if let Some(id) = account {
            let a = d.accounts.entry(id.to_string()).or_default();
            if a.day != day {
                a.day = day;
                a.today.clear();
            }
            bump(&mut a.total, endpoint, ok);
            bump(&mut a.today, endpoint, ok);
            a.last_used = Some(now);
        }
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn account(&self, id: &str) -> AccountStats {
        let day = day_of(now_millis());
        self.data.lock().unwrap().accounts.get(id).map(|a| a.as_of(day)).unwrap_or_default()
    }

    pub fn accounts(&self) -> HashMap<String, AccountStats> {
        let day = day_of(now_millis());
        self.data.lock().unwrap().accounts.iter().map(|(id, a)| (id.clone(), a.as_of(day))).collect()
    }

    pub fn summary(&self) -> StatsSummary {
        self.summary_at(now_millis())
    }

    pub(crate) fn summary_at(&self, now: i64) -> StatsSummary {
        let today = day_of(now);
        let d = self.data.lock().unwrap();
        StatsSummary {
            since: d.since,
            total: d.total.clone(),
            today: d.daily.get(&today).cloned().unwrap_or_default(),
            daily: (today - KEEP_DAYS + 1..=today)
                .map(|day| DayCounts { day, counts: d.daily.get(&day).cloned().unwrap_or_default() })
                .collect(),
        }
    }

    pub fn remove_account(&self, id: &str) {
        if self.data.lock().unwrap().accounts.remove(id).is_some() {
            self.dirty.store(true, Ordering::Relaxed);
        }
    }

    pub fn snapshot(&self) -> StatsData {
        self.data.lock().unwrap().clone()
    }

    /// Replaces everything (restoring a backup in replace mode).
    pub fn replace(&self, data: StatsData) {
        *self.data.lock().unwrap() = data;
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Takes over the per-account counters of restored accounts.
    pub fn set_account(&self, id: &str, stats: AccountStats) {
        self.data.lock().unwrap().accounts.insert(id.to_string(), stats);
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Writes the counters if they changed since the last flush.
    pub fn flush(&self) {
        if !self.dirty.swap(false, Ordering::Relaxed) {
            return;
        }
        let json = match serde_json::to_vec(&*self.data.lock().unwrap()) {
            Ok(j) => j,
            Err(e) => return tracing::warn!("cannot serialize stats: {e}"),
        };
        let tmp = self.path.with_extension("json.tmp");
        if let Err(e) = std::fs::write(&tmp, json).and_then(|_| std::fs::rename(&tmp, &self.path)) {
            tracing::warn!("cannot save {}: {e}", self.path.display());
            self.dirty.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_stats(name: &str) -> Stats {
        let dir = std::env::temp_dir().join(format!("wrapper-stats-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Stats::load(dir.join("stats.json"))
    }

    #[test]
    fn counts_per_account_and_day() {
        let s = temp_stats("count");
        let day1 = 20_000 * DAY_MS + 5;
        s.record_at(day1, Some("a"), "key", true);
        s.record_at(day1, Some("a"), "key", false);
        s.record_at(day1, None, "m3u8", false);
        let a = s.data.lock().unwrap().accounts["a"].clone();
        assert_eq!(a.total["key"], Counter { ok: 1, fail: 1 });
        assert_eq!(a.last_used, Some(day1));

        // The next day starts a new `today`, the total keeps counting.
        let day2 = day1 + DAY_MS;
        s.record_at(day2, Some("a"), "lyrics", true);
        let a = s.data.lock().unwrap().accounts["a"].clone();
        assert_eq!(a.today.len(), 1);
        assert_eq!(a.total.len(), 2);

        let sum = s.summary_at(day2);
        assert_eq!(sum.daily.len(), KEEP_DAYS as usize);
        assert_eq!(sum.daily.last().unwrap().day, day_of(day2));
        assert_eq!(sum.today["lyrics"].ok, 1);
        assert_eq!(sum.daily[KEEP_DAYS as usize - 2].counts["m3u8"].fail, 1);
        assert_eq!(sum.total["key"], Counter { ok: 1, fail: 1 });
    }

    #[test]
    fn old_days_are_dropped_and_flush_roundtrips() {
        let s = temp_stats("prune");
        for d in 0..(KEEP_DAYS + 5) {
            s.record_at((20_000 + d) * DAY_MS, Some("a"), "key", true);
        }
        assert_eq!(s.data.lock().unwrap().daily.len(), KEEP_DAYS as usize);
        s.flush();
        let again = Stats::load(s.path.clone());
        assert_eq!(again.snapshot(), s.snapshot());
        s.remove_account("a");
        assert!(s.snapshot().accounts.is_empty());
    }
}
