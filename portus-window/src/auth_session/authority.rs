use super::{AuthDomainError, AuthSiteBundle, ValidatedAuthRequest, ValidatedAuthTarget};
use parking_lot::Mutex;
use portus_window_protocol::{
    AuthSessionAction, AuthSessionDecision, AuthSessionResult, AuthSessionScope, AuthSessionState,
    AuthSessionTarget, BrowserKind,
};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AuthGrantTarget {
    Domain(String),
    Window(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingAuthGrant {
    pub target: AuthGrantTarget,
    pub domain: String,
    pub window_session_id: Option<String>,
    pub browser: BrowserKind,
    pub scope: AuthSessionScope,
    pub reason: Option<String>,
    pub cookie_domains: Vec<String>,
    pub bundle: AuthSiteBundle,
}

#[derive(Clone, Debug)]
struct GrantRecord {
    grant: PendingAuthGrant,
    decision: AuthSessionDecision,
    available: bool,
    applied: bool,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AuthAuthorityError {
    #[error(transparent)]
    Validation(#[from] AuthDomainError),
    #[error("remembered authenticated sessions are reserved but unsupported until secure persistence is implemented")]
    RememberedUnsupported,
    #[error("authenticated-session window target does not resolve to an HTTP(S) web domain")]
    WindowDomainUnavailable,
    #[error("authenticated-session consent target was not requested")]
    GrantNotFound,
    #[error("authenticated-session state transition requires an authorized grant")]
    NotAuthorized,
    #[error("authenticated-session grant is actively applied and must be cleaned up before it can be replaced")]
    GrantInUse,
}

#[derive(Default)]
pub struct AuthenticatedSessionAuthority {
    grants: Mutex<BTreeMap<AuthGrantTarget, GrantRecord>>,
}

impl AuthenticatedSessionAuthority {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn request(
        &self,
        validated: ValidatedAuthRequest,
        resolved_window_domain: Option<ValidatedAuthTarget>,
    ) -> Result<AuthSessionResult, AuthAuthorityError> {
        let ValidatedAuthRequest::Request {
            target,
            window_session_id,
            browser,
            scope,
            reason,
            presentation_window_session_id,
        } = validated
        else {
            return Err(AuthAuthorityError::GrantNotFound);
        };
        if scope == AuthSessionScope::Remembered {
            return Err(AuthAuthorityError::RememberedUnsupported);
        }

        let (key, target, window_session_id) = match (target, window_session_id) {
            (Some(target), None) => {
                let presentation_window_session_id =
                    presentation_window_session_id.ok_or(AuthAuthorityError::GrantNotFound)?;
                (
                    AuthGrantTarget::Domain(target.requested_domain().to_string()),
                    target,
                    Some(presentation_window_session_id),
                )
            }
            (None, Some(window_session_id)) => {
                if presentation_window_session_id.is_some() {
                    return Err(AuthAuthorityError::GrantNotFound);
                }
                let target =
                    resolved_window_domain.ok_or(AuthAuthorityError::WindowDomainUnavailable)?;
                (
                    AuthGrantTarget::Window(window_session_id.clone()),
                    target,
                    Some(window_session_id),
                )
            }
            _ => return Err(AuthAuthorityError::GrantNotFound),
        };

        let pending = PendingAuthGrant {
            target: key.clone(),
            domain: target.requested_domain().to_string(),
            window_session_id,
            browser,
            scope,
            reason,
            cookie_domains: target.cookie_domains().to_vec(),
            bundle: target.bundle(),
        };

        let mut grants = self.grants.lock();
        if let Some(existing) = grants.get(&key) {
            if existing.decision == AuthSessionDecision::Authorized
                && existing.grant.browser == browser
                && existing.grant.scope == scope
                && existing.grant.domain == pending.domain
            {
                return Ok(result(existing));
            }
            if existing.applied {
                return Err(AuthAuthorityError::GrantInUse);
            }
        }

        let record = GrantRecord {
            grant: pending,
            decision: AuthSessionDecision::Pending,
            available: false,
            applied: false,
        };
        let response = result(&record);
        grants.insert(key, record);
        Ok(response)
    }

    pub fn status(
        &self,
        validated: ValidatedAuthRequest,
        resolved_window_domain: Option<ValidatedAuthTarget>,
    ) -> Result<AuthSessionResult, AuthAuthorityError> {
        let (key, fallback_domain, _fallback_window) = match validated {
            ValidatedAuthRequest::Status {
                target: Some(target),
                window_session_id: None,
            } => (
                AuthGrantTarget::Domain(target.requested_domain().to_string()),
                target.requested_domain().to_string(),
                None,
            ),
            ValidatedAuthRequest::Status {
                target: None,
                window_session_id: Some(window_session_id),
            } => {
                let domain = resolved_window_domain
                    .as_ref()
                    .map(|target| target.requested_domain().to_string())
                    .unwrap_or_default();
                (
                    AuthGrantTarget::Window(window_session_id.clone()),
                    domain,
                    Some(window_session_id),
                )
            }
            _ => return Err(AuthAuthorityError::GrantNotFound),
        };

        let grants = self.grants.lock();
        if let Some(record) = grants.get(&key) {
            return Ok(result(record));
        }
        let (target, canonical_domain) = match &key {
            AuthGrantTarget::Domain(dom) => (
                AuthSessionTarget::Domain {
                    domain: dom.clone(),
                },
                dom.clone(),
            ),
            AuthGrantTarget::Window(win) => (
                AuthSessionTarget::Window {
                    window_session_id: win.clone(),
                },
                fallback_domain,
            ),
        };
        Ok(AuthSessionResult {
            state: AuthSessionState {
                target,
                canonical_domain,
                browser: BrowserKind::Firefox,
                scope: AuthSessionScope::Session,
                decision: AuthSessionDecision::Denied,
                available: false,
                applied: false,
                reason: None,
            },
        })
    }

    pub fn revoke(
        &self,
        validated: ValidatedAuthRequest,
        resolved_window_domain: Option<ValidatedAuthTarget>,
    ) -> Result<AuthSessionResult, AuthAuthorityError> {
        let status_form = match validated {
            ValidatedAuthRequest::Revoke {
                target,
                window_session_id,
            } => ValidatedAuthRequest::Status {
                target,
                window_session_id,
            },
            _ => return Err(AuthAuthorityError::GrantNotFound),
        };
        let current = self.status(status_form, resolved_window_domain)?;
        let key = match &current.state.target {
            AuthSessionTarget::Window { window_session_id } => {
                AuthGrantTarget::Window(window_session_id.clone())
            }
            AuthSessionTarget::Domain { domain } => AuthGrantTarget::Domain(domain.clone()),
        };

        let mut grants = self.grants.lock();
        let Some(record) = grants.get_mut(&key) else {
            return Ok(current);
        };
        record.decision = AuthSessionDecision::Revoked;
        record.available = false;
        record.applied = false;
        Ok(result(record))
    }

    // Trusted UI/host integration calls these methods directly. They are intentionally not
    // represented as protocol actions, so an agent or ordinary CLI caller cannot forge consent.
    pub fn authorize(
        &self,
        target: &AuthGrantTarget,
    ) -> Result<AuthSessionResult, AuthAuthorityError> {
        self.set_pending_decision(target, None, AuthSessionDecision::Authorized)
    }

    pub fn deny(&self, target: &AuthGrantTarget) -> Result<AuthSessionResult, AuthAuthorityError> {
        self.set_pending_decision(target, None, AuthSessionDecision::Denied)
    }

    pub fn authorize_pending_exact(
        &self,
        target: &AuthGrantTarget,
        expected: &PendingAuthGrant,
    ) -> Result<AuthSessionResult, AuthAuthorityError> {
        self.set_pending_decision(target, Some(expected), AuthSessionDecision::Authorized)
    }

    pub fn deny_pending_exact(
        &self,
        target: &AuthGrantTarget,
        expected: &PendingAuthGrant,
    ) -> Result<AuthSessionResult, AuthAuthorityError> {
        self.set_pending_decision(target, Some(expected), AuthSessionDecision::Denied)
    }

    fn set_pending_decision(
        &self,
        target: &AuthGrantTarget,
        expected: Option<&PendingAuthGrant>,
        decision: AuthSessionDecision,
    ) -> Result<AuthSessionResult, AuthAuthorityError> {
        let mut grants = self.grants.lock();
        let record = grants
            .get_mut(target)
            .ok_or(AuthAuthorityError::GrantNotFound)?;
        if record.decision != AuthSessionDecision::Pending
            || expected.is_some_and(|expected| expected != &record.grant)
        {
            return Err(AuthAuthorityError::GrantNotFound);
        }
        record.decision = decision;
        if decision != AuthSessionDecision::Authorized {
            record.available = false;
            record.applied = false;
        }
        Ok(result(record))
    }

    pub fn mark_available(
        &self,
        target: &AuthGrantTarget,
        available: bool,
    ) -> Result<AuthSessionResult, AuthAuthorityError> {
        let mut grants = self.grants.lock();
        let record = grants
            .get_mut(target)
            .ok_or(AuthAuthorityError::GrantNotFound)?;
        if record.decision != AuthSessionDecision::Authorized {
            return Err(AuthAuthorityError::NotAuthorized);
        }
        record.available = available;
        if !available {
            record.applied = false;
        }
        Ok(result(record))
    }

    pub fn mark_applied(
        &self,
        target: &AuthGrantTarget,
        applied: bool,
    ) -> Result<AuthSessionResult, AuthAuthorityError> {
        let mut grants = self.grants.lock();
        let record = grants
            .get_mut(target)
            .ok_or(AuthAuthorityError::GrantNotFound)?;
        if record.decision != AuthSessionDecision::Authorized {
            return Err(AuthAuthorityError::NotAuthorized);
        }
        if applied && !record.available {
            return Err(AuthAuthorityError::NotAuthorized);
        }
        record.applied = applied;
        Ok(result(record))
    }

    pub fn clear_once_after_use(
        &self,
        target: &AuthGrantTarget,
    ) -> Result<AuthSessionResult, AuthAuthorityError> {
        let mut grants = self.grants.lock();
        let record = grants
            .get_mut(target)
            .ok_or(AuthAuthorityError::GrantNotFound)?;
        if record.grant.scope == AuthSessionScope::Once {
            record.decision = AuthSessionDecision::Revoked;
            record.available = false;
            record.applied = false;
        }
        Ok(result(record))
    }

    pub fn revoke_window(&self, window_session_id: &str) {
        let key = AuthGrantTarget::Window(window_session_id.to_string());
        if let Some(record) = self.grants.lock().get_mut(&key) {
            record.decision = AuthSessionDecision::Revoked;
            record.available = false;
            record.applied = false;
        }
    }

    pub fn pending_grant(&self, target: &AuthGrantTarget) -> Option<PendingAuthGrant> {
        self.grants
            .lock()
            .get(target)
            .filter(|record| record.decision == AuthSessionDecision::Pending)
            .map(|record| record.grant.clone())
    }

    pub fn grant(&self, target: &AuthGrantTarget) -> Option<PendingAuthGrant> {
        self.grants
            .lock()
            .get(target)
            .map(|record| record.grant.clone())
    }

    pub fn authorized_grant(
        &self,
        target: &AuthGrantTarget,
    ) -> Result<PendingAuthGrant, AuthAuthorityError> {
        self.grants
            .lock()
            .get(target)
            .filter(|record| record.decision == AuthSessionDecision::Authorized)
            .map(|record| record.grant.clone())
            .ok_or(AuthAuthorityError::NotAuthorized)
    }
}

fn result(record: &GrantRecord) -> AuthSessionResult {
    let target = match &record.grant.target {
        AuthGrantTarget::Window(window_session_id) => AuthSessionTarget::Window {
            window_session_id: window_session_id.clone(),
        },
        AuthGrantTarget::Domain(domain) => AuthSessionTarget::Domain {
            domain: domain.clone(),
        },
    };
    AuthSessionResult {
        state: AuthSessionState {
            target,
            canonical_domain: record.grant.domain.clone(),
            browser: record.grant.browser,
            scope: record.grant.scope,
            decision: record.decision,
            available: record.available,
            applied: record.applied,
            reason: record.grant.reason.clone(),
        },
    }
}

pub fn dispatch_action(
    authority: &AuthenticatedSessionAuthority,
    action: &AuthSessionAction,
    resolved_window_domain: Option<ValidatedAuthTarget>,
) -> Result<AuthSessionResult, AuthAuthorityError> {
    let validated = super::validate_action(action)?;
    match validated {
        request @ ValidatedAuthRequest::Request { .. } => {
            authority.request(request, resolved_window_domain)
        }
        status @ ValidatedAuthRequest::Status { .. } => {
            authority.status(status, resolved_window_domain)
        }
        ValidatedAuthRequest::Apply { .. } => Err(AuthAuthorityError::GrantNotFound),
        revoke @ ValidatedAuthRequest::Revoke { .. } => {
            authority.revoke(revoke, resolved_window_domain)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use portus_window_protocol::AuthSessionTarget;

    fn domain_target() -> AuthSessionTarget {
        AuthSessionTarget::Domain {
            domain: "Example.COM.".to_string(),
        }
    }

    fn request(scope: AuthSessionScope) -> AuthSessionAction {
        AuthSessionAction::Request {
            target: domain_target(),
            browser: BrowserKind::Chromium,
            scope,
            reason: Some("login required".to_string()),
            presentation_window_session_id: Some(
                "wsess_00000000000000000000000000000000".to_string(),
            ),
        }
    }

    #[test]
    fn request_is_pending_until_trusted_authorization() {
        let authority = AuthenticatedSessionAuthority::new();
        let result = dispatch_action(&authority, &request(AuthSessionScope::Once), None).unwrap();
        assert_eq!(result.state.canonical_domain, "example.com");
        assert_eq!(result.state.decision, AuthSessionDecision::Pending);
        assert!(!result.state.available);
        assert!(!result.state.applied);

        let key = AuthGrantTarget::Domain("example.com".to_string());
        assert!(authority.pending_grant(&key).is_some());
        let authorized = authority.authorize(&key).unwrap();
        assert_eq!(authorized.state.decision, AuthSessionDecision::Authorized);
    }

    #[test]
    fn availability_and_application_require_authorization() {
        let authority = AuthenticatedSessionAuthority::new();
        dispatch_action(&authority, &request(AuthSessionScope::Session), None).unwrap();
        let key = AuthGrantTarget::Domain("example.com".to_string());
        assert_eq!(
            authority.mark_available(&key, true),
            Err(AuthAuthorityError::NotAuthorized)
        );
        authority.authorize(&key).unwrap();
        let available = authority.mark_available(&key, true).unwrap();
        assert!(available.state.available);
        let applied = authority.mark_applied(&key, true).unwrap();
        assert!(applied.state.applied);
    }

    #[test]
    fn once_grant_is_revoked_after_use_but_session_grant_survives() {
        let once = AuthenticatedSessionAuthority::new();
        dispatch_action(&once, &request(AuthSessionScope::Once), None).unwrap();
        let key = AuthGrantTarget::Domain("example.com".to_string());
        once.authorize(&key).unwrap();
        once.mark_available(&key, true).unwrap();
        once.mark_applied(&key, true).unwrap();
        let cleared = once.clear_once_after_use(&key).unwrap();
        assert_eq!(cleared.state.decision, AuthSessionDecision::Revoked);
        assert!(!cleared.state.available);
        assert!(!cleared.state.applied);

        let session = AuthenticatedSessionAuthority::new();
        dispatch_action(&session, &request(AuthSessionScope::Session), None).unwrap();
        session.authorize(&key).unwrap();
        let retained = session.clear_once_after_use(&key).unwrap();
        assert_eq!(retained.state.decision, AuthSessionDecision::Authorized);
    }

    #[test]
    fn denial_and_revoke_clear_capability_state() {
        let authority = AuthenticatedSessionAuthority::new();
        dispatch_action(&authority, &request(AuthSessionScope::Session), None).unwrap();
        let key = AuthGrantTarget::Domain("example.com".to_string());
        let denied = authority.deny(&key).unwrap();
        assert_eq!(denied.state.decision, AuthSessionDecision::Denied);

        dispatch_action(&authority, &request(AuthSessionScope::Session), None).unwrap();
        authority.authorize(&key).unwrap();
        authority.mark_available(&key, true).unwrap();
        let revoked = dispatch_action(
            &authority,
            &AuthSessionAction::Revoke {
                target: domain_target(),
            },
            None,
        )
        .unwrap();
        assert_eq!(revoked.state.decision, AuthSessionDecision::Revoked);
        assert!(!revoked.state.available);
    }

    #[test]
    fn remembered_scope_stays_runtime_unsupported() {
        let authority = AuthenticatedSessionAuthority::new();
        assert_eq!(
            dispatch_action(&authority, &request(AuthSessionScope::Remembered), None),
            Err(AuthAuthorityError::RememberedUnsupported)
        );
    }

    #[test]
    fn repeated_matching_authorized_request_is_idempotent() {
        let authority = AuthenticatedSessionAuthority::new();
        dispatch_action(&authority, &request(AuthSessionScope::Session), None).unwrap();
        let key = AuthGrantTarget::Domain("example.com".to_string());
        authority.authorize(&key).unwrap();
        let repeated =
            dispatch_action(&authority, &request(AuthSessionScope::Session), None).unwrap();
        assert_eq!(repeated.state.decision, AuthSessionDecision::Authorized);
    }
}
