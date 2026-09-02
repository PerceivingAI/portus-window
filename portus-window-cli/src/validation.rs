use super::args::MediaCommands;
use portus_window_protocol::{
    ConfigAction, InteractionAction, MediaAction, OpenSource, WorkspaceTarget,
    MAX_HISTORY_QUERY_CHARS, MAX_INTERACTION_ACTIONS, MAX_INTERACTION_KEY_CHARS,
    MAX_INTERACTION_SELECTOR_CHARS, MAX_INTERACTION_TEXT_CHARS, MAX_INTERACTION_VALUE_CHARS,
    MAX_PROFILE_NAME_CHARS, MAX_RETENTION_DAYS, MAX_SOURCE_BYTES,
};
use std::path::Path;

pub(super) fn absolute_utf8_path(path: &Path, purpose: &str) -> Result<String, String> {
    if path.as_os_str().is_empty() {
        return Err(format!("{purpose} path must not be empty"));
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("could not resolve current directory: {error}"))?
            .join(path)
    };
    absolute
        .to_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{purpose} path must be valid UTF-8"))
}

pub(super) fn classify_open_source(source: &str) -> Result<OpenSource, String> {
    let source = source.trim();
    if source.is_empty() {
        return Err("open source must not be empty".to_string());
    }
    if source.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "open source must be at most {MAX_SOURCE_BYTES} UTF-8 bytes"
        ));
    }

    if let Some((scheme, _)) = source.split_once("://") {
        return match scheme.to_ascii_lowercase().as_str() {
            "http" | "https" => Ok(OpenSource::Web {
                url: source.to_string(),
            }),
            "file" => Err(
                "file:// URLs are not supported; pass the local media path directly".to_string(),
            ),
            _ => Err(format!(
                "unsupported source scheme '{scheme}'; use http/https or a local media path"
            )),
        };
    }

    let path = absolute_utf8_path(Path::new(source), "local media")?;
    if path.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "resolved local media path must be at most {MAX_SOURCE_BYTES} UTF-8 bytes"
        ));
    }
    Ok(OpenSource::LocalMedia { path })
}

pub(super) fn validate_profile_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("profile name must not be empty".to_string());
    }
    if trimmed.chars().count() > MAX_PROFILE_NAME_CHARS {
        return Err(format!(
            "profile name must be at most {MAX_PROFILE_NAME_CHARS} characters"
        ));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(
            "profile name may only contain alphanumeric characters, dashes, and underscores"
                .to_string(),
        );
    }
    Ok(())
}

pub(super) fn validate_optional_profile(profile: Option<&str>) -> Result<Option<String>, String> {
    let Some(name) = profile else {
        return Ok(None);
    };
    validate_profile_name(name)?;
    Ok(Some(name.trim().to_string()))
}

pub(super) fn workspace_target_for(value: &str) -> Result<WorkspaceTarget, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("workspace target must not be empty".to_string());
    }
    if let Ok(index) = value.parse::<u32>() {
        return Ok(WorkspaceTarget::Index { index });
    }
    Ok(WorkspaceTarget::Name {
        name: value.to_string(),
    })
}

pub(super) fn config_action_for(show: bool, set: &Option<String>) -> Result<ConfigAction, String> {
    match (show, set.as_deref()) {
        (true, None) => Ok(ConfigAction::Show),
        (false, Some(setting)) => {
            let (key, value) = setting
                .split_once('=')
                .ok_or_else(|| "config --set requires KEY=VALUE".to_string())?;
            let key = key.trim();
            let value = value.trim();
            if key.is_empty() || value.is_empty() {
                return Err("config --set requires non-empty KEY=VALUE".to_string());
            }
            match key {
                "history_enabled" => match value {
                    "true" => Ok(ConfigAction::SetHistoryEnabled { enabled: true }),
                    "false" => Ok(ConfigAction::SetHistoryEnabled { enabled: false }),
                    _ => Err("history_enabled must be true or false".to_string()),
                },
                "retention_days" => {
                    if value.eq_ignore_ascii_case("null") {
                        return Ok(ConfigAction::SetRetentionDays { days: None });
                    }
                    let days = value.parse::<u32>().map_err(|_| {
                        format!(
                            "retention_days must be null or an integer between 1 and {MAX_RETENTION_DAYS}"
                        )
                    })?;
                    if !(1..=MAX_RETENTION_DAYS).contains(&days) {
                        return Err(format!(
                            "retention_days must be null or between 1 and {MAX_RETENTION_DAYS}"
                        ));
                    }
                    Ok(ConfigAction::SetRetentionDays { days: Some(days) })
                }
                _ => Err(format!(
                    "unknown config key '{key}'; supported keys are history_enabled and retention_days"
                )),
            }
        }
        _ => Err("config requires exactly one of --show or --set KEY=VALUE".to_string()),
    }
}

