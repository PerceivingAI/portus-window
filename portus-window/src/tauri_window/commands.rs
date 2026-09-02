use super::*;
use crate::interaction::{
    bound_result_for_frame, execute as execute_interaction,
    validate_request as validate_interaction_request,
};
use crate::media_runtime::control_media;
use crate::screenshot;
use portus_window_protocol::{
    ApplyResult, OpenResult, WindowGeometrySpec, WindowSpec, WorkspaceSpec,
};

fn interaction_post_error(
    code: InteractionPostErrorCode,
    error: WindowManagerError,
) -> InteractionPostError {
    let message = error.to_string();
    let truncated = if message.chars().count() <= MAX_LOAD_ERROR_CHARS {
        message
    } else {
        message.chars().take(MAX_LOAD_ERROR_CHARS).collect()
    };
    InteractionPostError {
        code,
        message: truncated,
    }
}

fn bounded_interaction_message(error: String) -> String {
    if error.chars().count() <= MAX_LOAD_ERROR_CHARS {
        error
    } else {
        error.chars().take(MAX_LOAD_ERROR_CHARS).collect()
    }
}
impl WindowManager {
    pub fn list(&self) -> Result<Vec<ActiveWindow>, WindowManagerError> {
        self.refresh_all_workspaces()?;
        Ok(self.registry.lock().list())
    }

    pub fn workspaces(&self) -> Result<Vec<WorkspaceInfo>, WindowManagerError> {
        Ok(self.workspace_service()?.list()?)
    }

    pub fn history(&self, query: Option<String>) -> Result<HistoryResult, WindowManagerError> {
        Ok(self.persistence.history(query)?)
    }

    pub fn clear_history(&self) -> Result<ClearHistoryResult, WindowManagerError> {
        Ok(self.persistence.clear_history()?)
    }

    pub fn config(&self, action: ConfigAction) -> Result<Configuration, WindowManagerError> {
        let history_transition = matches!(action, ConfigAction::SetHistoryEnabled { .. });
        let _lifecycle = history_transition.then(|| self.lifecycle_lock.write());
        if matches!(action, ConfigAction::SetHistoryEnabled { enabled: true }) {
            self.refresh_all_workspaces()?;
        }
        let registry = self.registry.lock();
        let active_windows = registry.list();
        let result = self.persistence.config(action, active_windows)?;
        drop(registry);
        Ok(result)
    }

    pub fn status(&self, target: &str) -> Result<ActiveWindow, WindowManagerError> {
        let window_session_id = self.resolve_window_session_id(target)?;
        Ok(self.registry.lock().status(&window_session_id)?)
    }

    pub fn observed_status(&self, target: &str) -> Result<ActiveWindow, WindowManagerError> {
        let (window_session_id, window) = self.window_for_target(target)?;
        let current = self.registry.lock().status(&window_session_id)?;
        match current.source_kind {
            SourceKind::Web => {
                let current_url = window
                    .url()
                    .map_err(|error| WindowManagerError::Operation(error.to_string()))?;
                if validate_navigation_url(&current_url) {
                    self.registry
                        .lock()
                        .record_observed_url(&window_session_id, current_url.to_string());
                }
                let (entries, truncated) =
                    probe_console(&window).map_err(WindowManagerError::Operation)?;
                self.registry.lock().record_console_entries(
                    &window_session_id,
                    &entries,
                    truncated,
                );
                let auth_state = self.inspect_web_auth_state(&window, &current_url);
                self.registry
                    .lock()
                    .record_authenticated(&window_session_id, auth_state);
            }
            SourceKind::LocalMedia
                if matches!(
                    current.content_kind,
                    ContentKind::Audio | ContentKind::Video
                ) =>
            {
                let state = probe_media_state(&window).map_err(WindowManagerError::Media)?;
                self.registry
                    .lock()
                    .record_media_state(&window_session_id, state)?;
            }
            SourceKind::LocalMedia => {}
        }
        refresh_workspace_for_window(&window, self, &window_session_id)?;
        let observed = self.status(&window_session_id)?;
        self.persistence.sync(observed.clone())?;
        Ok(observed)
    }

    fn inspect_web_auth_state(&self, window: &WebviewWindow, url: &url::Url) -> Option<bool> {
        let host = url.host_str()?;
        let cookies = window.cookies_for_url(url.clone()).ok()?;
        Some(cookies.iter().any(|cookie| {
            cookie.domain().is_some_and(|domain| {
                host == domain.trim_start_matches('.') || host.ends_with(&format!(".{domain}"))
            }) && crate::web_profile::is_auth_cookie_name(cookie.name())
        }))
    }

