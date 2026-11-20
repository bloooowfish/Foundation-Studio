use reqwest::Client;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::kawari::uri::KawariAutoConfig;
use crate::settings::DAWNTRAIL_EXPANSION_ID;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LoginResult {
    pub success: bool,
    pub session_id: Option<String>,
    pub max_expansion: i32,
    pub error: Option<String>,
}

pub async fn fetch_auto_config(
    client: &Client,
    web_server: &Url,
) -> Result<KawariAutoConfig, String> {
    let url = web_server
        .join(".well-known/xiv")
        .map_err(|e| format!("Invalid autoconfig URL: {e}"))?;
    let resp = client
        .get(url.clone())
        .send()
        .await
        .map_err(|e| format!("Failed connecting to {url}: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(format!("Autoconfig HTTP {status}: {text}"));
    }
    resp.json::<KawariAutoConfig>()
        .await
        .map_err(|e| format!("Invalid Kawari autoconfig response: {e}"))
}

pub async fn login(
    client: &Client,
    login_server: &Url,
    username: &str,
    password: &str,
) -> Result<LoginResult, String> {
    let url = login_server
        .join("oauth/ffxivarr/login/login.send")
        .map_err(|e| format!("Invalid login URL: {e}"))?;
    let resp = client
        .post(url.clone())
        .form(&[
            ("sqexid", username),
            ("password", password),
            ("otppw", ""),
            ("_STORED_", "34657"),
        ])
        .send()
        .await
        .map_err(|e| format!("Login request failed ({url}): {e}"))?;

    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| format!("Failed reading login response: {e}"))?;
    if !status.is_success() {
        return Err(format!("Login HTTP {status}: {body}"));
    }
    Ok(parse_login_response(&body))
}

pub fn parse_login_response(response: &str) -> LoginResult {
    if response.trim().is_empty() {
        return LoginResult {
            success: false,
            session_id: None,
            max_expansion: DAWNTRAIL_EXPANSION_ID,
            error: Some("Empty login response".to_string()),
        };
    }

    let payload = extract_window_external_user_payload(response);
    let tokens: Vec<&str> = payload.split(',').collect();
    if tokens.len() < 2 || !tokens[0].eq_ignore_ascii_case("login=auth") {
        return LoginResult {
            success: false,
            session_id: None,
            max_expansion: DAWNTRAIL_EXPANSION_ID,
            error: Some("Unexpected login response".to_string()),
        };
    }

    if tokens[1].eq_ignore_ascii_case("ok") {
        let sid = find_value(&tokens, "sid");
        if sid.is_none_or(|s| s.trim().is_empty()) {
            return LoginResult {
                success: false,
                session_id: None,
                max_expansion: DAWNTRAIL_EXPANSION_ID,
                error: Some("Login response did not include a SID".to_string()),
            };
        }
        let max_exp = find_value(&tokens, "maxex")
            .and_then(|v| v.trim().parse::<i32>().ok())
            .unwrap_or(DAWNTRAIL_EXPANSION_ID);

        return LoginResult {
            success: true,
            session_id: sid.map(ToString::to_string),
            max_expansion: max_exp,
            error: None,
        };
    }

    let error = find_value(&tokens, "err")
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("Login failed")
        .to_string();

    LoginResult {
        success: false,
        session_id: None,
        max_expansion: DAWNTRAIL_EXPANSION_ID,
        error: Some(error),
    }
}

fn extract_window_external_user_payload(response: &str) -> String {
    const MARKER: &str = "window.external.user(";
    let lower = response.to_ascii_lowercase();
    let Some(marker_pos) = lower.find(MARKER) else {
        return response.trim().to_string();
    };
    let rest = &response[marker_pos + MARKER.len()..];
    let Some(first_quote) = rest.find('"') else {
        return response.trim().to_string();
    };

    let mut payload = String::new();
    let mut escaped = false;
    for ch in rest[first_quote + 1..].chars() {
        if escaped {
            payload.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '"' {
            return payload;
        }
        payload.push(ch);
    }

    response.trim().to_string()
}

fn find_value<'a>(tokens: &'a [&'a str], key: &str) -> Option<&'a str> {
    for pair in tokens.windows(2) {
        if pair[0].eq_ignore_ascii_case(key) {
            return Some(pair[1]);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_successful_window_external_user_response() {
        let html = r#"<script>window.external.user("login=auth,ok,sid,ABC123DEF,region,3,maxex,5");</script>"#;
        let res = parse_login_response(html);
        assert!(res.success);
        assert_eq!(res.session_id.as_deref(), Some("ABC123DEF"));
        assert_eq!(res.max_expansion, 5);
        assert!(res.error.is_none());
    }

    #[test]
    fn parses_failed_window_external_user_response() {
        let html = r#"window.external.user("login=auth,ng,err,Wrong Password");"#;
        let res = parse_login_response(html);
        assert!(!res.success);
        assert!(res.session_id.is_none());
        assert_eq!(res.error.as_deref(), Some("Wrong Password"));
    }
}