pub(super) fn history_query_for(query: &Option<String>) -> Result<Option<String>, String> {
    let Some(query) = query else {
        return Ok(None);
    };
    let query = query.trim();
    if query.is_empty() {
        return Err("history query must not be blank".to_string());
    }
    if query.chars().count() > MAX_HISTORY_QUERY_CHARS {
        return Err(format!(
            "history query must be at most {MAX_HISTORY_QUERY_CHARS} characters"
        ));
    }
    Ok(Some(query.to_string()))
}

pub(super) fn interaction_actions_for(
    raw_actions: &[String],
) -> Result<Vec<InteractionAction>, String> {
    if raw_actions.is_empty() {
        return Err("interact requires at least one --action JSON object".to_string());
    }
    if raw_actions.len() > MAX_INTERACTION_ACTIONS {
        return Err(format!(
            "interact accepts at most {MAX_INTERACTION_ACTIONS} ordered --action values"
        ));
    }
    raw_actions
        .iter()
        .enumerate()
        .map(|(index, raw)| {
            let action = serde_json::from_str::<InteractionAction>(raw).map_err(|error| {
                format!("invalid interaction action {} JSON: {error}", index + 1)
            })?;
            validate_interaction_action_cli(&action)?;
            Ok(action)
        })
        .collect()
}

pub(super) fn validate_interaction_action_cli(action: &InteractionAction) -> Result<(), String> {
    let validate_selector = |selector: &str| {
        if selector.trim().is_empty() {
            return Err("interaction selector must not be blank".to_string());
        }
        if selector.chars().count() > MAX_INTERACTION_SELECTOR_CHARS {
            return Err(format!(
                "interaction selector must be at most {MAX_INTERACTION_SELECTOR_CHARS} characters"
            ));
        }
        Ok(())
    };

    match action {
        InteractionAction::Click { selector }
        | InteractionAction::WaitForSelector { selector }
        | InteractionAction::CheckSelector { selector } => validate_selector(selector),
        InteractionAction::Fill { selector, value } => {
            validate_selector(selector)?;
            if value.chars().count() > MAX_INTERACTION_VALUE_CHARS {
                return Err(format!(
                    "interaction fill value must be at most {MAX_INTERACTION_VALUE_CHARS} characters"
                ));
            }
            Ok(())
        }
        InteractionAction::PressKey { key, selector } => {
            if key.trim().is_empty() || key.chars().count() > MAX_INTERACTION_KEY_CHARS {
                return Err(format!(
                    "interaction key must be nonblank and at most {MAX_INTERACTION_KEY_CHARS} characters"
                ));
            }
            if let Some(selector) = selector {
                validate_selector(selector)?;
            }
            Ok(())
        }
        InteractionAction::WaitForText { text, selector }
        | InteractionAction::CheckText { text, selector } => {
            if text.is_empty() || text.chars().count() > MAX_INTERACTION_TEXT_CHARS {
                return Err(format!(
                    "interaction text must be non-empty and at most {MAX_INTERACTION_TEXT_CHARS} characters"
                ));
            }
            if let Some(selector) = selector {
                validate_selector(selector)?;
            }
            Ok(())
        }
    }
}

pub(super) fn media_action_for(action: &MediaCommands) -> Result<MediaAction, String> {
    match action {
        MediaCommands::Play => Ok(MediaAction::Play),
        MediaCommands::Pause => Ok(MediaAction::Pause),
        MediaCommands::Seek { seconds } => {
            if !seconds.is_finite() || *seconds < 0.0 {
                return Err("media seek seconds must be a finite value >= 0".to_string());
            }
            Ok(MediaAction::Seek { seconds: *seconds })
        }
        MediaCommands::SetVolume { level } => {
            if !level.is_finite() || !(0.0..=1.0).contains(level) {
                return Err("media volume level must be a finite value between 0 and 1".to_string());
            }
            Ok(MediaAction::SetVolume { level: *level })
        }
    }
}
