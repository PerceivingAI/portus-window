use super::{AuthGrantTarget, AuthenticatedSessionAuthority, PendingAuthGrant};
use parking_lot::Mutex;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use tauri::{AppHandle, Manager};
use thiserror::Error;
use url::Url;
use uuid::Uuid;

const CONSENT_OVERLAY_ID: &str = "__portus_auth_consent_overlay";

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AuthConsentView {
    pub domain: String,
    pub window_session_id: Option<String>,
    pub browser: String,
    pub scope: String,
    pub reason: Option<String>,
    pub provenance: &'static str,
}

#[derive(Clone)]
struct ConsentRecord {
    target: AuthGrantTarget,
    grant: PendingAuthGrant,
}

#[derive(Debug, Error)]
pub enum AuthConsentError {
    #[error("authenticated-session consent request is not pending")]
    NotPending,
    #[error("authenticated-session consent window could not be created: {0}")]
    Window(String),
    #[error("authenticated-session consent view could not be serialized: {0}")]
    Serialization(String),
}

pub struct AuthConsentController {
    app: AppHandle,
    authority: Arc<AuthenticatedSessionAuthority>,
    by_token: Mutex<BTreeMap<String, ConsentRecord>>,
    by_target: Mutex<BTreeMap<AuthGrantTarget, String>>,
}

