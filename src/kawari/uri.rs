use serde::{Deserialize, Serialize};
use url::Url;

pub const DEFAULT_SIMULATOR_PORT: u16 = 21064;

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(default)]
pub struct KawariAutoConfig {
    pub game_patch_server: String,
    pub boot_patch_server: String,
    pub login_server: String,
    pub lobby_server: String,
    pub lobby_port: u16,
    pub lodestone_server: String,
    pub frontier_server: String,
    pub savedata_bank_server: String,
    pub savedata_bank_port: String,
    pub datacenter_travel_server: String,
    pub simulator_server: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedEndpoints {
    pub login_server: String,
    pub lobby_host: String,
    pub lobby_port: u16,
    pub frontier_host_argument: String,
    pub simulator_server: String,
}

/// Normalizes a user-entered Kawari server URL into a clean HTTP(S) root URL without userinfo or path.
pub fn normalize_server_url(value: &str) -> Result<Url, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("Server URL is required.".to_string());
    }

    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        if has_disallowed_scheme_prefix(trimmed) {
            return Err("Only HTTP and HTTPS Kawari server URLs are supported.".to_string());
        }
        format!("http://{trimmed}")
    };

    let mut url = Url::parse(&candidate).map_err(|_| format!("Invalid server URL: {value}"))?;
    validate_http_url(&url)?;
    strip_to_root(&mut url);
    Ok(url)
}

pub fn has_disallowed_scheme_prefix(trimmed: &str) -> bool {
    if trimmed.contains("://") || trimmed.starts_with('[') {
        return false;
    }
    let Some(colon_idx) = trimmed.find(':') else {
        return false;
    };
    let after_colon = &trimmed[colon_idx + 1..];
    let stop_idx = after_colon
        .find(['/', '?', '#'])
        .unwrap_or(after_colon.len());
    let port_candidate = &after_colon[..stop_idx];
    port_candidate.is_empty() || port_candidate.parse::<u16>().is_err()
}

pub fn resolve_simulator_fallback(entered_web_server: &Url) -> Url {
    let mut url = entered_web_server.clone();
    strip_to_root(&mut url);
    let _ = url.set_port(Some(DEFAULT_SIMULATOR_PORT));
    url
}

pub fn resolve_endpoints(
    config: &KawariAutoConfig,
    entered_web_server: &Url,
) -> Result<ResolvedEndpoints, String> {
    let login_uri = rewrite_localhost_endpoint(&to_uri(&config.login_server)?, entered_web_server);
    let frontier_uri =
        rewrite_localhost_endpoint(&to_uri(&config.frontier_server)?, entered_web_server);
    let lobby_host = resolve_host(&config.lobby_server, entered_web_server);

    let simulator_uri = match config.simulator_server.as_deref() {
        Some(raw) if !raw.trim().is_empty() => {
            rewrite_localhost_endpoint(&to_uri(raw)?, entered_web_server)
        }
        _ => resolve_simulator_fallback(entered_web_server),
    };

    Ok(ResolvedEndpoints {
        login_server: login_uri.to_string(),
        lobby_host,
        lobby_port: config.lobby_port,
        frontier_host_argument: to_host_port_argument(&frontier_uri),
        simulator_server: simulator_uri.to_string(),
    })
}

pub fn rewrite_localhost_endpoint(endpoint: &Url, entered_web_server: &Url) -> Url {
    let mut out = endpoint.clone();
    if let (Some(ep_host), Some(entered_host)) =
        (endpoint.host_str(), entered_web_server.host_str())
        && should_rewrite_host(ep_host)
    {
        let _ = out.set_host(Some(entered_host));
    }
    strip_to_root(&mut out);
    out
}

fn resolve_host(value: &str, entered_web_server: &Url) -> String {
    let entered_host = entered_web_server.host_str().unwrap_or("127.0.0.1");
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return entered_host.to_string();
    }

    if trimmed.contains("://")
        && let Ok(uri) = Url::parse(trimmed)
        && validate_http_url(&uri).is_ok()
    {
        let rewritten = rewrite_localhost_endpoint(&uri, entered_web_server);
        return rewritten.host_str().unwrap_or(entered_host).to_string();
    }

    if !trimmed.contains("://")
        && !has_disallowed_scheme_prefix(trimmed)
        && trimmed.contains(':')
        && let Ok(host_port_uri) = Url::parse(&format!("http://{trimmed}"))
        && host_port_uri.has_host()
    {
        let rewritten = rewrite_localhost_endpoint(&host_port_uri, entered_web_server);
        return rewritten.host_str().unwrap_or(entered_host).to_string();
    }

    if should_rewrite_host(trimmed) {
        entered_host.to_string()
    } else {
        trimmed.to_string()
    }
}

