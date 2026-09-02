mod authority;
mod broker;
mod consent;
mod domain;
mod webkit;

pub use domain::{
    validate_action, validate_domain, AuthDomainError, AuthSiteBundle, ValidatedAuthRequest,
    ValidatedAuthTarget,
};

pub use authority::{
    dispatch_action, AuthAuthorityError, AuthGrantTarget, AuthenticatedSessionAuthority,
    PendingAuthGrant,
};
pub(crate) use broker::BrokeredCookie;
pub use broker::{AuthBrokerError, AuthenticatedSessionBroker, BrokerSessionMetadata};
pub use webkit::{AppliedSessionMetadata, AuthWebKitError, AuthenticatedSessionWebKit};

pub(crate) use consent::consent_navigation_decision;
pub use consent::{AuthConsentController, AuthConsentError, AuthConsentView};
