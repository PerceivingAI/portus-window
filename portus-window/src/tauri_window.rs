use crate::database::{DatabaseService, PersistenceError};
use crate::media::MediaAuthority;
use crate::media_runtime::probe_media_state;
use crate::observability::{probe_console, CONSOLE_INIT_SCRIPT};
use crate::web_profile::WebProfile;
use crate::web_video::{
    validate_web_video_navigation, WebVideoAuthority, WebVideoError, YouTubeVideo,
};
use crate::window_core::{
    validate_description, validate_media_action, validate_navigation_url, validate_url, ResizeSpec,
    ScreenshotSpec, WindowCoreError, WindowRegistry, DEFAULT_HEIGHT, DEFAULT_WIDTH,
};
use crate::workspace::{WorkspaceError, WorkspacePlacement, WorkspaceService};
use parking_lot::{Mutex, RwLock};
use portus_window_protocol::{
    ActiveWindow, ClearHistoryResult, CloseReason, ConfigAction, Configuration, ConsoleResult,
    ContentKind, HistoryResult, InteractionAction, InteractionPostError, InteractionPostErrorCode,
    InteractionResult, InteractionScreenshot, MediaAction, MediaResult, OpenSource,
    ScreenshotResult, SourceKind, WindowGeometrySpec, WindowStateAction, WorkspaceInfo,
    WorkspaceTarget, MAX_LOAD_ERROR_CHARS,
};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

