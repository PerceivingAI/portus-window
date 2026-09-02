use super::args::{AuthSessionCommands, Commands};
use super::validation::*;
use portus_window_protocol::{
    encode_frame, AuthSessionAction, AuthSessionScope, AuthSessionTarget, BrowserKind, FrameError,
    InteractionScreenshot, Request, WindowGeometrySpec, WindowStateAction, WorkspaceSpec,
    MAX_DESCRIPTION_CHARS, MAX_INTERACTION_TIMEOUT_MS, MIN_INTERACTION_TIMEOUT_MS,
    PROTOCOL_VERSION,
};
use std::io::Read;

pub fn request_for(command: &Commands) -> Result<Request, String> {
    let request = match command {
        Commands::Ping => Request::ping(),
        Commands::OpenBatch { file } => {
            let manifest = match file {
                Some(path) if path.to_str() != Some("-") => std::fs::read_to_string(path)
                    .map_err(|e| format!("failed to read open-batch manifest: {e}"))?,
                _ => {
                    let mut buffer = String::new();
                    std::io::stdin().read_to_string(&mut buffer).map_err(|e| {
                        format!("failed to read open-batch manifest from STDIN: {e}")
                    })?;
                    buffer
                }
            };
            let windows: Vec<portus_window_protocol::WindowSpec> = serde_json::from_str(&manifest)
                .map_err(|e| format!("invalid open-batch JSON manifest: {e}"))?;
            if windows.is_empty() {
                return Err("open-batch requires at least one window".to_string());
            }
            if windows.len() > portus_window_protocol::MAX_OPEN_BATCH_WINDOWS {
                return Err(format!(
                    "open-batch supports at most {} windows",
                    portus_window_protocol::MAX_OPEN_BATCH_WINDOWS
                ));
            }
            for window in &windows {
                if window.media.is_some() {
                    return Err("open-batch manifest windows must not contain media actions; use open or apply for media control".to_string());
                }
            }
            Request::OpenBatch {
                version: PROTOCOL_VERSION,
                windows,
            }
        }
        Commands::Open {
            source,
            description,
            profile,
            width,
            height,
            x,
            y,
            maximize,
            minimize,
            restore,
            fullscreen,
            always_on_top,
            workspace,
            ..
        } => {
            let state = if *maximize {
                Some(WindowStateAction::Maximize)
            } else if *minimize {
                Some(WindowStateAction::Minimize)
            } else if *restore {
                Some(WindowStateAction::Restore)
            } else if *fullscreen {
                Some(WindowStateAction::Fullscreen)
            } else {
                None
            };
            let geometry = if width.is_some()
                || height.is_some()
                || x.is_some()
                || y.is_some()
                || state.is_some()
                || always_on_top.is_some()
                || workspace.is_some()
            {
                Some(WindowGeometrySpec {
                    width: *width,
                    height: *height,
                    x: *x,
                    y: *y,
                    state,
                    always_on_top: *always_on_top,
                    workspace: workspace.as_deref().map(workspace_target_for).transpose()?,
                })
            } else {
                None
            };
            Request::Open {
                version: PROTOCOL_VERSION,
                source: classify_open_source(source)?,
                description: description.clone(),
                profile: validate_optional_profile(profile.as_deref())?,
                geometry,
            }
        }
        Commands::Apply { file, prune } => {
            let spec_json = match file {
                Some(path) if path.to_str() != Some("-") => std::fs::read_to_string(path)
                    .map_err(|e| format!("failed to read workspace file: {e}"))?,
                _ => {
                    let mut buffer = String::new();
                    std::io::stdin()
                        .read_to_string(&mut buffer)
                        .map_err(|e| format!("failed to read from STDIN: {e}"))?;
                    buffer
                }
            };
            let spec: WorkspaceSpec = serde_json::from_str(&spec_json)
                .map_err(|e| format!("invalid workspace spec JSON: {e}"))?;
            Request::Apply {
                version: PROTOCOL_VERSION,
                spec,
                prune: *prune,
            }
        }
        Commands::Export { .. } => Request::Export {
            version: PROTOCOL_VERSION,
        },
        Commands::List => Request::List {
            version: PROTOCOL_VERSION,
        },
        Commands::History { query, clear } => {
            if *clear && query.is_some() {
                return Err("history accepts --query or --clear, not both".to_string());
            }
            Request::History {
                version: PROTOCOL_VERSION,
                query: history_query_for(query)?,
                clear: *clear,
            }
        }
        Commands::Config { show, set } => Request::Config {
            version: PROTOCOL_VERSION,
            action: config_action_for(*show, set)?,
        },
        Commands::AuthSession { action } => Request::AuthSession {
            version: PROTOCOL_VERSION,
            action: auth_session_action_for(action)?,
        },
        Commands::Workspaces => Request::Workspaces {
            version: PROTOCOL_VERSION,
        },
        Commands::Status { target } => Request::Status {
            version: PROTOCOL_VERSION,
            target: target.clone(),
        },
        Commands::Console { target, all } => {
            if *all == target.is_some() {
                return Err("console requires either target or --all".to_string());
            }
            Request::Console {
                version: PROTOCOL_VERSION,
                target: target.clone(),
                all: *all,
            }
        }
        Commands::Screenshot {
            target,
            out,
            overwrite,
            all,
        } => {
            if *all == target.is_some() {
                return Err("screenshot requires either target or --all".to_string());
            }
            let out_path = out
                .as_ref()
                .ok_or_else(|| "screenshot requires --out <PATH>".to_string())?;
            Request::Screenshot {
                version: PROTOCOL_VERSION,
                target: target.clone(),
                out: absolute_utf8_path(out_path, "screenshot output")?,
                overwrite: *overwrite,
                all: *all,
            }
        }
        Commands::Record {
            target,
            out,
            duration_seconds,
            overwrite,
        } => {
            if *duration_seconds <= 0.0 || *duration_seconds > 600.0 {
                return Err("record duration must be between 0.1 and 600.0 seconds".to_string());
            }
            Request::Record {
                version: PROTOCOL_VERSION,
                target: target.clone(),
                out: absolute_utf8_path(out, "record output")?,
                duration_seconds: *duration_seconds,
                overwrite: *overwrite,
            }
        }
        Commands::Media {
            target,
            action,
            all,
        } => {
            if *all == target.is_some() {
                return Err("media requires either target or --all".to_string());
            }
            Request::Media {
                version: PROTOCOL_VERSION,
                target: target.clone(),
                action: media_action_for(action)?,
                all: *all,
            }
        }
        Commands::Interact {
            target,
            actions,
            interaction_timeout_ms,
            screenshot_out,
            screenshot_overwrite,
        } => {
            if !(MIN_INTERACTION_TIMEOUT_MS..=MAX_INTERACTION_TIMEOUT_MS)
                .contains(interaction_timeout_ms)
            {
                return Err(format!(
                    "interaction timeout must be between {MIN_INTERACTION_TIMEOUT_MS} and {MAX_INTERACTION_TIMEOUT_MS} ms"
                ));
            }
            let screenshot = screenshot_out
                .as_ref()
                .map(|out| -> Result<InteractionScreenshot, String> {
                    Ok(InteractionScreenshot {
                        out: absolute_utf8_path(out, "interaction screenshot output")?,
                        overwrite: *screenshot_overwrite,
                    })
                })
                .transpose()?;
            Request::Interact {
                version: PROTOCOL_VERSION,
                target: target.clone(),
                actions: interaction_actions_for(actions)?,
                timeout_ms: *interaction_timeout_ms,
                screenshot,
            }
        }
        Commands::Tag {
            target,
            description,
            all,
        } => {
            let (target_id, desc) = if *all {
                if description.is_some() {
                    return Err("tag with --all accepts only description".to_string());
                }
                let desc = target
                    .clone()
                    .ok_or_else(|| "tag requires a description".to_string())?;
                (None, desc)
            } else {
                let target_id = target
                    .clone()
                    .ok_or_else(|| "tag requires target or --all".to_string())?;
                let desc = description
                    .clone()
                    .ok_or_else(|| "tag requires a description".to_string())?;
                (Some(target_id), desc)
            };
            if desc.chars().count() > MAX_DESCRIPTION_CHARS {
                return Err(format!(
                    "tag description must be at most {MAX_DESCRIPTION_CHARS} characters"
                ));
            }
            Request::Tag {
                version: PROTOCOL_VERSION,
                target: target_id,
                description: desc,
                all: *all,
            }
        }
        Commands::Focus { target } => Request::Focus {
            version: PROTOCOL_VERSION,
            target: target.clone(),
        },
        Commands::Resize {
            target,
            width,
            height,
            x,
            y,
            maximize,
            minimize,
            restore,
            fullscreen,
            always_on_top,
            workspace,
            all,
        } => {
            if *all == target.is_some() {
                return Err("resize requires either target or --all".to_string());
            }
            let state = if *maximize {
                Some(WindowStateAction::Maximize)
            } else if *minimize {
                Some(WindowStateAction::Minimize)
            } else if *restore {
                Some(WindowStateAction::Restore)
            } else if *fullscreen {
                Some(WindowStateAction::Fullscreen)
            } else {
                None
            };
            if width.is_none()
                && height.is_none()
                && x.is_none()
                && y.is_none()
                && state.is_none()
                && always_on_top.is_none()
                && workspace.is_none()
            {
                return Err("resize requires at least one operation".to_string());
            }
            Request::Resize {
                version: PROTOCOL_VERSION,
                target: target.clone(),
                width: *width,
                height: *height,
                x: *x,
                y: *y,
                state,
                always_on_top: *always_on_top,
                workspace: workspace.as_deref().map(workspace_target_for).transpose()?,
                all: *all,
            }
        }
        Commands::Close { target, all } => {
            if *all == target.is_some() {
                return Err("close requires exactly one target or --all".to_string());
            }
            Request::Close {
                version: PROTOCOL_VERSION,
                target: target.clone(),
                all: *all,
            }
        }
    };
    match encode_frame(&request) {
        Ok(_) => Ok(request),
        Err(FrameError::TooLarge { actual, maximum }) => Err(format!(
            "request is {actual} bytes; maximum protocol frame is {maximum} bytes"
        )),
        Err(error) => Err(format!(
            "request cannot be encoded as protocol JSON: {error}"
        )),
    }
}

