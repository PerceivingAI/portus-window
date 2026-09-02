use crate::auth_session::{
    dispatch_action, AuthConsentController, AuthGrantTarget, AuthenticatedSessionAuthority,
    AuthenticatedSessionWebKit,
};
use crate::database::PersistenceError;
use crate::media::MediaAdmissionError;
use crate::tauri_window::{WindowManager, WindowManagerError};
use crate::window_core::{ResizeSpec, WindowCoreError};
use crate::workspace::WorkspaceError;
use portus_window_protocol::{
    AuthSessionAction, AuthSessionDecision, AuthSessionTarget, ErrorCode, OpenResult, Request,
    Response, PROTOCOL_VERSION,
};
use serde_json::json;
use std::sync::Arc;

pub trait CommandHandler: Send + Sync + 'static {
    fn handle(&self, request: Request) -> Response;
}

pub struct DaemonHandler {
    windows: Arc<WindowManager>,
    auth_authority: Arc<AuthenticatedSessionAuthority>,
    auth_webkit: Arc<AuthenticatedSessionWebKit>,
    auth_consent: Arc<AuthConsentController>,
}

impl DaemonHandler {
    pub fn new(
        windows: Arc<WindowManager>,
        auth_authority: Arc<AuthenticatedSessionAuthority>,
        auth_webkit: Arc<AuthenticatedSessionWebKit>,
        auth_consent: Arc<AuthConsentController>,
    ) -> Self {
        Self {
            windows,
            auth_authority,
            auth_webkit,
            auth_consent,
        }
    }
}

impl CommandHandler for DaemonHandler {
    fn handle(&self, request: Request) -> Response {
        match request {
            Request::Ping { .. } => ok(json!({
                "service": "portus-window",
                "version": env!("CARGO_PKG_VERSION"),
                "protocol_version": PROTOCOL_VERSION,
            })),
            Request::OpenBatch { windows, .. } => match self.windows.open_batch(windows) {
                Ok(results) => ok(json!({ "opened": results })),
                Err(error) => manager_error(error),
            },
            Request::Open {
                source,
                description,
                profile,
                geometry,
                ..
            } => match self.windows.open(source, description, profile, geometry) {
                Ok(window) => ok(OpenResult {
                    window_session_id: window.window_session_id,
                }),
                Err(error) => manager_error(error),
            },
            Request::Apply { spec, prune, .. } => match self.windows.apply_spec(spec, prune) {
                Ok(result) => ok(result),
                Err(error) => manager_error(error),
            },
            Request::Export { .. } => match self.windows.export_spec() {
                Ok(spec) => ok(spec),
                Err(error) => manager_error(error),
            },
            Request::List { .. } => match self.windows.list() {
                Ok(windows) => ok(windows),
                Err(error) => manager_error(error),
            },
            Request::Workspaces { .. } => match self.windows.workspaces() {
                Ok(workspaces) => ok(workspaces),
                Err(error) => manager_error(error),
            },
            Request::History { query, clear, .. } => {
                if clear {
                    match self.windows.clear_history() {
                        Ok(result) => ok(result),
                        Err(error) => manager_error(error),
                    }
                } else {
                    match self.windows.history(query) {
                        Ok(result) => ok(result),
                        Err(error) => manager_error(error),
                    }
                }
            }
            Request::Config { action, .. } => match self.windows.config(action) {
                Ok(config) => ok(config),
                Err(error) => manager_error(error),
            },
            Request::Status { target, .. } => match self.windows.observed_status(&target) {
                Ok(window) => ok(window),
                Err(error) => manager_error(error),
            },
            Request::Console { target, all, .. } => {
                if all {
                    match self.windows.console_all() {
                        Ok(results) => ok(results),
                        Err(error) => manager_error(error),
                    }
                } else if let Some(target) = target {
                    match self.windows.console(&target) {
                        Ok(result) => ok(result),
                        Err(error) => manager_error(error),
                    }
                } else {
                    Response::error(
                        ErrorCode::ValidationFailed,
                        "console requires either target or --all",
                    )
                }
            }
            Request::Screenshot {
                target,
                out,
                overwrite,
                all,
                ..
            } => {
                if all {
                    match self.windows.screenshot_all(&out, overwrite) {
                        Ok(results) => ok(results),
                        Err(error) => manager_error(error),
                    }
                } else if let Some(target) = target {
                    match self.windows.screenshot(&target, &out, overwrite) {
                        Ok(result) => ok(result),
                        Err(error) => manager_error(error),
                    }
                } else {
                    Response::error(
                        ErrorCode::ValidationFailed,
                        "screenshot requires either target or --all",
                    )
                }
            }
            Request::Record {
                target,
                out,
                duration_seconds,
                overwrite,
                ..
            } => match self
                .windows
                .record(&target, &out, duration_seconds, overwrite)
            {
                Ok(result) => ok(result),
                Err(error) => manager_error(error),
            },
            Request::Media {
                target,
                action,
                all,
                ..
            } => {
                if all {
                    match self.windows.media_all(action) {
                        Ok(results) => ok(results),
                        Err(error) => manager_error(error),
                    }
                } else if let Some(target) = target {
                    match self.windows.media(&target, action) {
                        Ok(result) => ok(result),
                        Err(error) => manager_error(error),
                    }
                } else {
                    Response::error(
                        ErrorCode::ValidationFailed,
                        "media requires either target or --all",
                    )
                }
            }
            Request::Interact {
                target,
                actions,
                timeout_ms,
                screenshot,
                ..
            } => match self
                .windows
                .interact(&target, actions, timeout_ms, screenshot)
            {
                Ok(result) => ok(result),
                Err(error) => manager_error(error),
            },
            Request::Tag {
                target,
                description,
                all,
                ..
            } => {
                if all {
                    match self.windows.tag_all(description) {
                        Ok(windows) => ok(windows),
                        Err(error) => manager_error(error),
                    }
                } else if let Some(target) = target {
                    match self.windows.tag(&target, description) {
                        Ok(window) => ok(window),
                        Err(error) => manager_error(error),
                    }
                } else {
                    Response::error(
                        ErrorCode::ValidationFailed,
                        "tag requires either target or --all",
                    )
                }
            }
            Request::Focus { target, .. } => match self.windows.focus(&target) {
                Ok(window) => ok(window),
                Err(error) => manager_error(error),
            },
            Request::Resize {
                target,
                width,
                height,
                x,
                y,
                state,
                always_on_top,
                workspace,
                all,
                ..
            } => {
                let spec = ResizeSpec {
                    width,
                    height,
                    x,
                    y,
                    state,
                    always_on_top,
                    workspace,
                };
                if all {
                    match self.windows.resize_all(spec) {
                        Ok(windows) => ok(windows),
                        Err(error) => manager_error(error),
                    }
                } else if let Some(target) = target {
                    match self.windows.resize(&target, spec) {
                        Ok(window) => ok(window),
                        Err(error) => manager_error(error),
                    }
                } else {
                    Response::error(
                        ErrorCode::ValidationFailed,
                        "resize requires either target or --all",
                    )
                }
            }
            Request::Close { target, all, .. } => {
                let result = if all {
                    if target.is_some() {
                        return Response::error(
                            ErrorCode::ValidationFailed,
                            "close accepts either target or --all, not both",
                        );
                    }
                    self.windows.close_all()
                } else if let Some(target) = target {
                    self.windows.close(&target)
                } else {
                    return Response::error(
                        ErrorCode::ValidationFailed,
                        "close requires either target or --all",
                    );
                };
                match result {
                    Ok(closed) => ok(json!({ "closed": closed })),
                    Err(error) => manager_error(error),
                }
            }
            Request::AuthSession { action, .. } => handle_auth_session(self, action),
            Request::WebVideo { .. } => Response::error(
                ErrorCode::Internal,
                "web video handling is managed by authority",
            ),
        }
    }
}