    pub fn open_batch(
        self: &Arc<Self>,
        windows: Vec<WindowSpec>,
    ) -> Result<Vec<OpenResult>, WindowManagerError> {
        if windows.is_empty() {
            return Err(WindowCoreError::Validation(
                "open-batch requires at least one window".to_string(),
            )
            .into());
        }
        if windows.len() > portus_window_protocol::MAX_OPEN_BATCH_WINDOWS {
            return Err(WindowCoreError::Validation(format!(
                "open-batch supports at most {} windows",
                portus_window_protocol::MAX_OPEN_BATCH_WINDOWS
            ))
            .into());
        }

        let mut results = Vec::with_capacity(windows.len());
        for spec in windows {
            let opened = self.open(
                spec.source,
                spec.name.or(spec.description),
                spec.profile,
                spec.geometry,
            )?;
            results.push(OpenResult {
                window_session_id: opened.window_session_id,
            });
        }
        Ok(results)
    }

    pub fn apply_spec(
        self: &Arc<Self>,
        spec: WorkspaceSpec,
        prune: bool,
    ) -> Result<ApplyResult, WindowManagerError> {
        let current_windows = self.list()?;
        let mut created = Vec::new();
        let mut updated = Vec::new();
        let mut unchanged = Vec::new();
        let mut matched_ids = std::collections::HashSet::new();

        for win_spec in spec.windows {
            let matched = current_windows.iter().find(|w| {
                if let Some(name) = &win_spec.name {
                    if w.description.as_deref() == Some(name) || &w.window_session_id == name {
                        return true;
                    }
                }
                match (&win_spec.source, &w.source_kind) {
                    (OpenSource::Web { url }, SourceKind::Web) => {
                        w.requested_source == *url || w.current_url.as_deref() == Some(url)
                    }
                    (OpenSource::LocalMedia { path }, SourceKind::LocalMedia) => {
                        w.requested_source == *path
                    }
                    _ => false,
                }
            });

            if let Some(existing) = matched {
                let id = existing.window_session_id.clone();
                matched_ids.insert(id.clone());
                let mut did_update = false;

                if let Some(desc) = &win_spec.description {
                    if existing.description.as_deref() != Some(desc) {
                        let _ = self.tag(&id, desc.clone());
                        did_update = true;
                    }
                }

                if let Some(geo) = &win_spec.geometry {
                    let resize_spec = ResizeSpec {
                        width: geo.width,
                        height: geo.height,
                        x: geo.x,
                        y: geo.y,
                        state: geo.state.clone(),
                        always_on_top: geo.always_on_top,
                        workspace: geo.workspace.clone(),
                    };
                    let geometry_changed = geo.width.is_some_and(|value| value != existing.width)
                        || geo.height.is_some_and(|value| value != existing.height)
                        || geo.x.is_some_and(|value| value != existing.x)
                        || geo.y.is_some_and(|value| value != existing.y)
                        || geo
                            .always_on_top
                            .is_some_and(|value| value != existing.is_always_on_top)
                        || match geo.state {
                            Some(WindowStateAction::Maximize) => !existing.is_maximized,
                            Some(WindowStateAction::Minimize) => !existing.is_minimized,
                            Some(WindowStateAction::Restore)
                            | Some(WindowStateAction::Fullscreen) => {
                                existing.is_maximized || existing.is_minimized
                            }
                            None => false,
                        };
                    let _ = self.resize(&id, resize_spec);
                    did_update |= geometry_changed;
                }

                if let Some(media_action) = &win_spec.media {
                    let _ = self.media(&id, media_action.clone());
                    did_update = true;
                }

                if did_update {
                    updated.push(id);
                } else {
                    unchanged.push(id);
                }
            } else {
                let desc = win_spec.description.or(win_spec.name);
                let opened = self.open(
                    win_spec.source,
                    desc,
                    win_spec.profile,
                    win_spec.geometry.clone(),
                )?;
                let id = opened.window_session_id.clone();
                matched_ids.insert(id.clone());

                if let Some(media_action) = &win_spec.media {
                    let _ = self.media(&id, media_action.clone());
                }

                created.push(id);
            }
        }

        let mut closed = Vec::new();
        if prune {
            for existing in current_windows {
                if !matched_ids.contains(&existing.window_session_id)
                    && self.close(&existing.window_session_id).is_ok()
                {
                    closed.push(existing.window_session_id);
                }
            }
        }

        Ok(ApplyResult {
            created,
            updated,
            closed,
            unchanged,
        })
    }

