//! `wrapper login` / `wrapper accounts`: small clients for the agent's
//! management API, for headless setups.

use std::io::{BufRead, Write};
use std::time::Duration;

use anyhow::{bail, Context};
use clap::Args;
use serde_json::{json, Value};

#[derive(Args, Clone)]
pub struct ApiArgs {
    /// Agent URL.
    #[arg(long, default_value = "http://127.0.0.1:12340", env = "WRAPPER_URL")]
    pub url: String,
    /// Management API token.
    #[arg(long, env = "WRAPPER_API_TOKEN")]
    pub token: Option<String>,
}

#[derive(Args)]
pub struct LoginArgs {
    #[command(flatten)]
    pub api: ApiArgs,
    /// Apple ID (prompted when omitted).
    #[arg(long)]
    pub username: Option<String>,
    /// Read the password from this environment variable instead of prompting.
    #[arg(long, value_name = "VAR")]
    pub password_env: Option<String>,
    /// Per-account device info override.
    #[arg(long)]
    pub device_info: Option<String>,
}

struct Api {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
}

impl Api {
    fn new(args: &ApiArgs) -> anyhow::Result<Self> {
        Ok(Self {
            // Logins wait for Apple (and possibly a slow guest).
            http: reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(600)).build()?,
            base: args.url.trim_end_matches('/').to_string(),
            token: args.token.clone().filter(|t| !t.is_empty()),
        })
    }

    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> anyhow::Result<Value> {
        let mut req = self.http.request(method, format!("{}{path}", self.base));
        if let Some(t) = &self.token {
            req = req.bearer_auth(t);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let resp = req.send().await.with_context(|| format!("cannot reach the agent at {}", self.base))?;
        let v: Value = resp.json().await.context("invalid response from the agent")?;
        if v["code"].as_i64() != Some(0) {
            bail!("{}", v["msg"].as_str().unwrap_or("request failed"));
        }
        Ok(v["data"].clone())
    }
}

fn prompt(label: &str) -> anyhow::Result<String> {
    print!("{label}");
    std::io::stdout().flush()?;
    let mut s = String::new();
    std::io::stdin().lock().read_line(&mut s)?;
    Ok(s.trim().to_string())
}

fn print_login(v: &Value) {
    println!(
        "[{}] {}",
        v["state"].as_str().unwrap_or("?"),
        v["message"].as_str().unwrap_or("")
    );
}

pub async fn login(args: LoginArgs) -> anyhow::Result<()> {
    let api = Api::new(&args.api)?;
    let username = match args.username {
        Some(u) => u,
        None => prompt("Apple ID: ")?,
    };
    let password = match &args.password_env {
        Some(var) => std::env::var(var).with_context(|| format!("environment variable {var} is not set"))?,
        None => rpassword::prompt_password("Password: ")?,
    };
    let mut body = json!({ "username": username, "password": password });
    if let Some(d) = args.device_info {
        body["deviceInfo"] = json!(d);
    }
    let mut v = api.call(reqwest::Method::POST, "/api/accounts/login", Some(body)).await?;
    print_login(&v);
    let id = v["loginId"].as_str().unwrap_or_default().to_string();
    loop {
        match v["state"].as_str() {
            Some("need2fa") => {
                let code = prompt("Verification code: ")?;
                match api.call(reqwest::Method::POST, &format!("/api/logins/{id}/code"), Some(json!({ "code": code }))).await {
                    Ok(next) => v = next,
                    Err(e) => {
                        eprintln!("{e}");
                        continue;
                    }
                }
                print_login(&v);
            }
            Some("running") => {
                tokio::time::sleep(Duration::from_secs(2)).await;
                v = api.call(reqwest::Method::GET, &format!("/api/logins/{id}"), None).await?;
            }
            Some("success") => return Ok(()),
            _ => bail!("login failed"),
        }
    }
}

pub async fn accounts(args: ApiArgs) -> anyhow::Result<()> {
    let api = Api::new(&args)?;
    let list = api.call(reqwest::Method::GET, "/api/accounts", None).await?;
    let list = list.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        println!("no accounts; add one with `wrapper login`");
        return Ok(());
    }
    println!("{:<36}  {:<32}  {:<6}  {:<10}  note", "id", "username", "region", "state");
    for a in list {
        println!(
            "{:<36}  {:<32}  {:<6}  {:<10}  {}",
            a["id"].as_str().unwrap_or(""),
            a["username"].as_str().unwrap_or(""),
            a["region"].as_str().unwrap_or(""),
            a["state"].as_str().unwrap_or(""),
            a["lastError"].as_str().unwrap_or("")
        );
    }
    Ok(())
}

