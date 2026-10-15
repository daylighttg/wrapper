//! Persistent account registry (`<state>/accounts.json`) and on-disk layout.
//!
//! ```text
//! <state>/                 = what lite sees as /data
//! ├── accounts.json        registry (this module)
//! ├── agent.json           runtime settings (`settings`)
//! ├── stats.json           request counters (`stats`)
//! └── accounts/<id>/       per-account lite --base-dir
//!     ├── mpl_db/
//!     ├── MUSIC_TOKEN, STOREFRONT_ID, DEV_TOKEN, token_cache.json, ANDROID_ID
//!     └── 2fa.txt          only while a 2FA login is waiting
//! ```

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AccountRecord {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub region: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Per-account `--device-info` override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_info: Option<String>,
    #[serde(default)]
    pub created_at: i64,
    /// SHA-256 (hex) of the key handed out when the account was added
    /// through the public page; it lets that user remove the account again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_key_hash: Option<String>,
}

fn default_true() -> bool {
    true
}

pub fn normalize_username(username: &str) -> String {
    username.trim().to_lowercase()
}

/// Deterministic account id: UUIDv5 of the normalized username.
pub fn account_id(username: &str) -> String {
    Uuid::new_v5(&Uuid::nil(), normalize_username(username).as_bytes()).to_string()
}

/// Account ids are UUIDs; reject anything else before it touches a path.
pub fn is_valid_id(id: &str) -> bool {
    Uuid::parse_str(id).is_ok()
}

#[derive(Clone, Debug)]
pub struct Layout {
    /// Host path of the directory lite sees as `lite_root`.
    pub state_dir: PathBuf,
    /// The same directory as seen by lite (`/data` in the sandbox).
    pub lite_root: String,
}

impl Layout {
    pub fn registry_path(&self) -> PathBuf {
        self.state_dir.join("accounts.json")
    }

    pub fn settings_path(&self) -> PathBuf {
        self.state_dir.join("agent.json")
    }

    pub fn stats_path(&self) -> PathBuf {
        self.state_dir.join("stats.json")
    }

    pub fn account_dir(&self, id: &str) -> PathBuf {
        self.state_dir.join("accounts").join(id)
    }

    /// `--base-dir` argument for lite.
    pub fn lite_base_dir(&self, id: &str) -> String {
        format!("{}/accounts/{}", self.lite_root.trim_end_matches('/'), id)
    }

    pub fn code_file(&self, id: &str) -> PathBuf {
        self.account_dir(id).join("2fa.txt")
    }

    pub fn ensure_account_dir(&self, id: &str) -> io::Result<PathBuf> {
        let dir = self.account_dir(id);
        fs::create_dir_all(dir.join("mpl_db"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o777));
            let _ = fs::set_permissions(dir.join("mpl_db"), fs::Permissions::from_mode(0o777));
        }
        Ok(dir)
    }

    /// Reads a token file written by lite (trimmed, empty if missing).
    pub fn read_token(&self, id: &str, name: &str) -> String {
        fs::read_to_string(self.account_dir(id).join(name))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    pub fn has_tokens(&self, id: &str) -> bool {
        !self.read_token(id, "MUSIC_TOKEN").is_empty() && !self.read_token(id, "DEV_TOKEN").is_empty()
    }

    pub fn load(&self) -> Vec<AccountRecord> {
        let path = self.registry_path();
        let Ok(content) = fs::read(&path) else {
            return Vec::new();
        };
        match serde_json::from_slice::<Vec<AccountRecord>>(&content) {
            Ok(list) => list.into_iter().filter(|a| is_valid_id(&a.id)).collect(),
            Err(e) => {
                // A corrupt registry must not block startup; keep a copy.
                tracing::warn!("accounts.json is corrupt ({e}); backing it up and starting empty");
                let _ = fs::rename(&path, path.with_extension("json.corrupt"));
                Vec::new()
            }
        }
    }

    pub fn save(&self, accounts: &[AccountRecord]) -> io::Result<()> {
        fs::create_dir_all(&self.state_dir)?;
        let path = self.registry_path();
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec_pretty(accounts)?)?;
        fs::rename(&tmp, &path)
    }

    /// Moves a pre-multi-account install (tokens directly in `<state>/`) into
    /// `accounts/<id>/` and returns its record. The username is unknown, so a
    /// placeholder is used until the account is logged in again.
    pub fn migrate_legacy(&self, now: i64) -> io::Result<Option<AccountRecord>> {
        let root = &self.state_dir;
        let has_legacy = ["MUSIC_TOKEN", "token_cache.json"]
            .iter()
            .any(|f| fs::metadata(root.join(f)).map(|m| m.len() > 0).unwrap_or(false));
        if !has_legacy {
            return Ok(None);
        }
        let username = "legacy-account".to_string();
        let id = account_id(&username);
        let dest = self.account_dir(&id);
        fs::create_dir_all(&dest)?;
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if LEGACY_KEEP.contains(&name.as_ref()) || name.starts_with('.') {
                continue;
            }
            move_path(&entry.path(), &dest.join(name.as_ref()))?;
        }
        let _ = fs::remove_file(root.join("2fa.txt"));
        Ok(Some(AccountRecord {
            id,
            username,
            region: String::new(),
            enabled: true,
            device_info: None,
            created_at: now,
            owner_key_hash: None,
        }))
    }
}