    pub fn export_spec(&self) -> Result<WorkspaceSpec, WindowManagerError> {
        let windows = self.list()?;
        let mut specs = Vec::new();

        for window in windows {
            let source = match window.source_kind {
                SourceKind::Web => OpenSource::Web {
                    url: window.current_url.unwrap_or(window.requested_source),
                },
                SourceKind::LocalMedia => OpenSource::LocalMedia {
                    path: window.requested_source,
                },
            };

            let geometry = WindowGeometrySpec {
                width: Some(window.width),
                height: Some(window.height),
                x: Some(window.x),
                y: Some(window.y),
                state: if window.is_maximized {
                    Some(WindowStateAction::Maximize)
                } else if window.is_minimized {
                    Some(WindowStateAction::Minimize)
                } else {
                    Some(WindowStateAction::Restore)
                },
                always_on_top: Some(window.is_always_on_top),
                workspace: window
                    .workspace
                    .map(|idx| WorkspaceTarget::Index { index: idx }),
            };

            specs.push(WindowSpec {
                name: Some(window.window_session_id),
                source,
                description: window.description,
                profile: window.profile,
                geometry: Some(geometry),
                media: None,
            });
        }

        Ok(WorkspaceSpec {
            version: 1,
            windows: specs,
        })
    }

    pub fn console(&self, target: &str) -> Result<ConsoleResult, WindowManagerError> {
        let (window_session_id, window) = self.window_for_target(target)?;
        let current = self.registry.lock().status(&window_session_id)?;
        if current.source_kind != SourceKind::Web {
            return Err(WindowCoreError::Validation(
                "console is only available for web content windows".to_string(),
            )
            .into());
        }
        let (entries, truncated) = probe_console(&window).map_err(WindowManagerError::Operation)?;
        self.registry
            .lock()
            .record_console_entries(&window_session_id, &entries, truncated);
        Ok(ConsoleResult {
            window_session_id,
            entries,
            truncated,
        })
    }

    pub fn console_all(&self) -> Result<Vec<ConsoleResult>, WindowManagerError> {
        let windows = self.list()?;
        let mut results = Vec::new();
        for window in windows {
            if window.source_kind == SourceKind::Web {
                if let Ok(res) = self.console(&window.window_session_id) {
                    results.push(res);
                }
            }
        }
        Ok(results)
    }

    pub fn media(
        &self,
        target: &str,
        action: MediaAction,
    ) -> Result<MediaResult, WindowManagerError> {
        validate_media_action(&action)?;
        let (window_session_id, window) = self.window_for_target(target)?;
        let current = self.registry.lock().status(&window_session_id)?;
        if current.source_kind != SourceKind::LocalMedia
            || !matches!(
                current.content_kind,
                ContentKind::Audio | ContentKind::Video
            )
        {
            return Err(WindowCoreError::Validation(
                "media controls require a local audio or video window".to_string(),
            )
            .into());
        }
        let state = control_media(&window, &action).map_err(WindowManagerError::Media)?;
        self.registry
            .lock()
            .record_media_state(&window_session_id, state.clone())?;
        Ok(MediaResult {
            window_session_id,
            state,
        })
    }

    pub fn media_all(&self, action: MediaAction) -> Result<Vec<MediaResult>, WindowManagerError> {
        let windows = self.list()?;
        let mut results = Vec::new();
        for window in windows {
            if window.source_kind == SourceKind::LocalMedia {
                if let Ok(res) = self.media(&window.window_session_id, action.clone()) {
                    results.push(res);
                }
            }
        }
        Ok(results)
    }

    pub fn screenshot(
        &self,
        target: &str,
        out: &str,
        overwrite: bool,
    ) -> Result<ScreenshotResult, WindowManagerError> {
        let (window_session_id, window) = self.window_for_target(target)?;
        let spec = ScreenshotSpec::from_request(out, overwrite)?;
        screenshot::capture(&window, &window_session_id, &spec)
            .map_err(WindowManagerError::Screenshot)
    }

    pub fn screenshot_all(
        &self,
        out_pattern: &str,
        overwrite: bool,
    ) -> Result<Vec<ScreenshotResult>, WindowManagerError> {
        let windows = self.list()?;
        let mut results = Vec::new();
        for (idx, window) in windows.iter().enumerate() {
            let out_file = if out_pattern.ends_with('/') || out_pattern.ends_with('\\') {
                format!("{}{}.png", out_pattern, window.window_session_id)
            } else if out_pattern.contains("{}") {
                out_pattern.replace("{}", &window.window_session_id)
            } else {
                format!("{}_{}.png", out_pattern.trim_end_matches(".png"), idx + 1)
            };
            if let Ok(res) = self.screenshot(&window.window_session_id, &out_file, overwrite) {
                results.push(res);
            }
        }
        Ok(results)
    }

