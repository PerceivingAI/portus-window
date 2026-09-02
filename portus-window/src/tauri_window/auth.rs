use super::*;

impl WindowManager {
    /// Strategy 2 external broker import path: rebuild one ordinary web window on a dedicated
    /// session-scoped WebKit profile (`auth-session-profiles/<window_session_id>`) while preserving
    /// its public/native window_session_id. This isolates external browser tokens to the approved
    /// destination window without leaking into the shared Strategy 1 profile.
    pub(crate) fn upgrade_web_window_to_authenticated_profile(
        self: &Arc<Self>,
        window_session_id: &str,
    ) -> Result<WebviewWindow, WindowManagerError> {
        let _lifecycle = self.lifecycle_lock.write();
        let snapshot = self.registry.lock().status(window_session_id)?;
        if snapshot.source_kind != SourceKind::Web
            || snapshot.content_kind != ContentKind::Web
            || self.web_video.contains_window(window_session_id)
        {
            return Err(WindowManagerError::Operation(
                "ordinary authenticated profile upgrade does not accept managed YouTube windows"
                    .to_string(),
            ));
        }
        if self
            .authenticated_profile_windows
            .lock()
            .contains_key(window_session_id)
        {
            return self
                .app
                .get_webview_window(window_session_id)
                .ok_or_else(|| missing_handle(window_session_id));
        }

        let parsed_url = validate_url(&snapshot.requested_source)?;
        let profile_dir = self
            .web_profile
            .prepare_auth_session_directory(window_session_id)
            .map_err(|error| WindowManagerError::Operation(error.to_string()))?;
        let old_window = self
            .app
            .get_webview_window(window_session_id)
            .ok_or_else(|| missing_handle(window_session_id))?;
        let old_control = self
            .window_event_controls
            .lock()
            .get(window_session_id)
            .cloned()
            .ok_or_else(|| {
                WindowManagerError::Operation(
                    "window lifecycle control is unavailable for authenticated upgrade".to_string(),
                )
            })?;
        old_control.suppress_destroy.store(true, Ordering::Release);
        self.window_event_controls.lock().remove(window_session_id);
        if let Some(workspace) = &self.workspace {
            workspace.unwatch_window(window_session_id);
        }
        if let Err(error) = old_window.destroy() {
            old_control.suppress_destroy.store(false, Ordering::Release);
            let _ = self
                .web_profile
                .remove_auth_session_directory(window_session_id);
            return Err(WindowManagerError::Operation(error.to_string()));
        }

        let observation_gate = Arc::new(Mutex::new(ObservationGate::default()));
        let title_weak = Arc::downgrade(self);
        let title_gate = Arc::clone(&observation_gate);
        let title_window_id = window_session_id.to_string();
        let consent_weak = Arc::downgrade(self);
        let page_weak = Arc::downgrade(self);
        let page_gate = Arc::clone(&observation_gate);
        let page_window_id = window_session_id.to_string();
        let consent_window_session_id = window_session_id.to_string();

        let replacement = WebviewWindowBuilder::new(
            &self.app,
            window_session_id.to_string(),
            WebviewUrl::External(parsed_url),
        )
        .data_directory(profile_dir)
        .title(snapshot.title.clone())
        .inner_size(snapshot.width as f64, snapshot.height as f64)
        .position(snapshot.x as f64, snapshot.y as f64)
        .decorations(false)
        .focused(false)
        .visible(false)
        .initialization_script(CONSOLE_INIT_SCRIPT)
        .on_navigation({
            let consent_weak = consent_weak.clone();
            let window_session_id = consent_window_session_id.clone();
            move |url| {
                if let Some(manager) = consent_weak.upgrade() {
                    if let Some(true) = manager.handle_auth_consent_navigation(&window_session_id, url) {
                        return false;
                    }
                }
                validate_navigation_url(url)
            }
        })
        .on_new_window(|_, _| NewWindowResponse::Deny)
        .on_document_title_changed(move |_window, title| {
            record_or_buffer(
                &title_weak,
                &title_gate,
                &title_window_id,
                WebObservation::Title(title),
            );
        })
        .on_page_load(move |_window, payload| {
            let observation = match payload.event() {
                PageLoadEvent::Started => WebObservation::PageStarted(payload.url().to_string()),
                PageLoadEvent::Finished => WebObservation::PageFinished(payload.url().to_string()),
            };
            record_or_buffer(&page_weak, &page_gate, &page_window_id, observation);
        })
        .build()
        .map_err(|error| {
            self.registry.lock().remove(window_session_id);
            self.persistence.close_async(snapshot.clone(), CloseReason::Destroyed);
            let _ = self.web_profile.remove_auth_session_directory(window_session_id);
            WindowManagerError::Operation(format!(
                "authenticated replacement webview creation failed after old webview teardown: {error}"
            ))
        })?;

        self.attach_window_events(&replacement, window_session_id);
        if let Err(error) = attach_platform_load_failure_observer(
            &replacement,
            self,
            window_session_id,
            &observation_gate,
        ) {
            let _ = replacement.destroy();
            return Err(WindowManagerError::Operation(error));
        }
        self.authenticated_profile_windows
            .lock()
            .insert(window_session_id.to_string(), ());

        if let Err(error) = replacement.show().and_then(|_| replacement.set_focus()) {
            let _ = replacement.destroy();
            return Err(WindowManagerError::Operation(error.to_string()));
        }
        if snapshot.is_always_on_top {
            replacement
                .set_always_on_top(true)
                .map_err(|error| WindowManagerError::Operation(error.to_string()))?;
        }
        if snapshot.is_maximized {
            replacement
                .maximize()
                .map_err(|error| WindowManagerError::Operation(error.to_string()))?;
        } else if snapshot.is_minimized {
            replacement
                .minimize()
                .map_err(|error| WindowManagerError::Operation(error.to_string()))?;
        }

        self.registry.lock().record_focus(window_session_id, true);
        activate_observations(self, window_session_id, &observation_gate);
        attach_workspace_tracking(&replacement, self, window_session_id)?;
        if let Ok(updated) = self.registry.lock().status(window_session_id) {
            self.persistence.sync_async(updated);
        }
        Ok(replacement)
    }