impl AuthConsentController {
    pub fn new(app: AppHandle, authority: Arc<AuthenticatedSessionAuthority>) -> Self {
        Self {
            app,
            authority,
            by_token: Mutex::new(BTreeMap::new()),
            by_target: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn present(self: &Arc<Self>, target: &AuthGrantTarget) -> Result<(), AuthConsentError> {
        let grant = self
            .authority
            .pending_grant(target)
            .ok_or(AuthConsentError::NotPending)?;
        let existing_token = { self.by_target.lock().get(target).cloned() };
        if let Some(token) = existing_token {
            let existing = self.by_token.lock().get(&token).cloned();
            if existing
                .as_ref()
                .is_some_and(|record| record.grant == grant)
            {
                return Ok(());
            }
            self.remove_record(&token);
        }

        let target_window_session_id = match grant.window_session_id.as_deref() {
            Some(window_session_id) => window_session_id,
            None => {
                return Err(AuthConsentError::Window(
                    "consent request has no presentation window".to_string(),
                ));
            }
        };
        let parent = self
            .app
            .get_webview_window(target_window_session_id)
            .ok_or_else(|| {
                AuthConsentError::Window("target window is no longer open".to_string())
            })?;

        let token = Uuid::new_v4().simple().to_string();
        let view = AuthConsentView::from(&grant);
        let payload = serde_json::to_string(&view)
            .map_err(|error| AuthConsentError::Serialization(error.to_string()))?;

        let record = ConsentRecord {
            target: target.clone(),
            grant,
        };
        self.by_token.lock().insert(token.clone(), record);
        self.by_target.lock().insert(target.clone(), token.clone());

        let script = consent_overlay_script(&token, &payload);
        parent.eval(script).map_err(|error| {
            self.remove_record(&token);
            AuthConsentError::Window(error.to_string())
        })?;
        parent.set_focus().map_err(|error| {
            self.remove_record(&token);
            let _ = remove_consent_overlay(&parent);
            AuthConsentError::Window(error.to_string())
        })?;
        Ok(())
    }

    pub fn cancel_window(&self, window_session_id: &str) {
        let tokens = self
            .by_token
            .lock()
            .iter()
            .filter(|(_, record)| {
                record.grant.window_session_id.as_deref() == Some(window_session_id)
            })
            .map(|(token, _)| token.clone())
            .collect::<Vec<_>>();
        for token in tokens {
            self.cancel_if_pending(&token);
            if let Some(target_window_id) = target_window_id_for_token(self, &token) {
                if let Some(window) = self.app.get_webview_window(&target_window_id) {
                    let _ = remove_consent_overlay(&window);
                }
            }
        }
    }

    pub fn decide_for_window(&self, token: &str, window_session_id: &str, approve: bool) -> bool {
        let Some(record) = self.by_token.lock().get(token).cloned() else {
            return false;
        };
        let Some(target_window_id) = record.grant.window_session_id.as_deref() else {
            return false;
        };
        if target_window_id != window_session_id {
            return false;
        }
        self.remove_record(token);
        let result = if approve {
            self.authority
                .authorize_pending_exact(&record.target, &record.grant)
        } else {
            self.authority
                .deny_pending_exact(&record.target, &record.grant)
        };
        if let Err(error) = result {
            eprintln!(
                "Portus Window authenticated-session consent decision was stale or failed: {error}"
            );
        }
        if let Some(window) = self.app.get_webview_window(window_session_id) {
            let _ = remove_consent_overlay(&window);
        }
        true
    }

    fn cancel_if_pending(&self, token: &str) {
        let Some(record) = self.remove_record(token) else {
            return;
        };
        if let Err(error) = self
            .authority
            .deny_pending_exact(&record.target, &record.grant)
        {
            eprintln!(
                "Portus Window authenticated-session consent cancellation was stale or failed: {error}"
            );
        }
    }

    fn remove_record(&self, token: &str) -> Option<ConsentRecord> {
        let record = self.by_token.lock().remove(token)?;
        let mut by_target = self.by_target.lock();
        if by_target
            .get(&record.target)
            .is_some_and(|current| current == token)
        {
            by_target.remove(&record.target);
        }
        Some(record)
    }
}

impl From<&PendingAuthGrant> for AuthConsentView {
    fn from(grant: &PendingAuthGrant) -> Self {
        Self {
            domain: grant.domain.clone(),
            window_session_id: grant.window_session_id.clone(),
            browser: format!("{:?}", grant.browser).to_ascii_lowercase(),
            scope: format!("{:?}", grant.scope).to_ascii_lowercase(),
            reason: grant.reason.clone(),
            provenance: "local_agent_ipc",
        }
    }
}

pub(crate) fn consent_navigation_decision(url: &Url) -> Option<(String, bool)> {
    if url.scheme() != "portus-consent" {
        return None;
    }
    let approve = match url.host_str() {
        Some("approve") => true,
        Some("deny") => false,
        _ => return None,
    };
    let token = url
        .query_pairs()
        .find(|(key, _)| key == "token")
        .map(|(_, value)| value.into_owned())?;
    if token.is_empty() {
        return None;
    }
    Some((token, approve))
}

fn consent_overlay_script(token: &str, payload: &str) -> String {
    let token = serde_json::to_string(token).expect("consent token is serializable");
    format!(
        r#"(() => {{
const ID={CONSENT_OVERLAY_ID:?};
const token={token};
const data={payload};
const old=document.getElementById(ID); if(old) old.remove();
const style=document.createElement('style');
style.id=ID+'-style';
style.textContent=`#${{ID}}{{position:fixed;inset:0;z-index:2147483647;display:flex;align-items:center;justify-content:center;background:rgba(0,0,0,.58);font-family:system-ui,-apple-system,sans-serif;color:#f5f7fa;box-sizing:border-box}}#${{ID}} *{{box-sizing:border-box}}#${{ID}} .modal{{position:relative;width:min(520px,calc(100vw - 40px));padding:30px;border-radius:16px;background:#0d1117;box-shadow:0 20px 60px rgba(0,0,0,.45);border:1px solid #303842}}#${{ID}} .eyebrow{{margin:0 0 12px;font-size:12px;letter-spacing:.16em;color:#aeb8c4}}#${{ID}} h1{{margin:0;max-width:430px;font-size:26px;line-height:1.2}}#${{ID}} .actions{{display:flex;justify-content:flex-end;gap:10px;margin-top:28px}}#${{ID}} button{{font:inherit;cursor:pointer;padding:10px 15px;border-radius:8px;font-weight:650}}#${{ID}} .deny{{color:#eef2f7;background:transparent;border:1px solid #47515d}}#${{ID}} .allow{{background:#eef2f7;color:#111418;border:1px solid #eef2f7}}#${{ID}} button:focus-visible{{outline:2px solid #eef2f7;outline-offset:2px}}`;
const overlay=document.createElement('div'); overlay.id=ID; overlay.setAttribute('role','dialog'); overlay.setAttribute('aria-modal','true');
const modal=document.createElement('main'); modal.className='modal';
const eyebrow=document.createElement('p'); eyebrow.className='eyebrow'; eyebrow.textContent='PORTUS WINDOW';
const title=document.createElement('h1'); title.textContent='Do you want to use your existing browser session?'; title.id=ID+'-title';
const actions=document.createElement('div'); actions.className='actions';
const deny=document.createElement('button'); deny.className='deny'; deny.textContent='Deny';
const allow=document.createElement('button'); allow.className='allow'; allow.textContent='Allow';
const decide=(value)=>{{ window.location.href='portus-consent://'+(value?'approve':'deny')+'?token='+encodeURIComponent(token); }};
deny.onclick=()=>decide(false); allow.onclick=()=>decide(true); document.addEventListener('keydown',(event)=>{{if(event.key==='Escape') decide(false);}}); overlay.onclick=(event)=>{{if(event.target===overlay) decide(false);}};
modal.append(eyebrow,title,actions); actions.append(deny,allow); overlay.append(modal); document.head.append(style); document.body.append(overlay); allow.focus();
}})();"#
    )
}