fn to_uri(value: &str) -> Result<Url, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("Endpoint URI is required.".to_string());
    }

    if trimmed.contains("://") {
        let mut uri = Url::parse(trimmed).map_err(|_| format!("Invalid endpoint URI: {value}"))?;
        validate_http_url(&uri)?;
        strip_to_root(&mut uri);
        return Ok(uri);
    }

    if !has_disallowed_scheme_prefix(trimmed) {
        let mut uri = Url::parse(&format!("http://{trimmed}"))
            .map_err(|_| format!("Invalid endpoint URI: {value}"))?;
        if uri.has_host() {
            strip_to_root(&mut uri);
            return Ok(uri);
        }
    }

    Err(format!("Invalid endpoint URI: {value}"))
}

fn validate_http_url(url: &Url) -> Result<(), String> {
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err("Only HTTP and HTTPS Kawari server URLs are supported.".to_string());
    }
    if !url.has_host() {
        return Err(format!("Server URL is missing host: {url}"));
    }
    Ok(())
}

fn strip_to_root(url: &mut Url) {
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
}

fn should_rewrite_host(host: &str) -> bool {
    let lower = host.to_ascii_lowercase();
    lower == "localhost" || lower.ends_with(".localhost")
}

fn to_host_port_argument(endpoint: &Url) -> String {
    let host = endpoint.host_str().unwrap_or_default();
    match endpoint.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_bare_host_and_strips_userinfo_and_path() {
        assert_eq!(
            normalize_server_url("127.0.0.1:21062").unwrap().as_str(),
            "http://127.0.0.1:21062/"
        );
        assert_eq!(
            normalize_server_url("https://alice:secret@example.com:8443/setup?a=1#frag")
                .unwrap()
                .as_str(),
            "https://example.com:8443/"
        );
        assert!(normalize_server_url("mailto:user@example.com").is_err());
        assert!(normalize_server_url("file:///C:/test.json").is_err());
        assert!(normalize_server_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn rewrites_localhost_and_supports_schemeless_and_fallback_simulator() {
        let entered = normalize_server_url("192.168.0.42:21062").unwrap();
        let cfg = KawariAutoConfig {
            login_server: "localhost:21060".to_string(),
            frontier_server: "frontier.localhost:21058".to_string(),
            lobby_server: "lobby.localhost".to_string(),
            lobby_port: 21059,
            simulator_server: Some("http://user:pw@ffxiv.localhost:21064/ignored".to_string()),
            ..Default::default()
        };

        let resolved = resolve_endpoints(&cfg, &entered).unwrap();
        assert_eq!(resolved.login_server, "http://192.168.0.42:21060/");
        assert_eq!(resolved.frontier_host_argument, "192.168.0.42:21058");
        assert_eq!(resolved.lobby_host, "192.168.0.42");
        assert_eq!(resolved.lobby_port, 21059);
        assert_eq!(resolved.simulator_server, "http://192.168.0.42:21064/");

        // Missing simulator_server preserves HTTPS scheme in fallback.
        let https_entered = normalize_server_url("https://sim.example.org:21062").unwrap();
        let cfg_no_sim = KawariAutoConfig {
            simulator_server: None,
            ..cfg
        };
        let resolved_https = resolve_endpoints(&cfg_no_sim, &https_entered).unwrap();
        assert_eq!(
            resolved_https.simulator_server,
            "https://sim.example.org:21064/"
        );

        // Opaque scheme in simulator_server is rejected.
        let cfg_bad_sim = KawariAutoConfig {
            simulator_server: Some("javascript:alert(1)".to_string()),
            ..cfg_no_sim
        };
        assert!(resolve_endpoints(&cfg_bad_sim, &entered).is_err());
    }
}