type AuthBeforeCloseHook = dyn Fn(&str) -> Result<(), String> + Send + Sync;
type AuthAfterDestroyHook = dyn Fn(&str) + Send + Sync;
use tauri::{
    webview::{NewWindowResponse, PageLoadEvent},
    AppHandle, LogicalPosition, LogicalSize, Manager, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder, WindowEvent,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WindowManagerError {
    #[error(transparent)]
    Core(#[from] WindowCoreError),
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error(transparent)]
    Persistence(#[from] PersistenceError),
    #[error(transparent)]
    MediaAdmission(#[from] crate::media::MediaAdmissionError),
    #[error(transparent)]
    WebVideo(#[from] WebVideoError),
    #[error(transparent)]
    WebProfile(#[from] crate::web_profile::WebProfileError),
    #[error("media control failed: {0}")]
    Media(String),
    #[error("screenshot failed: {0}")]
    Screenshot(String),
    #[error("window operation failed: {0}")]
    Operation(String),
}

#[derive(Default)]
struct ObservationGate {
    active: bool,
    pending: Vec<WebObservation>,
}

#[derive(Debug)]
enum WebObservation {
    PageStarted(String),
    PageFinished(String),
    #[cfg(target_os = "linux")]
    PageFailed {
        url: String,
        error: String,
    },
    Title(String),
}

#[derive(Default)]
pub(crate) struct WindowEventControl {
    pub(crate) suppress_destroy: AtomicBool,
}

pub struct WindowManager {
    app: AppHandle,
    workspace: Option<Arc<WorkspaceService>>,
    persistence: Arc<DatabaseService>,
    media: Arc<MediaAuthority>,
    web_profile: Arc<WebProfile>,
    web_video: Arc<WebVideoAuthority>,
    registry: Mutex<WindowRegistry>,
    lifecycle_lock: Arc<parking_lot::RwLock<()>>,
    pending_close_reasons: Mutex<BTreeMap<String, CloseReason>>,
    window_event_controls: Mutex<BTreeMap<String, Arc<WindowEventControl>>>,
    authenticated_profile_windows: Mutex<BTreeMap<String, ()>>,
    auth_before_close: RwLock<Option<Arc<AuthBeforeCloseHook>>>,
    auth_after_destroy: RwLock<Option<Arc<AuthAfterDestroyHook>>>,
    auth_consent: RwLock<Option<Weak<crate::auth_session::AuthConsentController>>>,
}

impl WindowManager {
    pub fn new(
        app: AppHandle,
        workspace: Option<Arc<WorkspaceService>>,
        persistence: Arc<DatabaseService>,
        media: Arc<MediaAuthority>,
        web_profile: Arc<WebProfile>,
        web_video: Arc<WebVideoAuthority>,
    ) -> Self {
        Self {
            app,
            workspace,
            persistence,
            media,
            web_profile,
            web_video,
            registry: Mutex::new(WindowRegistry::default()),
            lifecycle_lock: Arc::new(parking_lot::RwLock::new(())),
            pending_close_reasons: Mutex::new(BTreeMap::new()),
            window_event_controls: Mutex::new(BTreeMap::new()),
            authenticated_profile_windows: Mutex::new(BTreeMap::new()),
            auth_before_close: RwLock::new(None),
            auth_after_destroy: RwLock::new(None),
            auth_consent: RwLock::new(None),
        }
    }

    fn attach_window_events(self: &Arc<Self>, window: &WebviewWindow, window_session_id: &str) {
        let event_window_id = window_session_id.to_string();
        let weak_manager = Arc::downgrade(self);
        window.on_window_event(move |event| {
            let Some(manager) = weak_manager.upgrade() else {
                return;
            };
            let control = manager
                .window_event_controls
                .lock()
                .get(&event_window_id)
                .cloned()
                .unwrap_or_default();
            match event {
                WindowEvent::Focused(focused) => {
                    manager
                        .registry
                        .lock()
                        .record_focus(&event_window_id, *focused);
                }
                WindowEvent::CloseRequested { api, .. } => {
                    if let Err(error) = manager.prepare_authenticated_window_close(&event_window_id)
                    {
                        eprintln!(
                            "Portus Window authenticated-session close cleanup failed: {error}"
                        );
                        api.prevent_close();
                        return;
                    }
                    if manager.registry.lock().is_last_web_window(&event_window_id) {
                        if let Some(window) = manager.app.get_webview_window(&event_window_id) {
                            if let Err(error) = prune_web_cache(&window) {
                                eprintln!(
                                    "Portus Window cache prune failed during user close: {error}"
                                );
                            }
                        }
                    }
                }
                WindowEvent::Destroyed => {
                    if control.suppress_destroy.load(Ordering::Acquire) {
                        return;
                    }
                    if let Some(removed) = manager.registry.lock().remove(&event_window_id) {
                        let reason = manager
                            .pending_close_reasons
                            .lock()
                            .remove(&event_window_id)
                            .unwrap_or(CloseReason::Destroyed);
                        manager.persistence.close_async(removed, reason);
                    }
                    if let Some(workspace) = &manager.workspace {
                        workspace.unwatch_window(&event_window_id);
                    }
                    manager.media.revoke_window(&event_window_id);
                    manager.web_video.revoke_window(&event_window_id);
                    let auth_lifecycle_installed = manager.auth_after_destroy.read().is_some();
                    manager.reconcile_authenticated_window_destroyed(&event_window_id);
                    if !auth_lifecycle_installed
                        && manager
                            .authenticated_profile_windows
                            .lock()
                            .contains_key(&event_window_id)
                    {
                        if let Err(error) = manager
                            .web_profile
                            .remove_auth_session_directory(&event_window_id)
                        {
                            eprintln!(
                                "Portus Window isolated profile removal failed on destroy: {error}"
                            );
                        } else {
                            manager
                                .authenticated_profile_windows
                                .lock()
                                .remove(&event_window_id);
                        }
                    }
                    manager
                        .window_event_controls
                        .lock()
                        .remove(&event_window_id);
                }
                _ => {}
            }
        });
    }

    pub fn install_auth_consent_controller(
        &self,
        controller: Weak<crate::auth_session::AuthConsentController>,
    ) {
        *self.auth_consent.write() = Some(controller);
    }

    pub(crate) fn handle_auth_consent_navigation(
        &self,
        window_session_id: &str,
        url: &url::Url,
    ) -> Option<bool> {
        let (token, approve) = crate::auth_session::consent_navigation_decision(url)?;
        let controller = self.auth_consent.read().as_ref()?.upgrade()?;
        Some(controller.decide_for_window(&token, window_session_id, approve))
    }

    pub fn install_auth_lifecycle_hooks(
        &self,
        before_close: Arc<AuthBeforeCloseHook>,
        after_destroy: Arc<AuthAfterDestroyHook>,
    ) {
        *self.auth_before_close.write() = Some(before_close);
        *self.auth_after_destroy.write() = Some(after_destroy);
    }

    pub(crate) fn prepare_authenticated_window_close(
        &self,
        window_session_id: &str,
    ) -> Result<(), WindowManagerError> {
        let hook = self.auth_before_close.read().clone();
        if let Some(hook) = hook {
            hook(window_session_id).map_err(WindowManagerError::Operation)?;
        }
        Ok(())
    }

    fn reconcile_authenticated_window_destroyed(&self, window_session_id: &str) {
        if let Some(hook) = self.auth_after_destroy.read().clone() {
            hook(window_session_id);
        }
    }
    pub(crate) fn auth_cookie_window(
        &self,
        window_session_id: &str,
    ) -> Result<WebviewWindow, WindowManagerError> {
        self.app
            .get_webview_window(window_session_id)
            .ok_or_else(|| missing_handle(window_session_id))
    }

    pub(crate) fn auth_cookie_handle_exact(
        &self,
        window_session_id: &str,
    ) -> Option<WebviewWindow> {
        self.app.get_webview_window(window_session_id)
    }

    pub(crate) fn is_authenticated_profile_window(&self, window_session_id: &str) -> bool {
        self.authenticated_profile_windows
            .lock()
            .contains_key(window_session_id)
    }
    pub(crate) fn purge_authenticated_profile_directory(
        &self,
        window_session_id: &str,
    ) -> Result<(), WindowManagerError> {
        self.web_profile
            .remove_auth_session_directory(window_session_id)
            .map_err(WindowManagerError::WebProfile)
    }

    fn window_for_target(
        &self,
        target: &str,
    ) -> Result<(String, WebviewWindow), WindowManagerError> {
        let window_session_id = self.resolve_window_session_id(target)?;
        let window = self
            .app
            .get_webview_window(&window_session_id)
            .ok_or_else(|| missing_handle(&window_session_id))?;
        Ok((window_session_id, window))
    }

    fn refresh_all_workspaces(&self) -> Result<(), WindowManagerError> {
        let ids: Vec<String> = self
            .registry
            .lock()
            .list()
            .into_iter()
            .map(|window| window.window_session_id)
            .collect();
        for window_session_id in ids {
            let window = self
                .app
                .get_webview_window(&window_session_id)
                .ok_or_else(|| missing_handle(&window_session_id))?;
            refresh_workspace_for_window(&window, self, &window_session_id)?;
        }
        Ok(())
    }

    fn workspace_service(&self) -> Result<&Arc<WorkspaceService>, WindowManagerError> {
        self.workspace.as_ref().ok_or_else(|| {
            WorkspaceError::DisplayUnavailable("workspace service unavailable".to_string()).into()
        })
    }
    pub(crate) fn resolve_window_session_id(
        &self,
        target: &str,
    ) -> Result<String, WindowManagerError> {
        if let Some(workspace_target) = target.strip_prefix("workspace:") {
            let service = self.workspace_service()?;
            let catalog = service.catalog()?;
            let target = if let Ok(index) = workspace_target.parse::<u32>() {
                WorkspaceTarget::Index { index }
            } else {
                WorkspaceTarget::Name {
                    name: workspace_target.to_string(),
                }
            };
            let index = catalog.resolve(&target)?;
            return Ok(self.registry.lock().resolve_workspace_index(index)?);
        }
        Ok(self.registry.lock().resolve_target(target)?)
    }

    fn apply_observation(&self, window_session_id: &str, observation: WebObservation) {
        let snapshot = {
            let mut registry = self.registry.lock();
            match observation {
                WebObservation::PageStarted(url) => {
                    registry.record_page_started(window_session_id, url)
                }
                WebObservation::PageFinished(url) => {
                    registry.record_page_finished(window_session_id, url)
                }
                #[cfg(target_os = "linux")]
                WebObservation::PageFailed { url, error } => {
                    registry.record_page_failed(window_session_id, url, error)
                }
                WebObservation::Title(title) => registry.record_title(window_session_id, title),
            }
            registry.status(window_session_id).ok()
        };
        if let Some(snapshot) = snapshot {
            self.persistence.sync_async(snapshot);
        }
    }
}

fn platform_window_id(window: &WebviewWindow) -> Result<usize, WindowManagerError> {
    #[cfg(target_os = "linux")]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let handle = window
            .window_handle()
            .map_err(|error| WindowManagerError::Operation(error.to_string()))?
            .as_raw();
        match handle {
            RawWindowHandle::Xlib(handle) => Ok(handle.window as usize),
            RawWindowHandle::Xcb(handle) => Ok(handle.window.get() as usize),
            _ => Err(WindowManagerError::Workspace(
                WorkspaceError::DisplayUnavailable("X11 window handle is unavailable".to_string()),
            )),
        }
    }
    #[cfg(target_os = "windows")]
    {
        let hwnd = window
            .hwnd()
            .map_err(|error| WindowManagerError::Operation(error.to_string()))?
            .0;
        if hwnd.is_null() {
            return Err(WindowManagerError::Workspace(
                WorkspaceError::DisplayUnavailable("Windows HWND is unavailable".to_string()),
            ));
        }
        Ok(hwnd as usize)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = window;
        Err(WindowManagerError::Workspace(
            WorkspaceError::DisplayUnavailable("native window handle is unavailable".to_string()),
        ))
    }
}

fn attach_workspace_tracking(
    window: &WebviewWindow,
    manager: &Arc<WindowManager>,
    window_session_id: &str,
) -> Result<(), WindowManagerError> {
    let handle_id = platform_window_id(window)?;
    let service = Arc::clone(manager.workspace_service()?);
    let weak_manager = Arc::downgrade(manager);
    let tracked_id = window_session_id.to_string();
    let callback_id = tracked_id.clone();
    let callback = Arc::new(move |placement: WorkspacePlacement| {
        if let Some(manager) = weak_manager.upgrade() {
            if let Ok(updated) = manager.registry.lock().record_workspace(
                &callback_id,
                placement.index,
                placement.all,
            ) {
                manager.persistence.sync_async(updated);
            }
        }
    });
    service.watch_window(tracked_id, handle_id, callback)?;
    Ok(())
}

fn refresh_workspace_for_window(
    window: &WebviewWindow,
    manager: &WindowManager,
    window_session_id: &str,
) -> Result<(), WindowManagerError> {
    let handle_id = platform_window_id(window)?;
    let placement = manager.workspace_service()?.query_window(handle_id)?;
    let updated = manager.registry.lock().record_workspace(
        window_session_id,
        placement.index,
        placement.all,
    )?;
    manager.persistence.sync_async(updated);
    Ok(())
}

fn prune_web_cache(_window: &WebviewWindow) -> Result<(), String> {
    Ok(())
}

fn record_or_buffer(
    weak_manager: &Weak<WindowManager>,
    gate: &Arc<Mutex<ObservationGate>>,
    window_session_id: &str,
    observation: WebObservation,
) {
    let mut state = gate.lock();
    if !state.active {
        state.pending.push(observation);
        return;
    }
    drop(state);
    if let Some(manager) = weak_manager.upgrade() {
        manager.apply_observation(window_session_id, observation);
    }
}

fn activate_observations(
    manager: &WindowManager,
    window_session_id: &str,
    gate: &Arc<Mutex<ObservationGate>>,
) {
    let pending = {
        let mut state = gate.lock();
        state.active = true;
        std::mem::take(&mut state.pending)
    };
    for observation in pending {
        manager.apply_observation(window_session_id, observation);
    }
}

#[cfg(target_os = "linux")]
fn attach_platform_load_failure_observer(
    window: &WebviewWindow,
    manager: &Arc<WindowManager>,
    window_session_id: &str,
    gate: &Arc<Mutex<ObservationGate>>,
) -> Result<(), String> {
    let weak_manager = Arc::downgrade(manager);
    let gate = Arc::clone(gate);
    let window_session_id = window_session_id.to_string();
    crate::linux_webkit::attach_load_failure_observer(window, move |url, error| {
        record_or_buffer(
            &weak_manager,
            &gate,
            &window_session_id,
            WebObservation::PageFailed { url, error },
        );
    })
}

#[cfg(not(target_os = "linux"))]
fn attach_platform_load_failure_observer(
    _window: &WebviewWindow,
    _manager: &Arc<WindowManager>,
    _window_session_id: &str,
    _gate: &Arc<Mutex<ObservationGate>>,
) -> Result<(), String> {
    Ok(())
}

fn missing_handle(window_session_id: &str) -> WindowManagerError {
    WindowManagerError::Operation(format!(
        "Tauri window handle for '{window_session_id}' is unavailable"
    ))
}

mod auth;
mod commands;
mod lifecycle;
mod open;