/// Entries of a legacy `/data` that are not account state.
const LEGACY_KEEP: &[&str] = &[
    "accounts",
    "accounts.json",
    "accounts.json.tmp",
    "accounts.json.corrupt",
    // Android app data dir shared by every instance (ANDROID_DATA=/data).
    "data",
    "lost+found",
    "2fa.txt",
    "agent.json",
    "agent.json.tmp",
    "stats.json",
    "stats.json.tmp",
];

fn move_path(from: &Path, to: &Path) -> io::Result<()> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    // Cross-device or similar: copy then delete.
    if from.is_dir() {
        fs::create_dir_all(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            move_path(&entry.path(), &to.join(entry.file_name()))?;
        }
        fs::remove_dir(from)
    } else {
        fs::copy(from, to)?;
        fs::remove_file(from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_layout(name: &str) -> Layout {
        let dir = std::env::temp_dir().join(format!("wrapper-agent-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Layout { state_dir: dir, lite_root: "/data".into() }
    }

    #[test]
    fn ids_are_stable_and_case_insensitive() {
        assert_eq!(account_id("User@Example.com "), account_id("user@example.com"));
        assert_ne!(account_id("a@b.c"), account_id("b@b.c"));
        assert!(is_valid_id(&account_id("x")));
        assert!(!is_valid_id("../etc"));
    }

    #[test]
    fn lite_paths() {
        let l = Layout { state_dir: "/tmp/x".into(), lite_root: "/data/".into() };
        assert_eq!(l.lite_base_dir("abc"), "/data/accounts/abc");
    }

    #[test]
    fn save_load_roundtrip() {
        let l = temp_layout("roundtrip");
        let rec = AccountRecord {
            id: account_id("a@b.c"),
            username: "a@b.c".into(),
            region: "jp".into(),
            enabled: true,
            created_at: 1,
            ..Default::default()
        };
        l.save(std::slice::from_ref(&rec)).unwrap();
        assert_eq!(l.load(), vec![rec]);
        fs::write(l.registry_path(), b"{not json").unwrap();
        assert!(l.load().is_empty());
        assert!(l.state_dir.join("accounts.json.corrupt").exists());
        let _ = fs::remove_dir_all(&l.state_dir);
    }

    #[test]
    fn migrates_legacy_layout() {
        let l = temp_layout("legacy");
        let root = &l.state_dir;
        fs::write(root.join("MUSIC_TOKEN"), "m").unwrap();
        fs::write(root.join("STOREFRONT_ID"), "143462-1,31").unwrap();
        fs::create_dir_all(root.join("mpl_db")).unwrap();
        fs::write(root.join("mpl_db/db"), "x").unwrap();
        fs::create_dir_all(root.join("data/com.apple.android.music")).unwrap();

        let rec = l.migrate_legacy(5).unwrap().expect("migrated");
        let dir = l.account_dir(&rec.id);
        assert!(dir.join("MUSIC_TOKEN").exists());
        assert!(dir.join("mpl_db/db").exists());
        assert!(!root.join("MUSIC_TOKEN").exists());
        assert!(root.join("data/com.apple.android.music").exists());
        assert!(l.migrate_legacy(6).unwrap().is_none());
        let _ = fs::remove_dir_all(root);
    }
}