fn handle_auth_session(handler: &DaemonHandler, action: AuthSessionAction) -> Response {
    let resolved_window_domain = match &action {
        AuthSessionAction::Request {
            target: AuthSessionTarget::Window { window_session_id },
            ..
        }
        | AuthSessionAction::Status {
            target: AuthSessionTarget::Window { window_session_id },
        }
        | AuthSessionAction::Revoke {
            target: AuthSessionTarget::Window { window_session_id },
        } => {
            let window = match handler.windows.status(window_session_id) {
                Ok(window) => window,
                Err(error) => return manager_error(error),
            };
            let url = window
                .current_url
                .as_deref()
                .unwrap_or(&window.requested_source);
            let Some(host) = url::Url::parse(url)
                .ok()
                .and_then(|parsed| parsed.host_str().map(str::to_owned))
            else {
                return Response::error(
                    ErrorCode::ValidationFailed,
                    "authenticated-session window target has no usable web domain",
                );
            };
            match crate::auth_session::validate_domain(&host) {
                Ok(target) => Some(target),
                Err(error) => return auth_error(error),
            }
        }
        _ => None,
    };

    if let AuthSessionAction::Apply {
        window_session_id, ..
    } = &action
    {
        let validated = match crate::auth_session::validate_action(&action) {
            Ok(crate::auth_session::ValidatedAuthRequest::Apply {
                target,
                target_window_session_id,
                ..
            }) => (target, target_window_session_id),
            Ok(_) => {
                return Response::error(
                    ErrorCode::ValidationFailed,
                    "invalid authenticated-session apply action",
                )
            }
            Err(error) => return auth_error(error),
        };
        let grant_target = match (validated.0, validated.1) {
            (Some(target), None) => AuthGrantTarget::Domain(target.requested_domain().to_string()),
            (None, Some(window_id)) => AuthGrantTarget::Window(window_id),
            _ => {
                return Response::error(
                    ErrorCode::ValidationFailed,
                    "invalid authenticated-session apply target",
                )
            }
        };
        return match handler.auth_webkit.apply(&grant_target, window_session_id) {
            Ok(result) => ok(result),
            Err(error) => Response::error(ErrorCode::Internal, error.to_string()),
        };
    }

    let result = match dispatch_action(&handler.auth_authority, &action, resolved_window_domain) {
        Ok(result) => result,
        Err(error) => return auth_error(error),
    };

    if matches!(action, AuthSessionAction::Request { .. })
        && result.state.decision == AuthSessionDecision::Pending
    {
        let target = match &result.state.target {
            AuthSessionTarget::Window { window_session_id } => {
                AuthGrantTarget::Window(window_session_id.clone())
            }
            AuthSessionTarget::Domain { domain } => AuthGrantTarget::Domain(domain.clone()),
        };
        if let Err(error) = handler.auth_consent.present(&target) {
            let _ = handler.auth_authority.deny(&target);
            return Response::error(ErrorCode::Internal, error.to_string());
        }
    }

    if matches!(action, AuthSessionAction::Revoke { .. }) {
        let target = match &result.state.target {
            AuthSessionTarget::Window { window_session_id } => {
                AuthGrantTarget::Window(window_session_id.clone())
            }
            AuthSessionTarget::Domain { domain } => AuthGrantTarget::Domain(domain.clone()),
        };
        if let Err(error) = handler.auth_webkit.revoke(&target) {
            return Response::error(ErrorCode::Internal, error.to_string());
        }
    }
    ok(result)
}

