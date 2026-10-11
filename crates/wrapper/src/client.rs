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
