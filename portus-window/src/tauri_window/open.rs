use super::*;
use crate::media::validate_presentation_navigation;
impl WindowManager {
    fn apply_initial_geometry(
        &self,
        window_session_id: &str,
        geometry: Option<&WindowGeometrySpec>,
    ) -> Result<(), WindowManagerError> {
        let Some(geometry) = geometry else {
            return Ok(());
        };
        self.resize(
            window_session_id,
            ResizeSpec {
                width: geometry.width,
                height: geometry.height,
                x: geometry.x,
                y: geometry.y,
                state: geometry.state.clone(),
                always_on_top: geometry.always_on_top,
                workspace: None,
            },
        )?;
        Ok(())
    }

    fn apply_initial_workspace(
        &self,
        window_session_id: &str,
        geometry: Option<&WindowGeometrySpec>,
    ) -> Result<(), WindowManagerError> {
        let Some(workspace) = geometry.and_then(|geometry| geometry.workspace.clone()) else {
            return Ok(());
        };
        self.resize(
            window_session_id,
            ResizeSpec {
                workspace: Some(workspace),
                ..Default::default()
            },
        )?;
        Ok(())
    }

    pub fn open(
        self: &Arc<Self>,
        source: OpenSource,
        description: Option<String>,
        profile: Option<String>,
        geometry: Option<WindowGeometrySpec>,
    ) -> Result<ActiveWindow, WindowManagerError> {
        let description = validate_description(description)?;
        let profile = validate_optional_profile(profile)?;
        // Multiple independent opens may proceed concurrently. History-mode transitions and
        // close-all operations take the write side of this gate.
        let _lifecycle = self.lifecycle_lock.read();
        let window_session_id = self.registry.lock().next_id()?;
        match source {
            OpenSource::Web { url } => self.open_web(
                window_session_id,
                url,
                description,
                profile,
                geometry.clone(),
            ),
            OpenSource::LocalMedia { path } => {
                self.open_local_media(window_session_id, path, description, profile, geometry)
            }
        }
    }