fn auth_session_action_for(action: &AuthSessionCommands) -> Result<AuthSessionAction, String> {
    fn target_for(
        domain: &Option<String>,
        window: &Option<String>,
    ) -> Result<AuthSessionTarget, String> {
        match (domain.as_deref(), window.as_deref()) {
            (Some(domain), None) => Ok(AuthSessionTarget::Domain {
                domain: domain.to_string(),
            }),
            (None, Some(window)) => Ok(AuthSessionTarget::Window {
                window_session_id: window.to_string(),
            }),
            _ => Err(
                "authenticated-session command requires exactly one of --domain or --window"
                    .to_string(),
            ),
        }
    }

    fn browser_for(value: &str) -> Result<BrowserKind, String> {
        match value {
            "firefox" => Ok(BrowserKind::Firefox),
            "chromium" => Ok(BrowserKind::Chromium),
            "chrome" => Ok(BrowserKind::Chrome),
            "brave" => Ok(BrowserKind::Brave),
            _ => Err("browser must be firefox, chromium, chrome, or brave".to_string()),
        }
    }

    fn scope_for(value: &str) -> Result<AuthSessionScope, String> {
        match value {
            "once" => Ok(AuthSessionScope::Once),
            "session" => Ok(AuthSessionScope::Session),
            "remembered" => Ok(AuthSessionScope::Remembered),
            _ => Err("scope must be once, session, or remembered".to_string()),
        }
    }

    Ok(match action {
        AuthSessionCommands::Request {
            domain,
            window,
            browser,
            scope,
            reason,
            presentation_window,
        } => AuthSessionAction::Request {
            target: target_for(domain, window)?,
            browser: browser_for(browser)?,
            scope: scope_for(scope)?,
            reason: reason.clone(),
            presentation_window_session_id: presentation_window.clone(),
        },
        AuthSessionCommands::Status { domain, window } => AuthSessionAction::Status {
            target: target_for(domain, window)?,
        },
        AuthSessionCommands::Apply {
            domain,
            window,
            window_session_id,
        } => AuthSessionAction::Apply {
            target: target_for(domain, window)?,
            window_session_id: window_session_id.clone(),
        },
        AuthSessionCommands::Revoke { domain, window } => AuthSessionAction::Revoke {
            target: target_for(domain, window)?,
        },
    })
}
