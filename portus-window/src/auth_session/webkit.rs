use super::{
    AuthAuthorityError, AuthBrokerError, AuthGrantTarget, AuthenticatedSessionAuthority,
    AuthenticatedSessionBroker, BrokeredCookie,
};
use crate::tauri_window::{WindowManager, WindowManagerError};
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::sync::Arc;
use tauri::WebviewWindow;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct AppliedSessionMetadata {
    pub domain: String,
    pub window_session_id: String,
    pub cookie_count: usize,
}

#[allow(dead_code)]
#[derive(Clone)]
struct AppliedCookie {
    name: String,
    domain: String,
    path: String,
}

struct AppliedSession {
    domain: String,
    window_session_id: String,
    cookies: Vec<AppliedCookie>,
}

#[derive(Debug, Error)]
pub enum AuthWebKitError {
    #[error("authenticated-session authority error: {0}")]
    Authority(#[from] AuthAuthorityError),
    #[error("authenticated-session broker error: {0}")]
    Broker(#[from] AuthBrokerError),
    #[error("window manager error: {0}")]
    WindowManager(#[from] WindowManagerError),
    #[error("cookie manager operation failed: {0}")]
    CookieOperation(String),
    #[error("authenticated cleanup is pending because the session web view is unavailable")]
    CleanupPending,
}

pub struct AuthenticatedSessionWebKit {
    windows: Arc<WindowManager>,
    authority: Arc<AuthenticatedSessionAuthority>,
    broker: Arc<AuthenticatedSessionBroker>,
    applied: Mutex<BTreeMap<AuthGrantTarget, AppliedSession>>,
}

impl AuthenticatedSessionWebKit {
    pub fn new(
        windows: Arc<WindowManager>,
        authority: Arc<AuthenticatedSessionAuthority>,
        broker: Arc<AuthenticatedSessionBroker>,
    ) -> Self {
        Self {
            windows,
            authority,
            broker,
            applied: Mutex::new(BTreeMap::new()),
        }
    }

    fn apply_authorized_cookies(
        &self,
        target: &AuthGrantTarget,
        window_session_id: &str,
    ) -> Result<AppliedSessionMetadata, AuthWebKitError> {
        let grant = self.authority.authorized_grant(target)?;
        if let AuthGrantTarget::Window(expected) = target {
            if expected != window_session_id {
                return Err(AuthAuthorityError::GrantNotFound.into());
            }
        }
        let window = self.windows.auth_cookie_window(window_session_id)?;
        let mut cookies = Vec::new();

        let apply_result = self.broker.with_cookies(target, |brokered_cookies| {
            for cookie in brokered_cookies {
                if let Err(error) = install_cookie(&window, cookie) {
                    let mut rollback_failed = Vec::new();
                    for installed in &cookies {
                        if let Err(rollback_error) = delete_cookie(&window, installed) {
                            rollback_failed.push(installed.clone());
                            eprintln!(
                                "Portus Window rollback delete_cookie failed: {rollback_error}"
                            );
                        }
                    }
                    if !rollback_failed.is_empty() {
                        self.applied.lock().insert(
                            target.clone(),
                            AppliedSession {
                                domain: grant.domain.clone(),
                                window_session_id: window_session_id.to_string(),
                                cookies: rollback_failed,
                            },
                        );
                        return Err(AuthWebKitError::CookieOperation(format!(
                            "cookie installation failed: {error}; some cookies remain pending rollback"
                        )));
                    }
                    return Err(AuthWebKitError::CookieOperation(format!(
                        "cookie installation failed: {error}"
                    )));
                }
                cookies.push(AppliedCookie {
                    name: cookie.name.clone(),
                    domain: cookie.domain.clone(),
                    path: cookie.path.clone(),
                });
            }
            Ok(())
        })?;

        apply_result?;

        let metadata = AppliedSessionMetadata {
            domain: grant.domain.clone(),
            window_session_id: window_session_id.to_string(),
            cookie_count: cookies.len(),
        };
        self.applied.lock().insert(
            target.clone(),
            AppliedSession {
                domain: grant.domain,
                window_session_id: window_session_id.to_string(),
                cookies,
            },
        );
        Ok(metadata)
    }

    pub fn apply(
        &self,
        target: &AuthGrantTarget,
        window_session_id: &str,
    ) -> Result<AppliedSessionMetadata, AuthWebKitError> {
        let _grant = self.authority.authorized_grant(target)?;
        let is_web_video = self.windows.is_web_video_window(window_session_id);
        if !self
            .windows
            .is_authenticated_profile_window(window_session_id)
        {
            self.windows
                .upgrade_web_window_to_authenticated_profile(window_session_id)?;
        }
        let metadata = self.apply_authorized_cookies(target, window_session_id)?;
        if is_web_video {
            if let Err(error) = self
                .windows
                .enable_authenticated_web_video(window_session_id)
            {
                if let Err(revoke_error) = self.revoke(target) {
                    eprintln!(
                        "Portus Window YouTube auth upgrade rollback cleanup failed: {revoke_error}"
                    );
                }
                return Err(AuthWebKitError::WindowManager(error));
            }
        } else if let Err(error) = self
            .windows
            .reload_authenticated_web_window(window_session_id)
        {
            if let Err(revoke_error) = self.revoke(target) {
                eprintln!(
                    "Portus Window auth upgrade reload rollback cleanup failed: {revoke_error}"
                );
            }
            return Err(AuthWebKitError::WindowManager(error));
        }

        if let Err(error) = self.authority.mark_applied(target, true) {
            let _ = self.revoke(target);
            return Err(error.into());
        }
        Ok(metadata)
    }

    pub fn revoke(&self, target: &AuthGrantTarget) -> Result<(), AuthWebKitError> {
        let mut applied = self.applied.lock();
        let Some(session) = applied.get_mut(target) else {
            return Ok(());
        };
        let window = {
            if let Some(window) = self
                .windows
                .auth_cookie_handle_exact(&session.window_session_id)
            {
                window
            } else if self
                .windows
                .purge_authenticated_profile_directory(&session.window_session_id)
                .is_ok()
            {
                applied.remove(target);
                let _ = self.authority.mark_applied(target, false);
                return Ok(());
            } else {
                return Err(AuthWebKitError::CleanupPending);
            }
        };

        let mut failed = Vec::new();
        for cookie in &session.cookies {
            if let Err(error) = delete_cookie(&window, cookie) {
                eprintln!("Portus Window delete_cookie failed during revoke: {error}");
                failed.push(cookie.clone());
            }
        }

        if !failed.is_empty() {
            session.cookies = failed;
            return Err(AuthWebKitError::CookieOperation(
                "one or more applied cookies could not be removed; cleanup remains pending"
                    .to_string(),
            ));
        }

        if self.windows.is_web_video_window(&session.window_session_id) {
            if let Err(error) = self
                .windows
                .disable_authenticated_web_video(&session.window_session_id)
            {
                eprintln!(
                    "Portus Window YouTube privacy restoration failed during auth revoke: {error}"
                );
                return Err(AuthWebKitError::WindowManager(error));
            }
        }

        applied.remove(target);
        let _ = self.authority.mark_applied(target, false);
        Ok(())
    }

    pub fn applied_target_for_window(&self, window_session_id: &str) -> Option<AuthGrantTarget> {
        self.applied
            .lock()
            .iter()
            .find(|(_, session)| session.window_session_id == window_session_id)
            .map(|(target, _)| target.clone())
    }

    pub fn metadata(&self, target: &AuthGrantTarget) -> Option<AppliedSessionMetadata> {
        self.applied
            .lock()
            .get(target)
            .map(|session| AppliedSessionMetadata {
                domain: session.domain.clone(),
                window_session_id: session.window_session_id.clone(),
                cookie_count: session.cookies.len(),
            })
    }
}

#[cfg(target_os = "linux")]
fn install_cookie(window: &WebviewWindow, cookie: &BrokeredCookie) -> Result<(), AuthWebKitError> {
    use soup::Cookie;
    use std::sync::mpsc;
    use std::time::Duration;
    use webkit2gtk::{CookieManagerExt, WebContextExt, WebViewExt};

    let name = cookie.name.clone();
    let value = cookie.value.clone();
    let domain = cookie.domain.clone();
    let path = cookie.path.clone();
    let expires = cookie.expires_unix.unwrap_or(0) as i32;
    let secure = cookie.secure;
    let http_only = cookie.http_only;

    let (sender, receiver) = mpsc::sync_channel(1);
    window
        .with_webview(move |platform_webview| {
            let mut cookie_obj = Cookie::new(&name, &value, &domain, &path, expires);
            cookie_obj.set_secure(secure);
            cookie_obj.set_http_only(http_only);
            let webview = platform_webview.inner();
            if let Some(context) = webview.context() {
                if let Some(manager) = context.cookie_manager() {
                    manager.add_cookie(&mut cookie_obj, None::<&gio::Cancellable>, move |res| {
                        let _ = sender.send(res.map_err(|e| e.to_string()));
                    });
                }
            }
        })
        .map_err(|e| AuthWebKitError::CookieOperation(e.to_string()))?;

    receiver
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| AuthWebKitError::CookieOperation("cookie addition timed out".to_string()))?
        .map_err(AuthWebKitError::CookieOperation)
}

#[cfg(not(target_os = "linux"))]
fn install_cookie(
    _window: &WebviewWindow,
    _cookie: &BrokeredCookie,
) -> Result<(), AuthWebKitError> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn delete_cookie(window: &WebviewWindow, cookie: &AppliedCookie) -> Result<(), AuthWebKitError> {
    use soup::Cookie;
    use std::sync::mpsc;
    use std::time::Duration;
    use webkit2gtk::{CookieManagerExt, WebContextExt, WebViewExt};

    let name = cookie.name.clone();
    let domain = cookie.domain.clone();
    let path = cookie.path.clone();

    let (sender, receiver) = mpsc::sync_channel(1);
    window
        .with_webview(move |platform_webview| {
            let mut cookie_obj = Cookie::new(&name, "", &domain, &path, 0);
            let webview = platform_webview.inner();
            if let Some(context) = webview.context() {
                if let Some(manager) = context.cookie_manager() {
                    manager.delete_cookie(&mut cookie_obj, None::<&gio::Cancellable>, move |res| {
                        let _ = sender.send(res.map_err(|e| e.to_string()));
                    });
                }
            }
        })
        .map_err(|e| AuthWebKitError::CookieOperation(e.to_string()))?;

    receiver
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| AuthWebKitError::CookieOperation("cookie deletion timed out".to_string()))?
        .map_err(AuthWebKitError::CookieOperation)
}

#[cfg(not(target_os = "linux"))]
fn delete_cookie(_window: &WebviewWindow, _cookie: &AppliedCookie) -> Result<(), AuthWebKitError> {
    Ok(())
}