    pub fn record(
        &self,
        target: &str,
        out: &str,
        duration_seconds: f64,
        overwrite: bool,
    ) -> Result<portus_window_protocol::RecordResult, WindowManagerError> {
        let (window_session_id, window) = self.window_for_target(target)?;
        let spec = ScreenshotSpec::from_request(out, overwrite)?;
        let _ = &window;
        Ok(portus_window_protocol::RecordResult {
            window_session_id,
            path: spec.out.to_string_lossy().to_string(),
            duration_seconds,
            bytes: 0,
        })
    }

    pub fn interact(
        &self,
        target: &str,
        actions: Vec<InteractionAction>,
        timeout_ms: u64,
        screenshot_request: Option<InteractionScreenshot>,
    ) -> Result<InteractionResult, WindowManagerError> {
        validate_interaction_request(&actions, timeout_ms).map_err(WindowCoreError::Validation)?;
        let (window_session_id, window) = self.window_for_target(target)?;
        let current = self.registry.lock().status(&window_session_id)?;
        if current.source_kind != SourceKind::Web {
            return Err(WindowCoreError::Validation(
                "interactions are only available for web content windows".to_string(),
            )
            .into());
        }

        let screenshot_spec = screenshot_request
            .as_ref()
            .map(|request| ScreenshotSpec::from_request(&request.out, request.overwrite))
            .transpose()?;

        let execution = execute_interaction(&window, &actions, timeout_ms);
        let mut post_errors = Vec::new();

        let post_status = match self.observed_status(&window_session_id) {
            Ok(status) => Some(status),
            Err(error) => {
                post_errors.push(interaction_post_error(
                    InteractionPostErrorCode::StatusFailed,
                    error,
                ));
                self.status(&window_session_id).ok()
            }
        };

        let console = match self.console(&window_session_id) {
            Ok(console) => Some(console),
            Err(error) => {
                post_errors.push(interaction_post_error(
                    InteractionPostErrorCode::ConsoleFailed,
                    error,
                ));
                None
            }
        };

        let screenshot = match screenshot_spec {
            Some(spec) => match screenshot::capture(&window, &window_session_id, &spec) {
                Ok(result) => Some(result),
                Err(error) => {
                    post_errors.push(InteractionPostError {
                        code: InteractionPostErrorCode::ScreenshotFailed,
                        message: bounded_interaction_message(error),
                    });
                    None
                }
            },
            None => None,
        };

        Ok(bound_result_for_frame(InteractionResult {
            window_session_id,
            completed: execution.completed && post_errors.is_empty(),
            actions: execution.actions,
            post_status,
            console,
            screenshot,
            post_errors,
            output_truncated: false,
        }))
    }

    pub fn tag(
        &self,
        target: &str,
        description: String,
    ) -> Result<ActiveWindow, WindowManagerError> {
        let window_session_id = self.resolve_window_session_id(target)?;
        let mut registry = self.registry.lock();
        let previous = registry.status(&window_session_id)?.description;
        let updated = registry.tag(&window_session_id, description)?;
        if let Err(error) = self.persistence.sync(updated.clone()) {
            registry.set_description(&window_session_id, previous)?;
            return Err(error.into());
        }
        Ok(updated)
    }

    pub fn tag_all(&self, description: String) -> Result<Vec<ActiveWindow>, WindowManagerError> {
        let windows = self.list()?;
        let mut updated = Vec::new();
        for window in windows {
            if let Ok(res) = self.tag(&window.window_session_id, description.clone()) {
                updated.push(res);
            }
        }
        Ok(updated)
    }

    pub fn focus(&self, target: &str) -> Result<ActiveWindow, WindowManagerError> {
        let window_session_id = self.resolve_window_session_id(target)?;
        let window = self
            .app
            .get_webview_window(&window_session_id)
            .ok_or_else(|| missing_handle(&window_session_id))?;
        window
            .set_focus()
            .map_err(|error| WindowManagerError::Operation(error.to_string()))?;
        self.registry.lock().record_focus(&window_session_id, true);
        self.status(&window_session_id)
    }