fn remove_consent_overlay<R: tauri::Runtime>(
    window: &tauri::WebviewWindow<R>,
) -> tauri::Result<()> {
    let id = serde_json::to_string(CONSENT_OVERLAY_ID).expect("overlay id is serializable");
    window.eval(format!(
        "document.getElementById({id})?.remove();document.getElementById({id}+'-style')?.remove();"
    ))
}

fn target_window_id_for_token(controller: &AuthConsentController, token: &str) -> Option<String> {
    match controller.by_token.lock().get(token)?.target.clone() {
        AuthGrantTarget::Window(window_id) => Some(window_id),
        AuthGrantTarget::Domain(_) => controller
            .by_token
            .lock()
            .get(token)?
            .grant
            .window_session_id
            .clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth_session::AuthSiteBundle;
    use portus_window_protocol::{AuthSessionScope, BrowserKind};

    #[test]
    fn consent_navigation_accepts_only_explicit_local_decisions() {
        assert_eq!(
            consent_navigation_decision(&Url::parse("portus-consent://approve?token=abc").unwrap()),
            Some(("abc".to_string(), true))
        );
        assert_eq!(
            consent_navigation_decision(&Url::parse("portus-consent://deny?token=abc").unwrap()),
            Some(("abc".to_string(), false))
        );
        assert_eq!(
            consent_navigation_decision(&Url::parse("https://example.com").unwrap()),
            None
        );
        assert_eq!(
            consent_navigation_decision(&Url::parse("portus-consent://other").unwrap()),
            None
        );
    }

    #[test]
    fn consent_view_contains_provenance_but_no_cookie_material() {
        let grant = PendingAuthGrant {
            target: AuthGrantTarget::Window("wsess_0123456789abcdef0123456789abcdef".to_string()),
            domain: "example.com".to_string(),
            window_session_id: Some("wsess_0123456789abcdef0123456789abcdef".to_string()),
            browser: BrowserKind::Firefox,
            scope: AuthSessionScope::Session,
            reason: Some("Open the signed-in page".to_string()),
            cookie_domains: vec!["example.com".to_string()],
            bundle: AuthSiteBundle::ExactDomain,
        };
        let view = AuthConsentView::from(&grant);
        let json = serde_json::to_string(&view).unwrap();
        assert_eq!(view.provenance, "local_agent_ipc");
        assert!(json.contains("example.com"));
        for forbidden in ["cookie", "token", "authorization", "secret-value"] {
            assert!(!json.contains(forbidden));
        }
    }
}
