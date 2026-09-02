use portus_window_protocol::{AuthSessionAction, AuthSessionScope, AuthSessionTarget, BrowserKind};

pub const MAX_AUTH_SESSION_DOMAIN_CHARS: usize = 253;
pub const MAX_AUTH_SESSION_REASON_CHARS: usize = 256;
use thiserror::Error;
use url::Host;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthSiteBundle {
    ExactDomain,
    YouTube,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedAuthTarget {
    requested_domain: String,
    cookie_domains: Vec<String>,
    bundle: AuthSiteBundle,
}

impl ValidatedAuthTarget {
    pub fn requested_domain(&self) -> &str {
        &self.requested_domain
    }

    pub fn cookie_domains(&self) -> &[String] {
        &self.cookie_domains
    }

    pub fn bundle(&self) -> AuthSiteBundle {
        self.bundle
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValidatedAuthRequest {
    Request {
        target: Option<ValidatedAuthTarget>,
        window_session_id: Option<String>,
        browser: BrowserKind,
        scope: AuthSessionScope,
        reason: Option<String>,
        presentation_window_session_id: Option<String>,
    },
    Status {
        target: Option<ValidatedAuthTarget>,
        window_session_id: Option<String>,
    },
    Apply {
        target: Option<ValidatedAuthTarget>,
        target_window_session_id: Option<String>,
        apply_window_session_id: String,
    },
    Revoke {
        target: Option<ValidatedAuthTarget>,
        window_session_id: Option<String>,
    },
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AuthDomainError {
    #[error("authenticated-session domain must not be blank")]
    BlankDomain,
    #[error("authenticated-session domain must be a host only, without scheme, path, port, credentials, wildcard, or leading dot")]
    InvalidDomainShape,
    #[error("authenticated-session domain exceeds {MAX_AUTH_SESSION_DOMAIN_CHARS} characters after canonicalization")]
    DomainTooLong,
    #[error("authenticated-session domain must be a DNS hostname, not an IP address or single-label local host")]
    NonDnsHost,
    #[error("authenticated-session reason must not be blank when supplied")]
    BlankReason,
    #[error("authenticated-session reason exceeds {MAX_AUTH_SESSION_REASON_CHARS} characters")]
    ReasonTooLong,
    #[error("authenticated-session window target must use the exact wsess_<32-lowercase-hex> identity format")]
    InvalidWindowSessionId,
}

pub fn validate_action(
    action: &AuthSessionAction,
) -> Result<ValidatedAuthRequest, AuthDomainError> {
    match action {
        AuthSessionAction::Request {
            target,
            browser,
            scope,
            reason,
            presentation_window_session_id,
        } => {
            let (target, window_session_id) = validate_target(target)?;
            let reason = validate_reason(reason.as_deref())?;
            let presentation_window_session_id = presentation_window_session_id
                .as_deref()
                .map(|id| {
                    validate_window_session_id(id)?;
                    Ok::<String, AuthDomainError>(id.to_string())
                })
                .transpose()?;
            if window_session_id.is_some() && presentation_window_session_id.is_some() {
                return Err(AuthDomainError::InvalidWindowSessionId);
            }
            if target.is_some() && presentation_window_session_id.is_none() {
                return Err(AuthDomainError::InvalidWindowSessionId);
            }
            Ok(ValidatedAuthRequest::Request {
                target,
                window_session_id,
                browser: *browser,
                scope: *scope,
                reason,
                presentation_window_session_id,
            })
        }
        AuthSessionAction::Status { target } => {
            let (target, window_session_id) = validate_target(target)?;
            Ok(ValidatedAuthRequest::Status {
                target,
                window_session_id,
            })
        }
        AuthSessionAction::Apply {
            target,
            window_session_id,
        } => {
            let (target, target_window_session_id) = validate_target(target)?;
            validate_window_session_id(window_session_id)?;
            if let Some(expected) = target_window_session_id.as_ref() {
                if expected != window_session_id {
                    return Err(AuthDomainError::InvalidWindowSessionId);
                }
            }
            Ok(ValidatedAuthRequest::Apply {
                target,
                target_window_session_id,
                apply_window_session_id: window_session_id.clone(),
            })
        }
        AuthSessionAction::Revoke { target } => {
            let (target, window_session_id) = validate_target(target)?;
            Ok(ValidatedAuthRequest::Revoke {
                target,
                window_session_id,
            })
        }
    }
}

fn validate_target(
    target: &AuthSessionTarget,
) -> Result<(Option<ValidatedAuthTarget>, Option<String>), AuthDomainError> {
    match target {
        AuthSessionTarget::Domain { domain } => Ok((Some(validate_domain(domain)?), None)),
        AuthSessionTarget::Window { window_session_id } => {
            validate_window_session_id(window_session_id)?;
            Ok((None, Some(window_session_id.clone())))
        }
    }
}

pub fn validate_domain(input: &str) -> Result<ValidatedAuthTarget, AuthDomainError> {
    if input.is_empty() || input.chars().all(char::is_whitespace) {
        return Err(AuthDomainError::BlankDomain);
    }
    if input != input.trim()
        || input.contains("//")
        || input.contains('/')
        || input.contains('\\')
        || input.contains('@')
        || input.contains(':')
        || input.contains('*')
        || input.starts_with('.')
        || input.contains('?')
        || input.contains('#')
    {
        return Err(AuthDomainError::InvalidDomainShape);
    }

    let without_final_dot = input.strip_suffix('.').unwrap_or(input);
    if without_final_dot.is_empty() || without_final_dot.ends_with('.') {
        return Err(AuthDomainError::InvalidDomainShape);
    }

    let canonical =
        match Host::parse(without_final_dot).map_err(|_| AuthDomainError::InvalidDomainShape)? {
            Host::Domain(domain) => domain.to_ascii_lowercase(),
            Host::Ipv4(_) | Host::Ipv6(_) => return Err(AuthDomainError::NonDnsHost),
        };

    if canonical.len() > MAX_AUTH_SESSION_DOMAIN_CHARS {
        return Err(AuthDomainError::DomainTooLong);
    }
    if !canonical.contains('.') {
        return Err(AuthDomainError::NonDnsHost);
    }

    let (bundle, requested_domain, cookie_domains) =
        youtube_bundle(&canonical).unwrap_or_else(|| {
            (
                AuthSiteBundle::ExactDomain,
                canonical.clone(),
                vec![canonical],
            )
        });

    Ok(ValidatedAuthTarget {
        requested_domain,
        cookie_domains,
        bundle,
    })
}

fn youtube_bundle(domain: &str) -> Option<(AuthSiteBundle, String, Vec<String>)> {
    let is_youtube = matches!(
        domain,
        "youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com" | "youtu.be"
    );
    is_youtube.then(|| {
        (
            AuthSiteBundle::YouTube,
            domain.to_string(),
            vec!["youtube.com".to_string()],
        )
    })
}

fn validate_reason(reason: Option<&str>) -> Result<Option<String>, AuthDomainError> {
    let Some(reason) = reason else {
        return Ok(None);
    };
    let trimmed = reason.trim();
    if trimmed.is_empty() {
        return Err(AuthDomainError::BlankReason);
    }
    if trimmed.chars().count() > MAX_AUTH_SESSION_REASON_CHARS {
        return Err(AuthDomainError::ReasonTooLong);
    }
    Ok(Some(trimmed.to_string()))
}

fn validate_window_session_id(value: &str) -> Result<(), AuthDomainError> {
    let Some(suffix) = value.strip_prefix("wsess_") else {
        return Err(AuthDomainError::InvalidWindowSessionId);
    };
    if suffix.len() != 32
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(AuthDomainError::InvalidWindowSessionId);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_dns_hosts_without_widening_generic_scope() {
        let validated = validate_domain("Example.COM.").unwrap();
        assert_eq!(validated.requested_domain(), "example.com");
        assert_eq!(validated.cookie_domains(), &["example.com".to_string()]);
        assert_eq!(validated.bundle(), AuthSiteBundle::ExactDomain);
    }

    #[test]
    fn unicode_dns_hosts_are_canonicalized_to_ascii() {
        let validated = validate_domain("bücher.example").unwrap();
        assert_eq!(validated.requested_domain(), "xn--bcher-kva.example");
    }

    #[test]
    fn rejects_broad_or_non_host_inputs() {
        for input in [
            "",
            " localhost ",
            "localhost",
            "127.0.0.1",
            "[::1]",
            "https://example.com",
            "example.com/path",
            "example.com:443",
            "user@example.com",
            "*.example.com",
            ".example.com",
            "example.com?..",
        ] {
            assert!(
                validate_domain(input).is_err(),
                "unexpectedly accepted {input:?}"
            );
        }
    }

    #[test]
    fn youtube_hosts_use_minimal_known_bundle() {
        for input in [
            "youtube.com",
            "www.youtube.com",
            "m.youtube.com",
            "music.youtube.com",
            "youtu.be",
        ] {
            let validated = validate_domain(input).unwrap();
            assert_eq!(validated.bundle(), AuthSiteBundle::YouTube);
            assert_eq!(validated.cookie_domains(), &["youtube.com".to_string()]);
        }
        let unrelated = validate_domain("notyoutube.com").unwrap();
        assert_eq!(unrelated.bundle(), AuthSiteBundle::ExactDomain);
        assert_eq!(unrelated.cookie_domains(), &["notyoutube.com".to_string()]);
    }

    #[test]
    fn request_reason_is_trimmed_and_bounded() {
        let action = AuthSessionAction::Request {
            target: AuthSessionTarget::Domain {
                domain: "example.com".to_string(),
            },
            browser: BrowserKind::Chromium,
            scope: AuthSessionScope::Once,
            reason: Some("  login required  ".to_string()),
            presentation_window_session_id: Some(
                "wsess_00000000000000000000000000000000".to_string(),
            ),
        };
        let ValidatedAuthRequest::Request { reason, .. } = validate_action(&action).unwrap() else {
            panic!("unexpected validated action");
        };
        assert_eq!(reason.as_deref(), Some("login required"));

        let blank = AuthSessionAction::Request {
            target: AuthSessionTarget::Domain {
                domain: "example.com".to_string(),
            },
            browser: BrowserKind::Chromium,
            scope: AuthSessionScope::Once,
            reason: Some("   ".to_string()),
            presentation_window_session_id: Some(
                "wsess_00000000000000000000000000000000".to_string(),
            ),
        };
        assert_eq!(validate_action(&blank), Err(AuthDomainError::BlankReason));
    }

    #[test]
    fn exact_window_session_identity_is_validated() {
        let valid = AuthSessionAction::Status {
            target: AuthSessionTarget::Window {
                window_session_id: "wsess_0123456789abcdef0123456789abcdef".to_string(),
            },
        };
        assert!(validate_action(&valid).is_ok());

        for bad in [
            "wsess_0123",
            "wsess_0123456789ABCDEF0123456789ABCDEF",
            "win_0001",
            "example.com",
        ] {
            let action = AuthSessionAction::Status {
                target: AuthSessionTarget::Window {
                    window_session_id: bad.to_string(),
                },
            };
            assert_eq!(
                validate_action(&action),
                Err(AuthDomainError::InvalidWindowSessionId)
            );
        }
    }
}