#[derive(Args)]
pub struct BackupArgs {
    #[command(flatten)]
    pub api: ApiArgs,
    /// File to write [default: the name the server suggests, in the current directory].
    #[arg(short, long)]
    pub output: Option<std::path::PathBuf>,
}

#[derive(Args)]
pub struct RestoreArgs {
    #[command(flatten)]
    pub api: ApiArgs,
    /// Backup file made by `wrapper backup` or the panel.
    #[arg(required_unless_present = "from")]
    pub file: Option<std::path::PathBuf>,
    /// Instead of a file, copy everything from this running wrapper (its URL).
    #[arg(long, conflicts_with = "file")]
    pub from: Option<String>,
    /// Management token of the wrapper given with --from.
    #[arg(long, requires = "from")]
    pub from_token: Option<String>,
    /// Remove accounts that are not in the backup (default: merge).
    #[arg(long)]
    pub replace: bool,
    /// Also restore the public-mode settings.
    #[arg(long)]
    pub settings: bool,
}

/// Client without an overall timeout, for transfers of any size.
fn transfer_client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder().no_proxy().connect_timeout(Duration::from_secs(15)).build()?)
}

async fn error_message(resp: reqwest::Response) -> String {
    let status = resp.status();
    resp.json::<Value>()
        .await
        .ok()
        .and_then(|v| v["msg"].as_str().map(str::to_string))
        .unwrap_or_else(|| status.to_string())
}

fn print_report(r: &Value) {
    let names: Vec<&str> = r["accounts"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    println!(
        "restored {} account(s) from a backup of wrapper {}: {} added, {} replaced, {} removed",
        names.len(),
        r["backupVersion"].as_str().unwrap_or("?"),
        r["added"],
        r["replaced"],
        r["removed"]
    );
    for n in names {
        println!("  {n}");
    }
    for n in r["skipped"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        println!("  skipped: {n}");
    }
    if r["settings"].as_bool() == Some(true) {
        println!("public-mode settings restored");
    }
}

pub async fn backup(args: BackupArgs) -> anyhow::Result<()> {
    let base = args.api.url.trim_end_matches('/');
    let mut req = transfer_client()?.get(format!("{base}/api/backup"));
    if let Some(t) = args.api.token.as_deref().filter(|t| !t.is_empty()) {
        req = req.bearer_auth(t);
    }
    let mut resp = req.send().await.with_context(|| format!("cannot reach the agent at {base}"))?;
    let is_json = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("json"));
    if !resp.status().is_success() || is_json {
        bail!("{}", error_message(resp).await);
    }
    let suggested = resp
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split("filename=\"").nth(1))
        .and_then(|v| v.split('"').next())
        .filter(|n| !n.contains(['/', '\\']))
        .unwrap_or("wrapper-backup.tar.gz")
        .to_string();
    let path = args.output.unwrap_or_else(|| suggested.into());
    let tmp = path.with_extension("part");
    let mut file = std::fs::File::create(&tmp).with_context(|| format!("cannot create {}", tmp.display()))?;
    let mut size = 0u64;
    while let Some(chunk) = resp.chunk().await.context("the download was interrupted")? {
        file.write_all(&chunk)?;
        size += chunk.len() as u64;
    }
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, &path)?;
    println!("saved {} ({:.1} MB)", path.display(), size as f64 / 1_048_576.0);
    println!("it holds the accounts' login sessions: keep it private");
    Ok(())
}

pub async fn restore(args: RestoreArgs) -> anyhow::Result<()> {
    let base = args.api.url.trim_end_matches('/');
    let mode = if args.replace { "replace" } else { "merge" };
    let http = transfer_client()?;
    let mut req = match (&args.from, &args.file) {
        (Some(from), _) => http.post(format!("{base}/api/restore/pull")).json(&json!({
            "url": from,
            "token": args.from_token.clone().unwrap_or_default(),
            "mode": mode,
            "settings": args.settings,
        })),
        (None, Some(file)) => {
            let f = tokio::fs::File::open(file).await.with_context(|| format!("cannot open {}", file.display()))?;
            let q = format!("mode={mode}&settings={}", u8::from(args.settings));
            http.post(format!("{base}/api/restore?{q}"))
                .header("Content-Type", "application/gzip")
                .body(reqwest::Body::from(f))
        }
        (None, None) => bail!("give a backup file or --from <url>"),
    };
    if let Some(t) = args.api.token.as_deref().filter(|t| !t.is_empty()) {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.with_context(|| format!("cannot reach the agent at {base}"))?;
    let v: Value = resp.json().await.context("invalid response from the agent")?;
    if v["code"].as_i64() != Some(0) {
        bail!("{}", v["msg"].as_str().unwrap_or("restore failed"));
    }
    print_report(&v["data"]);
    Ok(())
}