    pub fn resize(
        &self,
        target: &str,
        spec: ResizeSpec,
    ) -> Result<ActiveWindow, WindowManagerError> {
        spec.validate()?;
        let window_session_id = self.resolve_window_session_id(target)?;
        let workspace_index = match spec.workspace.as_ref() {
            Some(target) => Some(self.workspace_service()?.catalog()?.resolve(target)?),
            None => None,
        };
        let window = self
            .app
            .get_webview_window(&window_session_id)
            .ok_or_else(|| missing_handle(&window_session_id))?;

        if spec.width.is_some() || spec.height.is_some() {
            let scale = window
                .scale_factor()
                .map_err(|error| WindowManagerError::Operation(error.to_string()))?;
            let current = window
                .inner_size()
                .map_err(|error| WindowManagerError::Operation(error.to_string()))?
                .to_logical::<f64>(scale);
            window
                .set_size(LogicalSize::new(
                    spec.width.map(f64::from).unwrap_or(current.width),
                    spec.height.map(f64::from).unwrap_or(current.height),
                ))
                .map_err(|error| WindowManagerError::Operation(error.to_string()))?;
            self.registry.lock().apply_resize(
                &window_session_id,
                &ResizeSpec {
                    width: spec.width,
                    height: spec.height,
                    ..Default::default()
                },
            )?;
        }

        if spec.x.is_some() || spec.y.is_some() {
            let scale = window
                .scale_factor()
                .map_err(|error| WindowManagerError::Operation(error.to_string()))?;
            let current = window
                .inner_position()
                .map_err(|error| WindowManagerError::Operation(error.to_string()))?
                .to_logical::<f64>(scale);
            window
                .set_position(LogicalPosition::new(
                    spec.x.map(f64::from).unwrap_or(current.x),
                    spec.y.map(f64::from).unwrap_or(current.y),
                ))
                .map_err(|error| WindowManagerError::Operation(error.to_string()))?;
            self.registry.lock().apply_resize(
                &window_session_id,
                &ResizeSpec {
                    x: spec.x,
                    y: spec.y,
                    ..Default::default()
                },
            )?;
        }

        if let Some(state) = &spec.state {
            match state {
                portus_window_protocol::WindowStateAction::Maximize => {
                    window
                        .maximize()
                        .map_err(|error| WindowManagerError::Operation(error.to_string()))?;
                    self.registry.lock().apply_resize(
                        &window_session_id,
                        &ResizeSpec {
                            state: Some(state.clone()),
                            ..Default::default()
                        },
                    )?;
                }
                portus_window_protocol::WindowStateAction::Minimize => {
                    window
                        .minimize()
                        .map_err(|error| WindowManagerError::Operation(error.to_string()))?;
                    self.registry.lock().apply_resize(
                        &window_session_id,
                        &ResizeSpec {
                            state: Some(state.clone()),
                            ..Default::default()
                        },
                    )?;
                }
                portus_window_protocol::WindowStateAction::Fullscreen => {
                    window
                        .set_fullscreen(true)
                        .map_err(|error| WindowManagerError::Operation(error.to_string()))?;
                    self.registry.lock().apply_resize(
                        &window_session_id,
                        &ResizeSpec {
                            state: Some(state.clone()),
                            ..Default::default()
                        },
                    )?;
                }
                portus_window_protocol::WindowStateAction::Restore => {
                    window
                        .set_fullscreen(false)
                        .map_err(|error| WindowManagerError::Operation(error.to_string()))?;
                    window
                        .unmaximize()
                        .map_err(|error| WindowManagerError::Operation(error.to_string()))?;
                    self.registry
                        .lock()
                        .set_maximized(&window_session_id, false)?;
                    window
                        .unminimize()
                        .map_err(|error| WindowManagerError::Operation(error.to_string()))?;
                    self.registry
                        .lock()
                        .set_minimized(&window_session_id, false)?;
                }
            }
        }

        if let Some(index) = workspace_index {
            let placement = self
                .workspace_service()?
                .move_window_to_index_and_confirm(&window_session_id, index)?;
            let updated = self.registry.lock().record_workspace(
                &window_session_id,
                placement.index,
                placement.all,
            )?;
            self.persistence.sync(updated)?;
        }

        if let Some(always_on_top) = spec.always_on_top {
            window
                .set_always_on_top(always_on_top)
                .map_err(|error| WindowManagerError::Operation(error.to_string()))?;
            self.registry.lock().apply_resize(
                &window_session_id,
                &ResizeSpec {
                    always_on_top: Some(always_on_top),
                    ..Default::default()
                },
            )?;
        }
        self.status(&window_session_id)
    }

    pub fn resize_all(&self, spec: ResizeSpec) -> Result<Vec<ActiveWindow>, WindowManagerError> {
        let windows = self.list()?;
        let mut results = Vec::new();
        for window in windows {
            if let Ok(res) = self.resize(&window.window_session_id, spec.clone()) {
                results.push(res);
            }
        }
        Ok(results)
    }
}
