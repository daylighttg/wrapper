//! Backup and restore of the agent state as one `.tar.gz`:
//!
//! ```text
//! wrapper-backup/
//! ├── manifest.json        format, version, time, account list
//! ├── accounts.json        the registry (incl. owner key hashes)
//! ├── agent.json           public-mode settings
//! ├── stats.json           request counters
//! └── accounts/<id>/       each account's lite base dir (tokens, mpl_db, ANDROID_ID)
//! ```
//!
//! The account dirs hold the Apple session, so a restored account comes up
//! without a new login (and with the same device identity). The shared
//! Android data dir (`<state>/data`) holds no account state and is left out.
//!
//! Backups stream (nothing is buffered whole, which matters in a small QEMU
//! guest); a restore unpacks into a staging dir next to the accounts, checks
//! it, and only then swaps account dirs in, one account at a time.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use bytes::Bytes;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::accounts::{is_valid_id, AccountRecord, Layout};
use crate::agent::Agent;
use crate::api::InstanceState;
use crate::events::now_millis;
use crate::settings::{random_hex, AgentSettings};
use crate::stats::StatsData;

pub const FORMAT: u32 = 1;
const ROOT: &str = "wrapper-backup";
const TOP_FILES: &[&str] = &["manifest.json", "accounts.json", "agent.json", "stats.json"];
/// Files of an account dir that only matter while a login runs.
const SKIP: &[&str] = &["2fa.txt", ".login-args"];
const MAX_TOTAL_BYTES: u64 = 8 << 30;
const MAX_ENTRIES: usize = 200_000;
const CHUNK: usize = 64 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestAccount {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub region: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub format: u32,
    pub version: String,
    pub created_at: i64,
    pub accounts: Vec<ManifestAccount>,
}

/// What goes into a backup besides the account dirs.
pub struct Snapshot {
    pub records: Vec<AccountRecord>,
    pub settings: AgentSettings,
    pub stats: StatsData,
}

fn file_header(size: u64, mode: u32) -> tar::Header {
    let mut h = tar::Header::new_gnu();
    h.set_entry_type(tar::EntryType::Regular);
    h.set_size(size);
    h.set_mode(mode);
    h.set_mtime((now_millis() / 1000).max(0) as u64);
    h
}

fn add_bytes<W: Write>(tar: &mut tar::Builder<W>, name: &str, data: &[u8]) -> io::Result<()> {
    tar.append_data(&mut file_header(data.len() as u64, 0o644), name, data)
}

fn add_json<W: Write>(tar: &mut tar::Builder<W>, name: &str, value: &impl Serialize) -> io::Result<()> {
    add_bytes(tar, &format!("{ROOT}/{name}"), &serde_json::to_vec_pretty(value)?)
}

fn add_dir<W: Write>(tar: &mut tar::Builder<W>, dir: &Path, name: &str) -> io::Result<()> {
    let mut h = tar::Header::new_gnu();
    h.set_entry_type(tar::EntryType::Directory);
    h.set_size(0);
    h.set_mode(0o777);
    h.set_mtime((now_millis() / 1000).max(0) as u64);
    tar.append_data(&mut h, format!("{name}/"), io::empty())?;
    let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let fname = e.file_name();
        let Some(fname) = fname.to_str() else { continue };
        if SKIP.contains(&fname) || fname.ends_with(".tmp") {
            continue;
        }
        let meta = fs::symlink_metadata(e.path())?;
        let child = format!("{name}/{fname}");
        if meta.is_dir() {
            add_dir(tar, &e.path(), &child)?;
        } else if meta.is_file() {
            // Read it whole first: lite may rewrite a file while we copy it,
            // and a size that changes mid-entry would corrupt the archive.
            let data = match fs::read(e.path()) {
                Ok(d) => d,
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                Err(err) => return Err(err),
            };
            add_bytes(tar, &child, &data)?;
        }
    }
    Ok(())
}

