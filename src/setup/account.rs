//! Registers the fixed single-user simulator account on the local Foundation login server.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::Client;
use reqwest::redirect::Policy;
use serde::Serialize;
use url::Url;

use crate::kawari::client::login;
use crate::server::resolve_foundation_root;
use crate::settings::{StudioSettings, apply_local_account_defaults, save_settings};
use crate::setup::config_yaml::read_login_server_url;

/// Location the Foundation login server redirects to after a successful `registlist` POST.
const REGISTER_SUCCESS_LOCATION: &str = "/account/app/svc/manage";

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LocalAccountResult {
    pub registered: bool,
    pub already_done: bool,
    pub username: String,
    pub login_server: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalAccountEvent {
    pub ok: bool,
    pub registered: bool,
    pub already_done: bool,
    pub username: String,
    pub error: Option<String>,
}

impl LocalAccountEvent {
    pub fn from_result(res: &Result<LocalAccountResult, String>, username: &str) -> Self {
        match res {
            Ok(r) => Self {
                ok: true,
                registered: r.registered,
                already_done: r.already_done,
                username: r.username.clone(),
                error: None,
            },
            Err(e) => Self {
                ok: false,
                registered: false,
                already_done: false,
                username: username.to_string(),
                error: Some(e.clone()),
            },
        }
    }
}

pub fn registration_key(foundation_root: &str, username: &str) -> String {
    format!("{}|{}", foundation_root.trim(), username.trim())
}

fn local_http_client() -> Client {
    Client::builder()
        .user_agent(concat!("Foundation-Studio/", env!("CARGO_PKG_VERSION")))
        .redirect(Policy::none())
        .no_proxy()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap_or_else(|_| Client::new())
}

fn base_url_with_slash(raw: &str) -> Result<Url, String> {
    let trimmed = raw.trim().trim_end_matches('/');
    Url::parse(&format!("{trimmed}/")).map_err(|e| format!("Invalid login server URL '{raw}': {e}"))
}

/// Snapshot of what the account routine needs from the shared settings.
pub struct AccountContext {
    pub username: String,
    pub password: String,
    pub root_key: String,
    pub login_server: String,
    pub registered_for: Option<String>,
}

pub fn account_context(settings: &StudioSettings) -> AccountContext {
    let mut s = settings.clone();
    apply_local_account_defaults(&mut s);
    let resolved = resolve_foundation_root(&s.foundation_root);
    let root_key = resolved
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| s.foundation_root.trim().to_string());
    AccountContext {
        username: s.username.trim().to_string(),
        password: s.password.clone(),
        login_server: read_login_server_url(resolved.as_deref()),
        root_key,
        registered_for: s.local_account_registered_for.clone(),
    }
}

/// Ensures the configured (default `1`/`2`) account exists on the Foundation login server.
/// Attempted once per Foundation root unless `force` is set.
pub async fn ensure_local_account(
    settings: &Arc<Mutex<StudioSettings>>,
    force: bool,
) -> Result<LocalAccountResult, String> {
    let ctx = {
        let guard = settings
            .lock()
            .map_err(|_| "Failed to lock settings".to_string())?;
        account_context(&guard)
    };
    let key = registration_key(&ctx.root_key, &ctx.username);
    if !force && ctx.registered_for.as_deref() == Some(key.as_str()) {
        return Ok(LocalAccountResult {
            registered: false,
            already_done: true,
            username: ctx.username,
            login_server: ctx.login_server,
        });
    }

    let client = local_http_client();
    let base = base_url_with_slash(&ctx.login_server)?;
    let registered = register_or_verify(&client, &base, &ctx.username, &ctx.password).await?;

    if let Ok(mut guard) = settings.lock() {
        guard.local_account_registered_for = Some(key);
        save_settings(&guard)?;
    }

    Ok(LocalAccountResult {
        registered,
        already_done: !registered,
        username: ctx.username,
        login_server: ctx.login_server,
    })
}

/// Returns `Ok(true)` when the account was newly registered, `Ok(false)` if it already worked.
async fn register_or_verify(
    client: &Client,
    base: &Url,
    username: &str,
    password: &str,
) -> Result<bool, String> {
    // Foundation's `do_register` panics when the username exists with a different password, so
    // probe with a normal login first and only register when the username is unknown.
    match login(client, base, username, password).await {
        Ok(res) if res.success => return Ok(false),
        Ok(res) => {
            let err = res.error.unwrap_or_default();
            if err.to_ascii_lowercase().contains("password") {
                return Err(format!(
                    "Local account '{username}' already exists on the login server with a different password."
                ));
            }
        }
        Err(err) => {
            return Err(format!("Login server is not reachable: {err}"));
        }
    }

    let url = base
        .join("oauth/oa/registlist")
        .map_err(|e| format!("Invalid registration URL: {e}"))?;
    let resp = client
        .post(url.clone())
        .form(&[("username", username), ("password", password)])
        .send()
        .await
        .map_err(|e| format!("Account registration request failed ({url}): {e}"))?;
    let status = resp.status();
    if status.is_redirection() {
        let location = resp
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        if !location.contains(REGISTER_SUCCESS_LOCATION) {
            return Err(
                "Account registration is disabled on the Foundation login server (login.enable_registration)."
                    .to_string(),
            );
        }
    } else if !status.is_success() {
        return Err(format!("Account registration failed with HTTP {status}"));
    }

    match login(client, base, username, password).await {
        Ok(res) if res.success => Ok(true),
        Ok(res) => Err(format!(
            "Account registration did not take effect: {}",
            res.error.unwrap_or_else(|| "login failed".to_string())
        )),
        Err(err) => Err(format!("Account verification failed: {err}")),
    }
}

/// Polls the login server TCP port until it accepts connections or `timeout` elapses.
pub async fn wait_for_login_port(login_server: &str, timeout: Duration) -> bool {
    let Ok(url) = base_url_with_slash(login_server) else {
        return false;
    };
    let Some(host) = url.host_str().map(str::to_string) else {
        return false;
    };
    let port = url.port_or_known_default().unwrap_or(21060);
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let attempt = tokio::time::timeout(
            Duration::from_millis(800),
            tokio::net::TcpStream::connect((host.as_str(), port)),
        )
        .await;
        if matches!(attempt, Ok(Ok(_))) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_key_and_base_url_normalization() {
        assert_eq!(registration_key(" C:\\F ", " 1 "), "C:\\F|1");
        let base = base_url_with_slash("http://127.0.0.1:21060").unwrap();
        assert_eq!(
            base.join("oauth/oa/registlist").unwrap().as_str(),
            "http://127.0.0.1:21060/oauth/oa/registlist"
        );
    }

    #[test]
    fn context_defaults_to_fixed_account() {
        let s = StudioSettings {
            username: String::new(),
            password: String::new(),
            foundation_root: String::new(),
            ..Default::default()
        };
        let ctx = account_context(&s);
        assert_eq!(ctx.username, "1");
        assert_eq!(ctx.password, "2");
    }
}
