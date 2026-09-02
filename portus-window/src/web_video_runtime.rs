use portus_window_protocol::{WebVideoAction, WebVideoState, MAX_LOAD_ERROR_CHARS};
use serde::Deserialize;
use std::sync::mpsc;
use std::time::Duration;
use tauri::WebviewWindow;

const WEB_VIDEO_CALLBACK_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WebVideoProbeResult {
    ok: bool,
    #[serde(default)]
    state: Option<WebVideoState>,
    #[serde(default)]
    error: Option<String>,
}

pub fn validate_action(action: &WebVideoAction) -> Result<(), String> {
    match action {
        WebVideoAction::Seek { seconds } if !seconds.is_finite() || *seconds < 0.0 => {
            Err("web-video seek seconds must be a finite value >= 0".to_string())
        }
        WebVideoAction::SetVolume { level }
            if !level.is_finite() || !(0.0..=1.0).contains(level) =>
        {
            Err("web-video volume level must be a finite value between 0 and 1".to_string())
        }
        _ => Ok(()),
    }
}

pub fn control_web_video(
    window: &WebviewWindow,
    action: &WebVideoAction,
) -> Result<WebVideoState, String> {
    validate_action(action)?;
    let (command, value) = match action {
        WebVideoAction::State => ("state", "null".to_string()),
        WebVideoAction::Play => ("play", "null".to_string()),
        WebVideoAction::Pause => ("pause", "null".to_string()),
        WebVideoAction::Seek { seconds } => ("seek", seconds.to_string()),
        WebVideoAction::Mute => ("mute", "null".to_string()),
        WebVideoAction::Unmute => ("unmute", "null".to_string()),
        WebVideoAction::SetVolume { level } => ("set_volume", level.to_string()),
    };
    let script = format!(
        r#"(() => {{
  try {{
    const bridge = window.__portusWebVideo;
    if (!bridge || typeof bridge.command !== "function") {{
      return {{ ok: false, state: null, error: "web-video control bridge is unavailable" }};
    }}
    return {{ ok: true, state: bridge.command("{command}", {value}), error: null }};
  }} catch (error) {{
    return {{ ok: false, state: null, error: error && error.message ? String(error.message) : String(error) }};
  }}
}})()"#
    );
    evaluate_web_video_script(window, script)
}

fn sanitize_state(mut state: WebVideoState) -> Result<WebVideoState, String> {
    for (name, value) in [
        ("duration", state.duration_seconds),
        ("current_time", state.current_time_seconds),
    ] {
        if value.is_some_and(|value| !value.is_finite() || value < 0.0) {
            return Err(format!("web-video state returned an invalid {name}"));
        }
    }
    if state
        .volume
        .is_some_and(|volume| !volume.is_finite() || !(0.0..=1.0).contains(&volume))
    {
        return Err("web-video state returned an invalid volume".to_string());
    }
    state.error = state.error.map(|error| {
        if error.chars().count() <= MAX_LOAD_ERROR_CHARS {
            error
        } else {
            error.chars().take(MAX_LOAD_ERROR_CHARS).collect()
        }
    });
    Ok(state)
}

fn evaluate_web_video_script(
    window: &WebviewWindow,
    script: impl Into<String>,
) -> Result<WebVideoState, String> {
    let (sender, receiver) = mpsc::sync_channel(1);
    window
        .eval_with_callback(script, move |raw| {
            let parsed = serde_json::from_str::<WebVideoProbeResult>(&raw)
                .map_err(|error| format!("invalid web-video state result: {error}"))
                .and_then(|result| {
                    if result.ok {
                        result
                            .state
                            .ok_or_else(|| "web-video state result omitted state".to_string())
                            .and_then(sanitize_state)
                    } else {
                        Err(result
                            .error
                            .unwrap_or_else(|| "web-video operation failed".to_string()))
                    }
                });
            let _ = sender.send(parsed);
        })
        .map_err(|error| format!("could not schedule web-video operation: {error}"))?;

    receiver
        .recv_timeout(WEB_VIDEO_CALLBACK_TIMEOUT)
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => {
                "web-video operation timed out after 3 seconds".to_string()
            }
            mpsc::RecvTimeoutError::Disconnected => {
                "web-video operation callback disconnected".to_string()
            }
        })?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_rejects_invalid_numeric_actions() {
        assert!(validate_action(&WebVideoAction::Seek { seconds: -1.0 }).is_err());
        assert!(validate_action(&WebVideoAction::SetVolume { level: 1.1 }).is_err());
        assert!(validate_action(&WebVideoAction::SetVolume { level: 0.5 }).is_ok());
    }

    #[test]
    fn state_validation_preserves_bounded_errors() {
        let state = sanitize_state(WebVideoState {
            playing: false,
            paused: false,
            ended: false,
            muted: false,
            duration_seconds: None,
            current_time_seconds: None,
            volume: None,
            error: Some("x".repeat(MAX_LOAD_ERROR_CHARS + 8)),
        })
        .unwrap();
        assert_eq!(state.error.unwrap().chars().count(), MAX_LOAD_ERROR_CHARS);
    }
}