/// Writes a backup archive to `out`.
pub fn write_archive<W: Write>(layout: &Layout, snap: &Snapshot, out: W) -> io::Result<W> {
    let mut tar = tar::Builder::new(GzEncoder::new(out, Compression::default()));
    let manifest = Manifest {
        format: FORMAT,
        version: env!("CARGO_PKG_VERSION").into(),
        created_at: now_millis(),
        accounts: snap
            .records
            .iter()
            .map(|r| ManifestAccount { id: r.id.clone(), username: r.username.clone(), region: r.region.clone() })
            .collect(),
    };
    add_json(&mut tar, "manifest.json", &manifest)?;
    add_json(&mut tar, "accounts.json", &snap.records)?;
    add_json(&mut tar, "agent.json", &snap.settings)?;
    add_json(&mut tar, "stats.json", &snap.stats)?;
    for rec in &snap.records {
        let dir = layout.account_dir(&rec.id);
        if dir.is_dir() {
            add_dir(&mut tar, &dir, &format!("{ROOT}/accounts/{}", rec.id))?;
        }
    }
    tar.into_inner()?.finish()
}

/// A backup unpacked into a staging dir.
pub struct Staged {
    pub dir: PathBuf,
    pub manifest: Manifest,
    pub records: Vec<AccountRecord>,
    pub settings: Option<AgentSettings>,
    pub stats: Option<StatsData>,
}

/// Normal path components only (no `..`, no root, valid UTF-8).
fn safe_components(path: &Path) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for c in path.components() {
        match c {
            Component::Normal(s) => out.push(s.to_str()?.to_string()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    match fs::read(path) {
        Ok(b) => serde_json::from_slice(&b).map(Some).map_err(|e| format!("{} in the backup is invalid: {e}", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Unpacks and checks a backup. Only the files a backup consists of are
/// extracted; links, devices and paths leaving the archive root are refused.
pub fn stage<R: Read>(input: R, staging: &Path) -> Result<Staged, String> {
    fs::create_dir_all(staging).map_err(|e| format!("cannot create {}: {e}", staging.display()))?;
    let mut archive = tar::Archive::new(GzDecoder::new(input));
    let entries = archive.entries().map_err(|e| format!("not a backup archive: {e}"))?;
    let (mut total, mut count) = (0u64, 0usize);
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("the backup is damaged: {e}"))?;
        count += 1;
        if count > MAX_ENTRIES {
            return Err("the backup has too many files".into());
        }
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        let parts = safe_components(&path).ok_or_else(|| format!("unsafe path in the backup: {}", path.display()))?;
        if parts.first().map(String::as_str) != Some(ROOT) {
            return Err("not a wrapper backup".into());
        }
        let rest = &parts[1..];
        let allowed = match rest {
            [] => false,
            [f] => TOP_FILES.contains(&f.as_str()) || f == "accounts",
            [a, id, ..] => a == "accounts" && is_valid_id(id),
        };
        if !allowed {
            continue;
        }
        let dest = rest.iter().fold(staging.to_path_buf(), |p, c| p.join(c));
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
        } else if kind.is_file() {
            total += entry.size();
            if total > MAX_TOTAL_BYTES {
                return Err("the backup is too large".into());
            }
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut f = fs::File::create(&dest).map_err(|e| e.to_string())?;
            io::copy(&mut entry, &mut f).map_err(|e| format!("the backup is damaged: {e}"))?;
        }
    }
    let manifest: Manifest =
        read_json(&staging.join("manifest.json"))?.ok_or("manifest.json is missing: not a wrapper backup")?;
    if manifest.format > FORMAT {
        return Err(format!("this backup was made by a newer wrapper ({}); upgrade first", manifest.version));
    }
    let records: Vec<AccountRecord> = read_json(&staging.join("accounts.json"))?.unwrap_or_default();
    Ok(Staged {
        dir: staging.to_path_buf(),
        manifest,
        records: records.into_iter().filter(|r| is_valid_id(&r.id)).collect(),
        settings: read_json(&staging.join("agent.json"))?,
        stats: read_json(&staging.join("stats.json"))?,
    })
}

/// `std::io::Write` that hands 64 KiB chunks to an async receiver (for
/// streaming an archive written on a blocking thread as an HTTP body).
pub struct ChannelWriter {
    tx: mpsc::Sender<io::Result<Bytes>>,
    buf: Vec<u8>,
}

impl ChannelWriter {
    pub fn new(tx: mpsc::Sender<io::Result<Bytes>>) -> Self {
        Self { tx, buf: Vec::with_capacity(CHUNK) }
    }

    fn send(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let chunk = Bytes::from(std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK)));
        self.tx.blocking_send(Ok(chunk)).map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "the download was aborted"))
    }
}