pub struct HealthHandler;

impl CommandHandler for HealthHandler {
    fn handle(&self, request: Request) -> Response {
        match request {
            Request::Ping { .. } => Response::ok(json!({
                "service": "portus-window",
                "version": env!("CARGO_PKG_VERSION"),
                "protocol_version": PROTOCOL_VERSION,
            })),
            _ => Response::error(
                ErrorCode::ValidationFailed,
                "window commands are unavailable in this test handler",
            ),
        }
    }
}

fn ok<T: serde::Serialize>(data: T) -> Response {
    match serde_json::to_value(data) {
        Ok(value) => Response::ok(value),
        Err(error) => Response::error(
            ErrorCode::Internal,
            format!("failed to serialize successful response data: {error}"),
        ),
    }
}

fn auth_error<E: std::fmt::Display>(error: E) -> Response {
    Response::error(ErrorCode::ValidationFailed, error.to_string())
}

fn manager_error(error: WindowManagerError) -> Response {
    match error {
        WindowManagerError::Core(core) => match core {
            WindowCoreError::Validation(message) => {
                Response::error(ErrorCode::ValidationFailed, message)
            }
            WindowCoreError::TargetNotFound(target) => Response::error(
                ErrorCode::TargetNotFound,
                format!("target '{target}' was not found"),
            ),
            WindowCoreError::TargetAmbiguous(target) => Response::error(
                ErrorCode::TargetAmbiguous,
                format!("target '{target}' matches multiple active windows"),
            ),
            WindowCoreError::CapacityExhausted => Response::error(
                ErrorCode::WindowOperationFailed,
                "window capacity exhausted",
            ),
            WindowCoreError::State(message) => {
                Response::error(ErrorCode::WindowOperationFailed, message)
            }
            WindowCoreError::Operation(message) => {
                Response::error(ErrorCode::WindowOperationFailed, message)
            }
        },
        WindowManagerError::Workspace(workspace) => match workspace {
            WorkspaceError::InvalidWorkspace(target) => Response::error(
                ErrorCode::InvalidWorkspace,
                format!("workspace target '{target}' is invalid"),
            ),
            WorkspaceError::DisplayUnavailable(message) => Response::error(
                ErrorCode::DisplayUnavailable,
                format!("X11 display is unavailable: {message}"),
            ),
            WorkspaceError::Operation(message) => {
                Response::error(ErrorCode::WindowOperationFailed, message)
            }
        },
        WindowManagerError::Persistence(persistence) => match persistence {
            PersistenceError::Validation(message) => {
                Response::error(ErrorCode::ValidationFailed, message)
            }
            PersistenceError::Storage(message) => {
                Response::error(ErrorCode::PersistenceFailed, message)
            }
            PersistenceError::WorkerUnavailable => Response::error(
                ErrorCode::PersistenceFailed,
                "persistence service worker is unavailable",
            ),
        },
        WindowManagerError::MediaAdmission(media) => match media {
            MediaAdmissionError::Validation(message) => {
                Response::error(ErrorCode::ValidationFailed, message)
            }
            MediaAdmissionError::Io(error) => Response::error(ErrorCode::MediaFailed, error),
        },
        WindowManagerError::Media(message) => Response::error(ErrorCode::MediaFailed, message),
        WindowManagerError::WebVideo(error) => {
            Response::error(ErrorCode::WebVideoFailed, error.to_string())
        }
        WindowManagerError::WebProfile(error) => {
            Response::error(ErrorCode::Internal, error.to_string())
        }
        WindowManagerError::Screenshot(message) => {
            Response::error(ErrorCode::ScreenshotFailed, message)
        }
        WindowManagerError::Operation(message) => {
            Response::error(ErrorCode::WindowOperationFailed, message)
        }
    }
}
