//! Minimal line-based helpers for reading the Foundation `config.yaml`.
//!
//! Foundation's `core/src/config.rs` defines a `listen_address` (default `0.0.0.0`) for each of
//! the server sections below. Studio generates configs that bind every one of them to loopback.

use std::fs;
use std::net::IpAddr;
use std::path::Path;
use std::str::FromStr;

/// Every Foundation config section that exposes a `listen_address` field.
pub const LISTEN_SECTIONS: [&str; 7] = [
    "admin", "frontier", "lobby", "login", "patch", "web", "world",
];

pub const DEFAULT_LOGIN_SERVER_URL: &str = "http://127.0.0.1:21060";

/// Returns the scalar value of `section.key` from a simple two-level YAML document.
pub fn yaml_section_value(text: &str, section: &str, key: &str) -> Option<String> {
    let mut current: Option<String> = None;
    for raw_line in text.lines() {
        let line = raw_line.trim_end();
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let indented = line.len() != trimmed.len();
        if !indented {
            current = trimmed
                .split_once(':')
                .map(|(k, _)| k.trim().trim_matches(['"', '\'']).to_string());
            continue;
        }
        if current.as_deref() != Some(section) {
            continue;
        }
        let Some((k, v)) = trimmed.split_once(':') else {
            continue;
        };
        if k.trim().trim_matches(['"', '\'']) != key {
            continue;
        }
        return Some(clean_scalar(v));
    }
    None
}

fn clean_scalar(raw: &str) -> String {
    let v = raw.trim();
    if let Some(rest) = v.strip_prefix('"')
        && let Some(end) = rest.find('"')
    {
        return rest[..end].to_string();
    }
    if let Some(rest) = v.strip_prefix('\'')
        && let Some(end) = rest.find('\'')
    {
        return rest[..end].to_string();
    }
    let without_comment = match v.find(" #") {
        Some(idx) => &v[..idx],
        None => v,
    };
    without_comment.trim().to_string()
}

fn is_loopback_literal(value: &str) -> bool {
    IpAddr::from_str(value.trim()).is_ok_and(|ip| ip.is_loopback())
}

/// True when every server section explicitly binds a loopback `listen_address`.
/// A missing key means Foundation falls back to `0.0.0.0`, so it counts as not localhost-only.
pub fn config_text_is_localhost_only(text: &str) -> bool {
    LISTEN_SECTIONS.iter().all(|section| {
        yaml_section_value(text, section, "listen_address")
            .is_some_and(|value| is_loopback_literal(&value))
    })
}

pub fn config_file_is_localhost_only(config_path: &Path) -> bool {
    fs::read_to_string(config_path)
        .map(|text| config_text_is_localhost_only(&text))
        .unwrap_or(false)
}

/// Resolves the login server base URL from `<root>/config.yaml` (`login.server_name`).
pub fn read_login_server_url(foundation_root: Option<&Path>) -> String {
    foundation_root
        .and_then(|root| fs::read_to_string(root.join("config.yaml")).ok())
        .and_then(|text| yaml_section_value(&text, "login", "server_name"))
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| v.starts_with("http://") || v.starts_with("https://"))
        .unwrap_or_else(|| DEFAULT_LOGIN_SERVER_URL.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_localhost_only_configs() {
        let mut full = String::from("filesystem:\n  game_path: \"C:\\\\game\"\n");
        for s in LISTEN_SECTIONS {
            full.push_str(&format!(
                "{s}:\n  listen_address: \"127.0.0.1\" # loopback\n  server_name: x\n"
            ));
        }
        assert!(config_text_is_localhost_only(&full));

        let public = full.replacen(
            "listen_address: \"127.0.0.1\"",
            "listen_address: 0.0.0.0",
            1,
        );
        assert!(!config_text_is_localhost_only(&public));

        let missing_lobby =
            full.replace("lobby:\n  listen_address: \"127.0.0.1\" # loopback\n", "");
        assert!(!config_text_is_localhost_only(&missing_lobby));

        assert!(!config_text_is_localhost_only("custom: keep\n"));
        assert_eq!(
            yaml_section_value(&full, "login", "server_name").as_deref(),
            Some("x")
        );
    }

    #[test]
    fn login_url_falls_back_when_config_missing() {
        assert_eq!(read_login_server_url(None), DEFAULT_LOGIN_SERVER_URL);
        let text = "login:\n  server_name: 'http://127.0.0.1:31060/'\n";
        assert_eq!(
            yaml_section_value(text, "login", "server_name").as_deref(),
            Some("http://127.0.0.1:31060/")
        );
    }
}
