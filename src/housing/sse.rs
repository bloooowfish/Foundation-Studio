use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::housing::HousingClientState;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SseStatusPayload {
    pub connected: bool,
    pub url: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SseEventPayload {
    pub event: String,
    pub data: String,
}

pub fn spawn_sse_bridge(app: AppHandle, housing: Arc<HousingClientState>) {
    tauri::async_runtime::spawn(async move {
        let mut url_rx = housing.subscribe_url_watch();
        let mut last_connected: Option<bool> = None;

        loop {
            let base_url = url_rx.borrow_and_update().clone();
            let events_url = format!("{base_url}/api/v1/housing/events");

            let mut reconnect_now = false;
            match housing
                .sse_http
                .get(&events_url)
                .header("Accept", "text/event-stream")
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    emit_status(&app, &mut last_connected, true, &events_url);
                    let mut stream = resp.bytes_stream();
                    let mut buf = String::new();
                    let mut event_name = String::from("message");
                    let mut data_lines: Vec<String> = Vec::new();

                    loop {
                        tokio::select! {
                            changed = url_rx.changed() => {
                                if changed.is_ok() {
                                    reconnect_now = true;
                                } else {
                                    return;
                                }
                                break;
                            }
                            next_chunk = stream.next() => {
                                match next_chunk {
                                    Some(Ok(bytes)) => {
                                        buf.push_str(&String::from_utf8_lossy(&bytes));
                                        process_sse_buffer(
                                            &mut buf,
                                            &mut event_name,
                                            &mut data_lines,
                                            &app,
                                            &housing,
                                        );
                                    }
                                    Some(Err(_)) | None => {
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }

            if reconnect_now {
                last_connected = None;
                continue;
            }
            emit_status(&app, &mut last_connected, false, &events_url);

            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(1500)) => {}
                changed = url_rx.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    last_connected = None;
                }
            }
        }
    });
}

fn emit_status(app: &AppHandle, last_connected: &mut Option<bool>, connected: bool, url: &str) {
    if *last_connected != Some(connected) {
        *last_connected = Some(connected);
        let _ = app.emit(
            "housing-sse-status",
            SseStatusPayload {
                connected,
                url: url.to_string(),
            },
        );
    }
}

fn process_sse_buffer(
    buf: &mut String,
    event_name: &mut String,
    data_lines: &mut Vec<String>,
    app: &AppHandle,
    housing: &HousingClientState,
) {
    while let Some(nl_pos) = buf.find('\n') {
        let mut line = buf[..nl_pos].to_string();
        buf.drain(..=nl_pos);
        if line.ends_with('\r') {
            line.pop();
        }

        if line.is_empty() {
            if !data_lines.is_empty() {
                let payload = data_lines.join("\n");
                data_lines.clear();
                let evt = std::mem::replace(event_name, "message".to_string());
                housing.forward_sse_to_rmp(&payload);
                let _ = app.emit(
                    "housing-sse-event",
                    SseEventPayload {
                        event: evt,
                        data: payload,
                    },
                );
            } else {
                *event_name = "message".to_string();
            }
            continue;
        }

        if line.starts_with(':') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            *event_name = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
        }
    }
}
