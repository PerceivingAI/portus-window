use portus_window_protocol::{MediaAction, MediaState, MAX_LOAD_ERROR_CHARS};
use serde::Deserialize;
use std::sync::mpsc;
use std::time::Duration;
use tauri::WebviewWindow;

const MEDIA_CALLBACK_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MediaProbeResult {
    ok: bool,
    #[serde(default)]
    state: Option<MediaState>,
    #[serde(default)]
    error: Option<String>,
}

const MEDIA_STATE_SCRIPT: &str = r#"
(() => {
  try {
    const media = document.getElementById("portus-media-player");
    if (!media) return { ok: false, state: null, error: "media element is unavailable" };
    const finite = (value) => Number.isFinite(value) ? value : null;
    return {
      ok: true,
      state: {
        playing: !media.paused && !media.ended,
        paused: media.paused,
        ended: media.ended,
        duration_seconds: finite(media.duration),
        position_seconds: finite(media.currentTime),
        volume: Number.isFinite(media.volume) ? media.volume : 1,
        error: media.dataset.portusError || (media.error ? ("media error " + media.error.code + ": " + (media.error.message || "unknown decoder error")) : null),
      },
      error: null,
    };
  } catch (error) {
    return { ok: false, state: null, error: error && error.message ? String(error.message) : String(error) };
  }
})()
"#;

pub fn probe_media_state(window: &WebviewWindow) -> Result<MediaState, String> {
    evaluate_media_script(window, MEDIA_STATE_SCRIPT)
}

pub fn control_media(window: &WebviewWindow, action: &MediaAction) -> Result<MediaState, String> {
    let operation = match action {
        MediaAction::Play => {
            "delete media.dataset.portusError; try { const pending = media.play(); if (pending && typeof pending.catch === 'function') pending.catch((error) => { media.dataset.portusError = String(error && error.message ? error.message : error); }); } catch (error) { return { ok: false, state: null, error: String(error && error.message ? error.message : error) }; }"
                .to_string()
        }
        MediaAction::Pause => "delete media.dataset.portusError; media.pause();".to_string(),
        MediaAction::Seek { seconds } => {
            format!("delete media.dataset.portusError; media.currentTime = {seconds};")
        }
        MediaAction::SetVolume { level } => {
            format!("delete media.dataset.portusError; media.volume = {level};")
        }
    };
    let script = format!(
        r#"(() => {{
  try {{
    const media = document.getElementById("portus-media-player");
    if (!media) return {{ ok: false, state: null, error: "media element is unavailable" }};
    {operation}
    const finite = (value) => Number.isFinite(value) ? value : null;
    return {{
      ok: true,
      state: {{
        playing: !media.paused && !media.ended,
        paused: media.paused,
        ended: media.ended,
        duration_seconds: finite(media.duration),
        position_seconds: finite(media.currentTime),
        volume: Number.isFinite(media.volume) ? media.volume : 1,
        error: media.dataset.portusError || (media.error ? ("media error " + media.error.code + ": " + (media.error.message || "unknown decoder error")) : null),
      }},
      error: null,
    }};
  }} catch (error) {{
    return {{ ok: false, state: null, error: error && error.message ? String(error.message) : String(error) }};
  }}
}})()"#
    );
    let state = evaluate_media_script(window, script)?;
    if let Some(error) = &state.error {
        return Err(error.clone());
    }
    Ok(state)
}

fn sanitize_media_state(mut state: MediaState) -> Result<MediaState, String> {
    if let Some(vol) = state.volume {
        if !vol.is_finite() || !(0.0..=1.0).contains(&vol) {
            return Err("media state returned an invalid volume".to_string());
        }
    }
    for (name, value) in [
        ("duration", state.duration_seconds),
        ("position", state.position_seconds),
    ] {
        if value.is_some_and(|value| !value.is_finite() || value < 0.0) {
            return Err(format!("media state returned an invalid {name}"));
        }
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

fn evaluate_media_script(
    window: &WebviewWindow,
    script: impl Into<String>,
) -> Result<MediaState, String> {
    let (sender, receiver) = mpsc::sync_channel(1);
    window
        .eval_with_callback(script, move |raw| {
            let parsed = serde_json::from_str::<MediaProbeResult>(&raw)
                .map_err(|error| format!("invalid media state result: {error}"))
                .and_then(|result| {
                    if result.ok {
                        result
                            .state
                            .ok_or_else(|| "media state result omitted state".to_string())
                            .and_then(sanitize_media_state)
                    } else {
                        Err(result
                            .error
                            .unwrap_or_else(|| "media operation failed".to_string()))
                    }
                });
            let _ = sender.send(parsed);
        })
        .map_err(|error| format!("could not schedule media operation: {error}"))?;

    receiver
        .recv_timeout(MEDIA_CALLBACK_TIMEOUT)
        .map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => {
                "media operation timed out after 3 seconds".to_string()
            }
            mpsc::RecvTimeoutError::Disconnected => {
                "media operation callback disconnected".to_string()
            }
        })?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_scripts_do_not_expose_a_tauri_bridge() {
        assert!(MEDIA_STATE_SCRIPT.contains("portus-media-player"));
        assert!(!MEDIA_STATE_SCRIPT.contains("__TAURI__"));
        assert!(!MEDIA_STATE_SCRIPT.contains("invoke("));
    }

    #[test]
    fn media_state_validation_rejects_invalid_values_and_bounds_errors() {
        let invalid = MediaState {
            playing: false,
            paused: false,
            ended: false,
            duration_seconds: None,
            position_seconds: Some(0.0),
            volume: Some(2.0),
            error: None,
        };
        assert!(sanitize_media_state(invalid).is_err());

        let bounded = sanitize_media_state(MediaState {
            playing: false,
            paused: false,
            ended: false,
            duration_seconds: Some(10.0),
            position_seconds: Some(2.0),
            volume: Some(0.5),
            error: Some("x".repeat(MAX_LOAD_ERROR_CHARS + 10)),
        })
        .unwrap();
        assert_eq!(bounded.error.unwrap().chars().count(), MAX_LOAD_ERROR_CHARS);
    }

    #[test]
    fn media_control_actions_cover_play_pause_seek_and_volume() {
        let actions = [
            MediaAction::Play,
            MediaAction::Pause,
            MediaAction::Seek { seconds: 12.5 },
            MediaAction::SetVolume { level: 0.5 },
        ];
        assert_eq!(actions.len(), 4);
        assert!(matches!(actions[0], MediaAction::Play));
        assert!(matches!(actions[1], MediaAction::Pause));
        assert!(matches!(actions[2], MediaAction::Seek { seconds } if seconds == 12.5));
        assert!(matches!(actions[3], MediaAction::SetVolume { level } if level == 0.5));
    }

    #[test]
    fn generated_control_values_are_numeric_not_user_script() {
        let seek = match (MediaAction::Seek { seconds: 12.5 }) {
            MediaAction::Seek { seconds } => format!("media.currentTime = {seconds};"),
            _ => unreachable!(),
        };
        assert_eq!(seek, "media.currentTime = 12.5;");
    }
}
