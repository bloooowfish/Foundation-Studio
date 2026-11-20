use std::fs;
use std::path::Path;

use reqwest::Method;
use serde::Serialize;

use crate::housing::HousingClientState;
use crate::presets::metadata::decode_bytes_with_bom;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpProxyResponse {
    pub status: u16,
    pub ok: bool,
    pub body: String,
}

pub async fn api_request(
    state: &HousingClientState,
    method: &str,
    path: &str,
    body: Option<String>,
) -> Result<HttpProxyResponse, String> {
    let trimmed_path = path.trim();
    if !trimmed_path.starts_with('/') {
        return Err("Housing API path must start with '/'.".to_string());
    }
    if trimmed_path.contains("://") {
        return Err("Absolute URLs are not permitted in housing proxy.".to_string());
    }

    let method_upper = method.trim().to_ascii_uppercase();
    let req_method = match method_upper.as_str() {
        "GET" => Method::GET,
        "POST" => Method::POST,
        "PATCH" => Method::PATCH,
        "DELETE" => Method::DELETE,
        "PUT" => Method::PUT,
        _ => return Err(format!("Unsupported HTTP method: {method}")),
    };

    let base = state.simulator_base_url();
    let url = format!("{base}/api/v1/housing{trimmed_path}");

    let mut req = state.http.request(req_method, &url);
    if let Some(payload) = body {
        req = req.header("Content-Type", "application/json").body(payload);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| format!("Failed to reach simulator API ({url}): {e}"))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| format!("Failed to read response body ({url}): {e}"))?;

    Ok(HttpProxyResponse {
        status: status.as_u16(),
        ok: status.is_success(),
        body: text,
    })
}

pub async fn preview_local_preset_file(
    state: &HousingClientState,
    file_path: &str,
    scope: &str,
    land_ident: Option<i32>,
) -> Result<HttpProxyResponse, String> {
    let raw_json = read_preset_file_text(Path::new(file_path))?;
    let payload = serde_json::json!({
        "raw_json": raw_json,
        "scope": scope,
        "land_ident": land_ident,
    });
    api_request(state, "POST", "/presets/preview", Some(payload.to_string())).await
}

#[allow(clippy::too_many_arguments)]
pub async fn apply_local_preset_file(
    state: &HousingClientState,
    land_ident: i32,
    file_path: &str,
    scope: &str,
    reload_mode: &str,
    preserve_pos: bool,
    force_enter: bool,
    label: Option<String>,
) -> Result<HttpProxyResponse, String> {
    let path_obj = Path::new(file_path);
    let raw_json = read_preset_file_text(path_obj)?;
    let fallback_label = path_obj
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Local Preset")
        .to_string();
    let payload = serde_json::json!({
        "raw_json": raw_json,
        "scope": scope,
        "reload_mode": reload_mode,
        "preserve_pos": preserve_pos,
        "force_enter": force_enter,
        "label": label.filter(|l| !l.trim().is_empty()).unwrap_or(fallback_label),
        "source": "desktop_ui",
    });
    api_request(
        state,
        "POST",
        &format!("/estates/{land_ident}/preset/apply"),
        Some(payload.to_string()),
    )
    .await
}

fn read_preset_file_text(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path)
        .map_err(|e| format!("Failed to read preset file ({}): {e}", path.display()))?;
    Ok(decode_bytes_with_bom(&bytes))
}