    fn open_web(
        self: &Arc<Self>,
        window_session_id: String,
        url: String,
        description: Option<String>,
        profile: Option<String>,
        geometry: Option<WindowGeometrySpec>,
    ) -> Result<ActiveWindow, WindowManagerError> {
        let parsed_url = validate_url(&url)?;
        let normalized_url = parsed_url.to_string();
        if let Some(video) = YouTubeVideo::parse(&parsed_url) {
            return self.open_web_video(
                window_session_id,
                normalized_url,
                video,
                description,
                profile,
                geometry,
            );
        }
        let profile_dir = self
            .web_profile
            .profile_data_directory(profile.as_deref())
            .map_err(|error| WindowManagerError::Operation(error.to_string()))?;
        let observation_gate = Arc::new(Mutex::new(ObservationGate::default()));

        let title_weak = Arc::downgrade(self);
        let title_gate = Arc::clone(&observation_gate);
        let title_window_id = window_session_id.clone();
        let page_weak = Arc::downgrade(self);
        let page_gate = Arc::clone(&observation_gate);
        let page_window_id = window_session_id.clone();
        let consent_weak = Arc::downgrade(self);

        let window = WebviewWindowBuilder::new(
            &self.app,
            window_session_id.clone(),
            WebviewUrl::External(url::Url::parse("about:blank").expect("about:blank is valid")),
        )
        .data_directory(profile_dir)
        .title("Portus Window")
        .inner_size(DEFAULT_WIDTH as f64, DEFAULT_HEIGHT as f64)
        .decorations(false)
        .focused(false)
        .visible(false)
        .initialization_script(CONSOLE_INIT_SCRIPT)
        .on_navigation({
            let consent_weak = consent_weak.clone();
            let window_session_id = window_session_id.clone();
            move |url| {
                if let Some(manager) = consent_weak.upgrade() {
                    if let Some(true) =
                        manager.handle_auth_consent_navigation(&window_session_id, url)
                    {
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
            if payload.url().as_str() == "about:blank" {
                return;
            }
            let observation = match payload.event() {
                PageLoadEvent::Started => WebObservation::PageStarted(payload.url().to_string()),
                PageLoadEvent::Finished => WebObservation::PageFinished(payload.url().to_string()),
            };
            record_or_buffer(&page_weak, &page_gate, &page_window_id, observation);
        })
        .build()
        .map_err(|error| WindowManagerError::Operation(error.to_string()))?;

        self.attach_window_events(&window, &window_session_id);
        if let Err(error) = attach_platform_load_failure_observer(
            &window,
            self,
            &window_session_id,
            &observation_gate,
        ) {
            let _ = window.destroy();
            return Err(WindowManagerError::Operation(error));
        }
        self.registry.lock().insert_opened_web(
            window_session_id.clone(),
            normalized_url.clone(),
            description,
            profile,
        );
        #[cfg(target_os = "linux")]
        self.registry
            .lock()
            .record_page_started(&window_session_id, normalized_url);
        #[cfg(target_os = "linux")]
        window
            .with_webview(move |platform_webview| {
                use webkit2gtk::WebViewExt;
                platform_webview.inner().load_uri(parsed_url.as_str());
            })
            .map_err(|error| WindowManagerError::Operation(error.to_string()))?;
        #[cfg(not(target_os = "linux"))]
        window
            .navigate(parsed_url)
            .map_err(|error| WindowManagerError::Operation(error.to_string()))?;

        // Apply geometry while hidden, then realize the native window before attaching
        // workspace tracking. X11 requires a realized native handle for workspace watching;
        // workspace placement is therefore applied after show/focus but before open returns.
        if let Err(error) = self.apply_initial_geometry(&window_session_id, geometry.as_ref()) {
            self.registry.lock().remove(&window_session_id);
            let _ = window.destroy();
            return Err(error);
        }

        if let Err(error) = window.show().and_then(|_| window.set_focus()) {
            self.registry.lock().remove(&window_session_id);
            let _ = window.destroy();
            return Err(WindowManagerError::Operation(error.to_string()));
        }
        self.registry.lock().record_focus(&window_session_id, true);

        if let Err(error) = attach_workspace_tracking(&window, self, &window_session_id) {
            self.registry.lock().remove(&window_session_id);
            let _ = window.destroy();
            return Err(error);
        }
        if let Err(error) = self.apply_initial_workspace(&window_session_id, geometry.as_ref()) {
            self.workspace_service()?.unwatch_window(&window_session_id);
            self.registry.lock().remove(&window_session_id);
            let _ = window.destroy();
            return Err(error);
        }

        let initial = self.status(&window_session_id)?;
        if let Err(error) = self.persistence.track_open(initial) {
            self.registry.lock().remove(&window_session_id);
            let _ = window.destroy();
            return Err(error.into());
        }
        activate_observations(self, &window_session_id, &observation_gate);
        self.status(&window_session_id)
    }

    fn open_web_video(
        self: &Arc<Self>,
        window_session_id: String,
        requested_url: String,
        video: YouTubeVideo,
        description: Option<String>,
        profile: Option<String>,
        geometry: Option<WindowGeometrySpec>,
    ) -> Result<ActiveWindow, WindowManagerError> {
        let registration = self.web_video.register(&window_session_id, video)?;
        let expected_view = registration.view_url.clone();
        let profile_dir = self
            .web_profile
            .profile_data_directory(profile.as_deref())
            .map_err(|error| WindowManagerError::Operation(error.to_string()))?;
        let observation_gate = Arc::new(Mutex::new(ObservationGate::default()));

        let title_weak = Arc::downgrade(self);
        let title_gate = Arc::clone(&observation_gate);
        let title_window_id = window_session_id.clone();
        let page_weak = Arc::downgrade(self);
        let page_gate = Arc::clone(&observation_gate);
        let page_window_id = window_session_id.clone();
        let consent_weak = Arc::downgrade(self);

        let window = match WebviewWindowBuilder::new(
            &self.app,
            window_session_id.clone(),
            WebviewUrl::External(registration.view_url.clone()),
        )
        .data_directory(profile_dir)
        .title("YouTube Video")
        .inner_size(DEFAULT_WIDTH as f64, DEFAULT_HEIGHT as f64)
        .decorations(false)
        .focused(false)
        .visible(false)
        .initialization_script(CONSOLE_INIT_SCRIPT)
        .on_navigation({
            let consent_weak = consent_weak.clone();
            let window_session_id = window_session_id.clone();
            move |url| {
                if let Some(manager) = consent_weak.upgrade() {
                    if let Some(true) =
                        manager.handle_auth_consent_navigation(&window_session_id, url)
                    {
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
        {
            Ok(window) => window,
            Err(error) => {
                self.web_video.revoke_window(&window_session_id);
                return Err(WindowManagerError::Operation(error.to_string()));
            }
        };

        self.attach_window_events(&window, &window_session_id);
        if let Err(error) = attach_platform_load_failure_observer(
            &window,
            self,
            &window_session_id,
            &observation_gate,
        ) {
            let _ = window.destroy();
            self.web_video.revoke_window(&window_session_id);
            return Err(WindowManagerError::Operation(error));
        }

        self.registry.lock().insert_opened_web_video(
            window_session_id.clone(),
            requested_url,
            registration.embed_url,
            description,
            profile,
        );
        if let Err(error) = self.apply_initial_geometry(&window_session_id, geometry.as_ref()) {
            self.registry.lock().remove(&window_session_id);
            self.web_video.revoke_window(&window_session_id);
            let _ = window.destroy();
            return Err(error);
        }

        if let Err(error) = window.show().and_then(|_| window.set_focus()) {
            self.registry.lock().remove(&window_session_id);
            self.web_video.revoke_window(&window_session_id);
            let _ = window.destroy();
            return Err(WindowManagerError::Operation(error.to_string()));
        }
        self.registry.lock().record_focus(&window_session_id, true);

        if let Err(error) = attach_workspace_tracking(&window, self, &window_session_id) {
            self.registry.lock().remove(&window_session_id);
            let _ = window.destroy();
            return Err(error);
        }
        if let Err(error) = self.apply_initial_workspace(&window_session_id, geometry.as_ref()) {
            self.workspace_service()?.unwatch_window(&window_session_id);
            self.registry.lock().remove(&window_session_id);
            let _ = window.destroy();
            return Err(error);
        }

        let initial = self.status(&window_session_id)?;
        if let Err(error) = self.persistence.track_open(initial) {
            self.registry.lock().remove(&window_session_id);
            self.web_video.revoke_window(&window_session_id);
            let _ = window.destroy();
            return Err(error.into());
        }
        activate_observations(self, &window_session_id, &observation_gate);
        self.status(&window_session_id)
    }

    fn open_local_media(
        self: &Arc<Self>,
        window_session_id: String,
        path: String,
        description: Option<String>,
        profile: Option<String>,
        geometry: Option<WindowGeometrySpec>,
    ) -> Result<ActiveWindow, WindowManagerError> {
        let admitted = self.media.admit(&window_session_id, &path)?;
        let media_url = match self.media.presentation_url(&admitted) {
            Ok(url) => url,
            Err(error) => {
                self.media.revoke_window(&window_session_id);
                return Err(error.into());
            }
        };
        let observation_gate = Arc::new(Mutex::new(ObservationGate::default()));
        let page_weak = Arc::downgrade(self);
        let page_gate = Arc::clone(&observation_gate);
        let page_window_id = window_session_id.clone();
        let token = admitted.token.clone();

        let window = match WebviewWindowBuilder::new(
            &self.app,
            window_session_id.clone(),
            WebviewUrl::External(media_url),
        )
        .title(&admitted.title)
        .inner_size(DEFAULT_WIDTH as f64, DEFAULT_HEIGHT as f64)
        .decorations(false)
        .focused(false)
        .visible(false)
        .on_navigation({
            let app = self.app.clone();
            let consent_weak = Arc::downgrade(self);
            let window_session_id = window_session_id.clone();
            move |url| {
                if let Some(manager) = consent_weak.upgrade() {
                    if let Some(true) =
                        manager.handle_auth_consent_navigation(&window_session_id, url)
                    {
                        return false;
                    }
                }
                if url.scheme() == "portus-window-action" && url.host_str() == Some("fullscreen") {
                    if let Some(window) = app.get_webview_window(&window_session_id) {
                        match window.is_fullscreen() {
                            Ok(is_fullscreen) => {
                                if let Err(error) = window.set_fullscreen(!is_fullscreen) {
                                    eprintln!("Portus Window could not toggle fullscreen: {error}");
                                }
                            }
                            Err(error) => {
                                eprintln!(
                                    "Portus Window could not inspect fullscreen state: {error}"
                                );
                            }
                        }
                    }
                    return false;
                }
                validate_presentation_navigation(url, &token)
            }
        })
        .on_new_window(|_, _| NewWindowResponse::Deny)
        .on_page_load(move |_window, payload| {
            let observation = match payload.event() {
                PageLoadEvent::Started => WebObservation::PageStarted(payload.url().to_string()),
                PageLoadEvent::Finished => WebObservation::PageFinished(payload.url().to_string()),
            };
            record_or_buffer(&page_weak, &page_gate, &page_window_id, observation);
        })
        .build()
        {
            Ok(window) => window,
            Err(error) => {
                self.media.revoke_window(&window_session_id);
                return Err(WindowManagerError::Operation(error.to_string()));
            }
        };

        self.attach_window_events(&window, &window_session_id);
        if let Err(error) = attach_platform_load_failure_observer(
            &window,
            self,
            &window_session_id,
            &observation_gate,
        ) {
            let _ = window.destroy();
            self.media.revoke_window(&window_session_id);
            return Err(WindowManagerError::Operation(error));
        }

        if let Err(error) = self.registry.lock().insert_opened_media(
            window_session_id.clone(),
            admitted.requested_source,
            admitted.content_kind,
            admitted.title,
            description,
            profile,
        ) {
            let _ = window.destroy();
            self.media.revoke_window(&window_session_id);
            return Err(error.into());
        }

        if let Err(error) = self.apply_initial_geometry(&window_session_id, geometry.as_ref()) {
            self.registry.lock().remove(&window_session_id);
            self.media.revoke_window(&window_session_id);
            let _ = window.destroy();
            return Err(error);
        }

        if let Err(error) = window.show().and_then(|_| window.set_focus()) {
            self.registry.lock().remove(&window_session_id);
            self.media.revoke_window(&window_session_id);
            let _ = window.destroy();
            return Err(WindowManagerError::Operation(error.to_string()));
        }
        self.registry.lock().record_focus(&window_session_id, true);

        if let Err(error) = attach_workspace_tracking(&window, self, &window_session_id) {
            self.registry.lock().remove(&window_session_id);
            let _ = window.destroy();
            return Err(error);
        }
        if let Err(error) = self.apply_initial_workspace(&window_session_id, geometry.as_ref()) {
            self.workspace_service()?.unwatch_window(&window_session_id);
            self.registry.lock().remove(&window_session_id);
            let _ = window.destroy();
            return Err(error);
        }

        let initial = self.status(&window_session_id)?;
        if let Err(error) = self.persistence.track_open(initial) {
            self.registry.lock().remove(&window_session_id);
            self.media.revoke_window(&window_session_id);
            let _ = window.destroy();
            return Err(error.into());
        }
        activate_observations(self, &window_session_id, &observation_gate);
        self.status(&window_session_id)
    }
}

fn validate_optional_profile(
    profile: Option<String>,
) -> Result<Option<String>, WindowManagerError> {
    if let Some(ref name) = profile {
        crate::web_profile::validate_profile_name(name)
            .map_err(|e| WindowCoreError::Validation(e.to_string()))?;
    }
    Ok(profile)
}
