//! Persistent GUI settings (`<data dir>/wrapper-gui.json`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EngineKind {
    /// Boot the agent in a QEMU guest (every OS).
    Qemu,
    /// Run the agent natively with the user-namespace sandbox (Linux x86_64).
    Native,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct GuiConfig {
    pub engine: EngineKind,
    /// Address the public (wrapper-lite compatible) API binds to.
    pub api_host: String,
    pub api_port: u16,
    /// Token protecting the agent's management API. Generated on first run;
    /// the GUI sends it for you.
    pub api_token: String,
    pub memory_mb: u32,
    pub smp: u32,
    /// `auto`, `kvm`, `whpx`, `hvf` or `tcg`.
    pub accel: String,
    pub disk_size_mb: u64,
    pub proxy: String,
    pub device_info: String,
    pub lite_log_level: String,
    pub token_refresh_interval: u32,
    pub instance_concurrency: usize,
    /// Start the engine when the GUI starts.
    pub autostart: bool,
    /// Download packages through gh-proxy (for users in mainland China).
    pub mirror: bool,
    pub package_base_url: String,
}

pub const DEFAULT_PACKAGE_BASE: &str = "https://nightly.link/itouakirai/wrapper/workflows/build-lite/lite";

impl Default for GuiConfig {
    fn default() -> Self {
        Self {
            engine: if native_supported() { EngineKind::Native } else { EngineKind::Qemu },
            api_host: "127.0.0.1".into(),
            api_port: 12340,
            api_token: String::new(),
            memory_mb: 1024,
            smp: 2,
            accel: "auto".into(),
            disk_size_mb: 512,
            proxy: String::new(),
            device_info: String::new(),
            lite_log_level: "info".into(),
            token_refresh_interval: 1800,
            instance_concurrency: 2,
            autostart: false,
            mirror: false,
            package_base_url: DEFAULT_PACKAGE_BASE.into(),
        }
    }
}

/// The native engine needs Linux on x86_64 (lite is an x86_64 Android binary).
pub fn native_supported() -> bool {
    cfg!(all(target_os = "linux", target_arch = "x86_64"))
}

pub fn random_token() -> String {
    format!("{:032x}", rand::random::<u128>())
}

impl GuiConfig {
    pub fn load(path: &Path) -> Self {
        let mut cfg: GuiConfig = std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        if cfg.api_token.is_empty() {
            cfg.api_token = random_token();
            let _ = cfg.save(path);
        }
        cfg
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, path)
    }

    /// Rejects settings that cannot work, with a message for the UI.
    pub fn validate(&self) -> Result<(), String> {
        if self.engine == EngineKind::Native && !native_supported() {
            return Err("the native engine requires Linux x86_64".into());
        }
        if self.api_port == 0 {
            return Err("invalid API port".into());
        }
        if self.api_host.parse::<std::net::IpAddr>().is_err() {
            return Err(format!("invalid API listen address {:?}", self.api_host));
        }
        if !(256..=65536).contains(&self.memory_mb) {
            return Err("memory must be between 256 and 65536 MB".into());
        }
        if !(1..=64).contains(&self.smp) {
            return Err("CPU count must be between 1 and 64".into());
        }
        if !["auto", "kvm", "whpx", "hvf", "tcg"].contains(&self.accel.as_str()) {
            return Err(format!("unknown accelerator {:?}", self.accel));
        }
        for (name, v) in [("proxy", &self.proxy), ("device info", &self.device_info), ("API token", &self.api_token)] {
            if v.contains('\n') {
                return Err(format!("{name} must not contain line breaks"));
            }
        }
        Ok(())
    }

    /// Arguments shared by the native and the guest agent.
    pub fn agent_args(&self) -> Vec<String> {
        let mut a = vec![
            "--lite-log-level".to_string(),
            self.lite_log_level.clone(),
            "--token-refresh-interval".into(),
            self.token_refresh_interval.to_string(),
            "--instance-concurrency".into(),
            self.instance_concurrency.to_string(),
        ];
        if !self.proxy.is_empty() {
            a.extend(["--proxy".into(), self.proxy.clone()]);
        }
        if !self.device_info.is_empty() {
            a.extend(["--device-info".into(), self.device_info.clone()]);
        }
        a
    }

    /// URL to reach the public API from this machine.
    pub fn api_url(&self) -> String {
        let host = match self.api_host.as_str() {
            "0.0.0.0" | "" => "127.0.0.1",
            "::" => "[::1]",
            h => h,
        };
        format!("http://{host}:{}", self.api_port)
    }
}

pub fn config_path(data_dir: &Path) -> PathBuf {
    data_dir.join("wrapper-gui.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_hex() {
        let a = random_token();
        let b = random_token();
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn validation() {
        let mut c = GuiConfig { engine: EngineKind::Qemu, ..Default::default() };
        assert!(c.validate().is_ok());
        c.api_host = "localhost".into();
        assert!(c.validate().is_err());
        c.api_host = "0.0.0.0".into();
        assert_eq!(c.api_url(), "http://127.0.0.1:12340");
        c.accel = "fast".into();
        assert!(c.validate().is_err());
    }

    #[test]
    fn partial_json_uses_defaults() {
        let c: GuiConfig = serde_json::from_str(r#"{"apiPort": 9000}"#).unwrap();
        assert_eq!(c.api_port, 9000);
        assert_eq!(c.smp, 2);
    }
}
