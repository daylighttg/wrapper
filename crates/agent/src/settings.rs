//! Agent settings changed at runtime over the API (`<state>/agent.json`):
//! public mode and its limits. They live next to the accounts, so they travel
//! with a backup and apply to the QEMU guest without a reboot.

use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Longest public notice the operator can set.
pub const MAX_NOTICE: usize = 2000;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentSettings {
    /// Anyone may add (and later remove) their own account through
    /// `/api/public/*`.
    pub public_mode: bool,
    /// Most accounts that may be active (enabled and logged in) for a public
    /// login to be accepted; 0 = no limit. Each one is a lite process, so this
    /// is what keeps a small machine from running out of memory.
    pub max_accounts: u32,
    /// Plain text shown on the public page.
    pub public_notice: String,
    /// Random salt for the public account handles ([`public_ref`]).
    pub public_salt: String,
}

/// Fields of [`AgentSettings`] the API may change.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
    pub public_mode: Option<bool>,
    pub max_accounts: Option<u32>,
    pub public_notice: Option<String>,
}

impl AgentSettings {
    /// Loads the settings, creating the salt on first use.
    pub fn load(path: &Path) -> Self {
        let mut s: AgentSettings = std::fs::read(path)
            .ok()
            .and_then(|b| match serde_json::from_slice(&b) {
                Ok(s) => Some(s),
                Err(e) => {
                    tracing::warn!("{} is invalid ({e}); using defaults", path.display());
                    None
                }
            })
            .unwrap_or_default();
        if s.public_salt.is_empty() {
            s.public_salt = random_hex(16);
            if let Err(e) = s.save(path) {
                tracing::debug!("cannot save {}: {e}", path.display());
            }
        }
        s
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)
    }

    pub fn apply(&mut self, patch: SettingsPatch) -> Result<(), String> {
        if let Some(notice) = patch.public_notice {
            if notice.chars().count() > MAX_NOTICE {
                return Err(format!("the notice must be at most {MAX_NOTICE} characters"));
            }
            self.public_notice = notice.trim().to_string();
        }
        if let Some(v) = patch.public_mode {
            self.public_mode = v;
        }
        if let Some(v) = patch.max_accounts {
            self.max_accounts = v;
        }
        Ok(())
    }
}

pub fn random_hex(bytes: usize) -> String {
    (0..bytes).map(|_| format!("{:02x}", rand::random::<u8>())).collect()
}

pub fn sha256_hex(data: &str) -> String {
    Sha256::digest(data.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Opaque, stable handle of an account on the public page. Unlike the
/// account id (derived from the Apple ID alone) it cannot be checked against
/// a guessed Apple ID without the salt.
pub fn public_ref(salt: &str, id: &str) -> String {
    sha256_hex(&format!("{salt}:{id}"))[..12].to_string()
}

/// `john.doe@gmail.com` -> `jo***@g***.com`, `+8613812345678` -> `***78`.
pub fn mask_username(username: &str) -> String {
    let keep = |s: &str, n: usize| s.chars().take(n).collect::<String>();
    match username.split_once('@') {
        Some((local, domain)) => {
            let local = if local.chars().count() <= 2 { format!("{}*", keep(local, 1)) } else { format!("{}***", keep(local, 2)) };
            let domain = match domain.rsplit_once('.') {
                Some((name, tld)) => format!("{}***.{tld}", keep(name, 1)),
                None => format!("{}***", keep(domain, 1)),
            };
            format!("{local}@{domain}")
        }
        None => {
            let chars: Vec<char> = username.chars().collect();
            let tail: String = chars[chars.len().saturating_sub(2)..].iter().collect();
            format!("***{tail}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_usernames() {
        assert_eq!(mask_username("john.doe@gmail.com"), "jo***@g***.com");
        assert_eq!(mask_username("ab@x.co.jp"), "a*@x***.jp");
        assert_eq!(mask_username("a@localhost"), "a*@l***");
        assert_eq!(mask_username("+8613812345678"), "***78");
        assert_eq!(mask_username("x"), "***x");
    }

    #[test]
    fn refs_depend_on_the_salt() {
        assert_eq!(public_ref("s", "id").len(), 12);
        assert_eq!(public_ref("s", "id"), public_ref("s", "id"));
        assert_ne!(public_ref("s", "id"), public_ref("t", "id"));
    }

    #[test]
    fn load_creates_a_salt_and_patch_validates() {
        let dir = std::env::temp_dir().join(format!("wrapper-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("agent.json");
        let s = AgentSettings::load(&path);
        assert_eq!(s.public_salt.len(), 32);
        assert_eq!(AgentSettings::load(&path).public_salt, s.public_salt);

        let mut s = s;
        s.apply(SettingsPatch { public_mode: Some(true), max_accounts: Some(3), ..Default::default() }).unwrap();
        assert!(s.public_mode);
        assert_eq!(s.max_accounts, 3);
        let long = "x".repeat(MAX_NOTICE + 1);
        assert!(s.apply(SettingsPatch { public_notice: Some(long), ..Default::default() }).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