impl Write for ChannelWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        if self.buf.len() >= CHUNK {
            self.send()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.send()
    }
}

/// `std::io::Read` fed by an async byte stream (an upload or a download), for
/// unpacking on a blocking thread.
pub struct ChannelReader {
    rx: mpsc::Receiver<io::Result<Bytes>>,
    cur: Bytes,
}

impl Read for ChannelReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.cur.is_empty() {
            match self.rx.blocking_recv() {
                Some(Ok(b)) => self.cur = b,
                Some(Err(e)) => return Err(e),
                None => return Ok(0),
            }
        }
        let n = out.len().min(self.cur.len());
        out[..n].copy_from_slice(&self.cur.split_to(n));
        Ok(n)
    }
}

/// Pumps `stream` into a [`ChannelReader`] from a task.
pub fn stream_reader<S, E>(stream: S) -> ChannelReader
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    let (tx, rx) = mpsc::channel(16);
    tokio::spawn(async move {
        let mut stream = std::pin::pin!(stream);
        while let Some(item) = stream.next().await {
            let item = item.map_err(|e| io::Error::other(e.to_string()));
            if tx.send(item).await.is_err() {
                break;
            }
        }
    });
    ChannelReader { rx, cur: Bytes::new() }
}

/// `YYYYMMDD-HHMM` (UTC) for file names.
pub fn timestamp(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}", tod / 3600, tod % 3600 / 60)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RestoreMode {
    /// Add the backup's accounts; accounts in both are replaced by the backup.
    #[default]
    Merge,
    /// Make this agent an exact copy: accounts not in the backup are removed.
    Replace,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RestoreOptions {
    pub mode: RestoreMode,
    /// Also take over the backup's public-mode settings.
    pub settings: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreReport {
    pub accounts: Vec<String>,
    pub added: usize,
    pub replaced: usize,
    pub removed: usize,
    pub skipped: Vec<String>,
    pub settings: bool,
    pub backup_version: String,
    pub backup_created_at: i64,
}

struct ClearOnDrop<'a>(&'a std::sync::atomic::AtomicBool);