    pub(crate) fn reload_authenticated_web_window(
        &self,
        window_session_id: &str,
    ) -> Result<(), WindowManagerError> {
        if !self
            .authenticated_profile_windows
            .lock()
            .contains_key(window_session_id)
        {
            return Err(WindowManagerError::Operation(
                "window is not using an authenticated WebKit profile".to_string(),
            ));
        }
        let window = self
            .app
            .get_webview_window(window_session_id)
            .ok_or_else(|| missing_handle(window_session_id))?;
        window
            .reload()
            .map_err(|error| WindowManagerError::Operation(error.to_string()))
    }

    /// Strategy 2 external broker import path for YouTube: rebuild the managed YouTube window
    /// on an isolated session profile (`auth-session-profiles/<window_session_id>`) and activate
    /// authenticated embed mode.
    #[allow(dead_code)]
    pub(crate) fn upgrade_web_video_to_authenticated_profile(
        self: &Arc<Self>,
        window_session_id: &str,
    ) -> Result<WebviewWindow, WindowManagerError> {
        let _lifecycle = self.lifecycle_lock.write();
        let snapshot = self.registry.lock().status(window_session_id)?;
        if !self.web_video.contains_window(window_session_id) {
            return Err(WindowManagerError::Operation(
                "authenticated YouTube upgrade requires a managed YouTube window".to_string(),
            ));
        }
        if self
            .authenticated_profile_windows
            .lock()
            .contains_key(window_session_id)
        {
            return self
                .app
                .get_webview_window(window_session_id)
                .ok_or_else(|| missing_handle(window_session_id));
        }
        let view_url = self.web_video.view_url_for_window(window_session_id)?;
        let expected_view = view_url.clone();
        let profile_dir = self
            .web_profile
            .prepare_auth_session_directory(window_session_id)
            .map_err(|error| WindowManagerError::Operation(error.to_string()))?;
        let old_window = self
            .app
            .get_webview_window(window_session_id)
            .ok_or_else(|| missing_handle(window_session_id))?;
        let old_control = self
            .window_event_controls
            .lock()
            .get(window_session_id)
            .cloned()
            .ok_or_else(|| {
                WindowManagerError::Operation(
                    "window lifecycle control is unavailable for authenticated YouTube upgrade"
                        .to_string(),
                )
            })?;
        old_control.suppress_destroy.store(true, Ordering::Release);
        self.window_event_controls.lock().remove(window_session_id);
        if let Some(workspace) = &self.workspace {
            workspace.unwatch_window(window_session_id);
        }
        if let Err(error) = old_window.destroy() {
            old_control.suppress_destroy.store(false, Ordering::Release);
            self.window_event_controls
                .lock()
                .insert(window_session_id.to_string(), Arc::clone(&old_control));
            let _ = self
                .web_profile
                .remove_auth_session_directory(window_session_id);
            return Err(WindowManagerError::Operation(error.to_string()));
        }
        let observation_gate = Arc::new(Mutex::new(ObservationGate::default()));
        let title_weak = Arc::downgrade(self);
        let title_gate = Arc::clone(&observation_gate);
        let title_window_id = window_session_id.to_string();
        let consent_weak = Arc::downgrade(self);
        let page_weak = Arc::downgrade(self);
        let page_gate = Arc::clone(&observation_gate);
        let page_window_id = window_session_id.to_string();
        let consent_window_session_id = window_session_id.to_string();
        let replacement = WebviewWindowBuilder::new(
            &self.app,
            window_session_id.to_string(),
            WebviewUrl::External(view_url),
        )
        .data_directory(profile_dir)
        .title(snapshot.title.clone())
        .inner_size(snapshot.width as f64, snapshot.height as f64)
        .position(snapshot.x as f64, snapshot.y as f64)
        .decorations(false)
        .focused(false)
        .visible(false)
        .initialization_script(CONSOLE_INIT_SCRIPT)
        .on_navigation({
            let consent_weak = consent_weak.clone();
            let window_session_id = consent_window_session_id.clone();
            move |url| {
                if let Some(manager) = consent_weak.upgrade() {
                    if let Some(true) = manager.handle_auth_consent_navigation(&window_session_id, url) {
                        return false;
                    }
                }
                validate_web_video_navigation(url, &expected_view)
            }
        })
        .on_new_window(|_, _| NewWindowResponse::Deny)
        .on_document_title_changed(move |_window, title| {
            record_or_buffer(
                &title_weak,
                &title_gate,
                &title_window_id,
                WebObservation::Title(title),
            );
        })
        .on_page_load(move |_window, payload| {
            let observation = match payload.event() {
                PageLoadEvent::Started => WebObservation::PageStarted(payload.url().to_string()),
                PageLoadEvent::Finished => WebObservation::PageFinished(payload.url().to_string()),
            };
            record_or_buffer(&page_weak, &page_gate, &page_window_id, observation);
        })
        .build()
        .map_err(|error| {
            self.registry.lock().remove(window_session_id);
            self.persistence.close_async(snapshot.clone(), CloseReason::Destroyed);
            let _ = self.web_profile.remove_auth_session_directory(window_session_id);
            WindowManagerError::Operation(format!(
                "authenticated YouTube replacement creation failed after old webview teardown: {error}"
            ))
        })?;
        self.attach_window_events(&replacement, window_session_id);
        if let Err(error) = attach_platform_load_failure_observer(
            &replacement,
            self,
            window_session_id,
            &observation_gate,
        ) {
            let _ = replacement.destroy();
            let _ = self
                .web_profile
                .remove_auth_session_directory(window_session_id);
            return Err(WindowManagerError::Operation(error));
        }
        self.authenticated_profile_windows
            .lock()
            .insert(window_session_id.to_string(), ());
        if let Err(error) = replacement.show().and_then(|_| replacement.set_focus()) {
            let _ = replacement.destroy();
            return Err(WindowManagerError::Operation(error.to_string()));
        }
        if snapshot.is_always_on_top {
            replacement
                .set_always_on_top(true)
                .map_err(|e| WindowManagerError::Operation(e.to_string()))?;
        }
        if snapshot.is_maximized {
            replacement
                .maximize()
                .map_err(|e| WindowManagerError::Operation(e.to_string()))?;
        } else if snapshot.is_minimized {
            replacement
                .minimize()
                .map_err(|e| WindowManagerError::Operation(e.to_string()))?;
        }
        self.registry.lock().record_focus(window_session_id, true);
        activate_observations(self, window_session_id, &observation_gate);
        attach_workspace_tracking(&replacement, self, window_session_id)?;
        if let Ok(updated) = self.registry.lock().status(window_session_id) {
            self.persistence.sync_async(updated);
        }
        Ok(replacement)
    }

    pub(crate) fn is_web_video_window(&self, window_session_id: &str) -> bool {
        self.web_video.contains_window(window_session_id)
    }

    pub(crate) fn enable_authenticated_web_video(
        &self,
        window_session_id: &str,
    ) -> Result<(), WindowManagerError> {
        self.web_video
            .enable_authenticated_embed(window_session_id)?;
        Ok(())
    }

    pub(crate) fn disable_authenticated_web_video(
        &self,
        window_session_id: &str,
    ) -> Result<(), WindowManagerError> {
        self.web_video
            .disable_authenticated_embed(window_session_id)?;
        Ok(())
    }
}