impl Drop for ClearOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl Agent {
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            records: self.instances().iter().map(|i| i.record()).collect(),
            settings: self.settings(),
            stats: self.stats.snapshot(),
        }
    }

    /// Restores a backup read from `input` (on a blocking thread).
    pub async fn restore_from<R: Read + Send + 'static>(self: &Arc<Self>, input: R, opts: RestoreOptions) -> Result<RestoreReport, String> {
        if self.restoring.swap(true, Ordering::SeqCst) {
            return Err("another restore is running".into());
        }
        let _guard = ClearOnDrop(&self.restoring);
        if !self.active_sessions().is_empty() {
            return Err("finish or cancel the running logins first".into());
        }
        let staging = self.layout().state_dir.join(format!(".restore-{}", random_hex(6)));
        let dir = staging.clone();
        let staged = tokio::task::spawn_blocking(move || stage(input, &dir)).await.map_err(|e| e.to_string());
        let result = match staged {
            Ok(Ok(staged)) => self.apply_restore(staged, opts).await,
            Ok(Err(e)) | Err(e) => Err(e),
        };
        let _ = fs::remove_dir_all(&staging);
        result
    }

    /// Downloads `<url>/api/backup` from another wrapper and restores it.
    pub async fn pull_backup(self: &Arc<Self>, url: &str, token: &str, opts: RestoreOptions) -> Result<RestoreReport, String> {
        let base = url.trim().trim_end_matches('/');
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            return Err("the address must start with http:// or https://".into());
        }
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|e| e.to_string())?;
        let mut req = client.get(format!("{base}/api/backup"));
        if !token.trim().is_empty() {
            req = req.bearer_auth(token.trim());
        }
        let resp = req.send().await.map_err(|e| format!("cannot reach {base}: {e}"))?;
        let is_json = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("json"));
        if !resp.status().is_success() || is_json {
            let status = resp.status();
            let msg = resp
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| v["msg"].as_str().map(str::to_string))
                .unwrap_or_else(|| status.to_string());
            return Err(format!("{base} refused the backup: {msg}"));
        }
        tracing::info!("restoring the backup of {base}");
        self.restore_from(stream_reader(resp.bytes_stream()), opts).await
    }

    async fn apply_restore(self: &Arc<Self>, staged: Staged, opts: RestoreOptions) -> Result<RestoreReport, String> {
        let layout = self.layout().clone();
        let mut report = RestoreReport {
            accounts: Vec::new(),
            added: 0,
            replaced: 0,
            removed: 0,
            skipped: Vec::new(),
            settings: false,
            backup_version: staged.manifest.version.clone(),
            backup_created_at: staged.manifest.created_at,
        };
        // Only accounts whose data made it into the backup.
        let (records, missing): (Vec<_>, Vec<_>) =
            staged.records.into_iter().partition(|r| staged.dir.join("accounts").join(&r.id).is_dir());
        for r in missing {
            tracing::warn!("the backup has no data for {}; skipped", r.username);
            report.skipped.push(r.username);
        }
        if opts.mode == RestoreMode::Replace {
            let keep: HashSet<&str> = records.iter().map(|r| r.id.as_str()).collect();
            for inst in self.instances() {
                if !keep.contains(inst.id.as_str()) {
                    self.remove_account(&inst.id).await;
                    report.removed += 1;
                }
            }
        }
        let trash = staged.dir.join(".replaced");
        for rec in &records {
            let existing = self.instance(&rec.id);
            if let Some(inst) = &existing {
                self.stop_instance(inst).await;
            }
            let src = staged.dir.join("accounts").join(&rec.id);
            let dest = layout.account_dir(&rec.id);
            let old = trash.join(&rec.id);
            let swapped = (|| -> io::Result<()> {
                fs::create_dir_all(dest.parent().unwrap_or(&layout.state_dir))?;
                if dest.exists() {
                    fs::create_dir_all(&trash)?;
                    fs::rename(&dest, &old)?;
                }
                if let Err(e) = fs::rename(&src, &dest) {
                    let _ = fs::rename(&old, &dest);
                    return Err(e);
                }
                layout.ensure_account_dir(&rec.id).map(|_| ())
            })();
            if let Err(e) = swapped {
                tracing::warn!("cannot restore {}: {e}", rec.username);
                if let Some(inst) = &existing {
                    if inst.record().enabled {
                        self.start_instance(inst);
                    }
                }
                report.skipped.push(rec.username.clone());
                continue;
            }
            match &existing {
                Some(inst) => {
                    inst.update_record(|r| *r = rec.clone());
                    report.replaced += 1;
                }
                None => {
                    self.insert_instance(rec.clone());
                    report.added += 1;
                }
            }
            report.accounts.push(rec.username.clone());
        }
        self.save_registry();

        if let Some(stats) = staged.stats {
            if opts.mode == RestoreMode::Replace {
                self.stats.replace(stats);
            } else {
                for rec in &records {
                    if let Some(s) = stats.accounts.get(&rec.id) {
                        self.stats.set_account(&rec.id, s.clone());
                    }
                }
            }
            self.stats.flush();
        }
        if opts.settings {
            if let Some(s) = staged.settings {
                self.replace_settings(s);
                report.settings = true;
            }
        }
        for rec in &records {
            if let Some(inst) = self.instance(&rec.id) {
                if inst.record().enabled {
                    self.start_instance(&inst);
                } else {
                    inst.set_state(InstanceState::Stopped, None);
                }
            }
        }
        tracing::info!(
            "restored {} account(s) from a backup of {} ({} added, {} replaced, {} removed)",
            report.accounts.len(),
            report.backup_version,
            report.added,
            report.replaced,
            report.removed
        );
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps() {
        assert_eq!(timestamp(0), "19700101-0000");
        // 2026-10-15 12:34 UTC
        assert_eq!(timestamp(1_792_067_640_000), "20261015-1234");
        assert_eq!(timestamp(951_782_400_000), "20000229-0000");
    }

    #[test]
    fn unsafe_paths_are_refused() {
        assert!(safe_components(Path::new("wrapper-backup/../x")).is_none());
        assert!(safe_components(Path::new("/etc/passwd")).is_none());
        assert_eq!(safe_components(Path::new("./wrapper-backup/a")).unwrap(), ["wrapper-backup", "a"]);
    }

    #[test]
    fn archive_roundtrip_keeps_account_files_and_skips_login_leftovers() {
        let base = std::env::temp_dir().join(format!("wrapper-backup-rt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let layout = Layout { state_dir: base.join("state"), lite_root: "/data".into() };
        let rec = AccountRecord {
            id: crate::accounts::account_id("a@b.c"),
            username: "a@b.c".into(),
            enabled: true,
            ..Default::default()
        };
        let dir = layout.ensure_account_dir(&rec.id).unwrap();
        fs::write(dir.join("MUSIC_TOKEN"), "m").unwrap();
        fs::write(dir.join("mpl_db/db"), "x").unwrap();
        fs::write(dir.join("2fa.txt"), "123456").unwrap();
        fs::write(dir.join(".login-args"), "secret").unwrap();
        let snap = Snapshot { records: vec![rec.clone()], settings: AgentSettings::default(), stats: StatsData::default() };
        let bytes = write_archive(&layout, &snap, Vec::new()).unwrap();

        let staged = stage(&bytes[..], &base.join("staging")).unwrap();
        assert_eq!(staged.records, vec![rec.clone()]);
        assert_eq!(staged.manifest.accounts[0].username, "a@b.c");
        let acc = staged.dir.join("accounts").join(&rec.id);
        assert_eq!(fs::read_to_string(acc.join("MUSIC_TOKEN")).unwrap(), "m");
        assert!(acc.join("mpl_db/db").exists());
        assert!(!acc.join("2fa.txt").exists());
        assert!(!acc.join(".login-args").exists());
        assert!(stage(&b"not a backup"[..], &base.join("bad")).is_err());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn foreign_entries_are_not_extracted() {
        let base = std::env::temp_dir().join(format!("wrapper-backup-foreign-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let mut tar = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        let manifest = serde_json::to_vec(&Manifest { format: 1, version: "x".into(), created_at: 0, accounts: vec![] }).unwrap();
        add_bytes(&mut tar, "wrapper-backup/manifest.json", &manifest).unwrap();
        add_bytes(&mut tar, "wrapper-backup/evil.sh", b"x").unwrap();
        add_bytes(&mut tar, "wrapper-backup/accounts/not-a-uuid/x", b"x").unwrap();
        let bytes = tar.into_inner().unwrap().finish().unwrap();
        let staging = base.join("s");
        let staged = stage(&bytes[..], &staging).unwrap();
        assert!(staged.records.is_empty());
        assert!(!staging.join("evil.sh").exists());
        assert!(!staging.join("accounts/not-a-uuid").exists());

        // A backup from a newer format is refused.
        let mut tar = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        let manifest = serde_json::to_vec(&Manifest { format: 99, version: "9".into(), created_at: 0, accounts: vec![] }).unwrap();
        add_bytes(&mut tar, "wrapper-backup/manifest.json", &manifest).unwrap();
        let bytes = tar.into_inner().unwrap().finish().unwrap();
        assert!(stage(&bytes[..], &base.join("t")).err().unwrap().contains("newer"));
        let _ = fs::remove_dir_all(&base);
    }
}
