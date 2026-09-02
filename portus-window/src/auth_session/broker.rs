use super::{
    AuthAuthorityError, AuthGrantTarget, AuthSiteBundle, AuthenticatedSessionAuthority,
    PendingAuthGrant,
};
use parking_lot::Mutex;
use portus_window_protocol::BrowserKind;
use rusqlite::{Connection, OpenFlags};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

const MAX_PROFILES_INI_BYTES: u64 = 64 * 1024;
const MAX_COOKIE_NAME_BYTES: usize = 256;
const MAX_COOKIE_VALUE_BYTES: usize = 4096;
const MAX_COOKIE_PATH_BYTES: usize = 1024;
const MAX_BROKER_COOKIES: usize = 200;
const MAX_BROKER_SECRET_BYTES: usize = 64 * 1024;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AuthBrokerError {
    #[error("authenticated-session authority error: {0}")]
    Authority(#[from] AuthAuthorityError),
    #[error("browser profile could not be resolved")]
    FirefoxProfile,
    #[error("browser cookie store is unavailable or invalid")]
    FirefoxCookieStore,
    #[error("chromium cookie broker is currently unavailable")]
    ChromiumUnavailable,
    #[error("no usable cookies were found for the authorized grant")]
    NoUsableCookies,
    #[error("cookie secret bounds exceeded during broker extraction")]
    TransferLimit,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrokerSessionMetadata {
    pub cookie_count: usize,
    pub cookie_domains: Vec<String>,
}

#[derive(Clone)]
pub(crate) struct BrokeredCookie {
    pub(crate) name: String,
    pub(crate) value: String,
    pub(crate) domain: String,
    pub(crate) path: String,
    pub(crate) expires_unix: Option<i64>,
    pub(crate) secure: bool,
    pub(crate) http_only: bool,
    pub(crate) same_site: Option<i64>,
}

impl BrokeredCookie {
    fn validate_for_handoff(
        &self,
        grant: &PendingAuthGrant,
        now: i64,
    ) -> Result<(), AuthBrokerError> {
        if !cookie_host_allowed(grant, &self.domain)
            || self.name.len() > MAX_COOKIE_NAME_BYTES
            || self.value.len() > MAX_COOKIE_VALUE_BYTES
            || self.path.len() > MAX_COOKIE_PATH_BYTES
            || !self.path.starts_with('/')
            || self.expires_unix.is_some_and(|expiry| expiry <= now)
        {
            return Err(AuthBrokerError::TransferLimit);
        }

        let _policy = (self.secure, self.http_only, self.same_site);
        Ok(())
    }
}

struct BrokeredSession {
    metadata: BrokerSessionMetadata,
    cookies: Vec<BrokeredCookie>,
}

pub struct AuthenticatedSessionBroker {
    authority: Arc<AuthenticatedSessionAuthority>,
    sessions: Mutex<BTreeMap<AuthGrantTarget, BrokeredSession>>,
    firefox: FirefoxCookieSource,
}

impl AuthenticatedSessionBroker {
    pub fn new(authority: Arc<AuthenticatedSessionAuthority>) -> Self {
        Self {
            authority,
            sessions: Mutex::new(BTreeMap::new()),
            firefox: FirefoxCookieSource::default(),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn with_firefox_root(
        authority: Arc<AuthenticatedSessionAuthority>,
        root: PathBuf,
    ) -> Self {
        Self {
            authority,
            sessions: Mutex::new(BTreeMap::new()),
            firefox: FirefoxCookieSource { root: Ok(root) },
        }
    }

    pub fn acquire(
        &self,
        target: &AuthGrantTarget,
    ) -> Result<BrokerSessionMetadata, AuthBrokerError> {
        let grant = self.authority.authorized_grant(target)?;
        let cookies = match grant.browser {
            BrowserKind::Firefox => self.firefox.load(&grant)?,
            BrowserKind::Chromium | BrowserKind::Chrome | BrowserKind::Brave => {
                return Err(AuthBrokerError::ChromiumUnavailable);
            }
        };
        if cookies.is_empty() {
            return Err(AuthBrokerError::NoUsableCookies);
        }

        let mut cookie_domains = BTreeSet::new();
        for cookie in &cookies {
            cookie_domains.insert(cookie.domain.clone());
        }
        let metadata = BrokerSessionMetadata {
            cookie_count: cookies.len(),
            cookie_domains: cookie_domains.into_iter().collect(),
        };
        self.sessions.lock().insert(
            target.clone(),
            BrokeredSession {
                metadata: metadata.clone(),
                cookies,
            },
        );
        self.authority.mark_available(target, true)?;
        Ok(metadata)
    }

    pub fn metadata(&self, target: &AuthGrantTarget) -> Option<BrokerSessionMetadata> {
        self.sessions.lock().get(target).and_then(|session| {
            (session.cookies.len() == session.metadata.cookie_count)
                .then(|| session.metadata.clone())
        })
    }

    pub fn discard(&self, target: &AuthGrantTarget) {
        self.sessions.lock().remove(target);
        let _ = self.authority.mark_available(target, false);
    }

    pub(crate) fn with_cookies<T>(
        &self,
        target: &AuthGrantTarget,
        use_cookies: impl FnOnce(&[BrokeredCookie]) -> T,
    ) -> Result<T, AuthBrokerError> {
        self.authority.authorized_grant(target)?;
        let sessions = self.sessions.lock();
        let session = sessions
            .get(target)
            .ok_or(AuthBrokerError::NoUsableCookies)?;
        Ok(use_cookies(&session.cookies))
    }
}

struct FirefoxCookieSource {
    root: Result<PathBuf, AuthBrokerError>,
}

impl Default for FirefoxCookieSource {
    fn default() -> Self {
        Self {
            root: discover_firefox_root(),
        }
    }
}

impl FirefoxCookieSource {
    fn load(&self, grant: &PendingAuthGrant) -> Result<Vec<BrokeredCookie>, AuthBrokerError> {
        let root = self.root.as_ref().map_err(Clone::clone)?;
        let profile = resolve_default_profile(root)?;
        let cookie_store = profile.join("cookies.sqlite");
        require_regular_nonsymlink(&cookie_store)
            .map_err(|_| AuthBrokerError::FirefoxCookieStore)?;

        let connection = Connection::open_with_flags(
            &cookie_store,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|_| AuthBrokerError::FirefoxCookieStore)?;

        connection
            .pragma_query_value(None, "query_only", |row| row.get::<_, i64>(0))
            .map_err(|_| AuthBrokerError::FirefoxCookieStore)?;

        let columns = moz_cookie_columns(&connection)?;
        let same_site_expr = if columns.contains("sameSite") {
            "sameSite"
        } else {
            "NULL"
        };
        let allowed_hosts = authorized_cookie_hosts(grant);
        if allowed_hosts.is_empty() {
            return Err(AuthBrokerError::NoUsableCookies);
        }
        let placeholders = (0..allowed_hosts.len())
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT name, value, host, path, expiry, isSecure, isHttpOnly, {same_site_expr} \
             FROM moz_cookies WHERE originAttributes = '' AND host IN ({placeholders})"
        );
        let mut statement = connection
            .prepare(&sql)
            .map_err(|_| AuthBrokerError::FirefoxCookieStore)?;
        let now = unix_seconds()?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(allowed_hosts.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                ))
            })
            .map_err(|_| AuthBrokerError::FirefoxCookieStore)?;

        let mut cookies = Vec::new();
        let mut total_secret_bytes = 0usize;
        for row in rows {
            let (name, value, host, path, expiry, secure, http_only, same_site) =
                row.map_err(|_| AuthBrokerError::FirefoxCookieStore)?;
            if !cookie_host_allowed(grant, &host) {
                continue;
            }
            if expiry > 0 && expiry <= now {
                continue;
            }
            total_secret_bytes += name.len() + value.len();
            if total_secret_bytes > MAX_BROKER_SECRET_BYTES || cookies.len() >= MAX_BROKER_COOKIES {
                return Err(AuthBrokerError::TransferLimit);
            }
            let cookie = BrokeredCookie {
                name,
                value,
                domain: host,
                path,
                expires_unix: (expiry > 0).then_some(expiry),
                secure: secure != 0,
                http_only: http_only != 0,
                same_site,
            };
            cookie.validate_for_handoff(grant, now)?;
            cookies.push(cookie);
        }
        Ok(cookies)
    }
}

fn discover_firefox_root() -> Result<PathBuf, AuthBrokerError> {
    #[cfg(target_os = "linux")]
    {
        let home = std::env::var_os("HOME").ok_or(AuthBrokerError::FirefoxProfile)?;
        let base = PathBuf::from(home);
        let candidates = [
            base.join(".mozilla/firefox"),
            base.join(".var/app/org.mozilla.firefox/.mozilla/firefox"),
            base.join("snap/firefox/common/.mozilla/firefox"),
        ];
        let mut found = Vec::new();
        for candidate in candidates {
            if candidate.is_dir() && candidate.join("profiles.ini").is_file() {
                found.push(candidate);
            }
        }
        match found.len() {
            0 => Err(AuthBrokerError::FirefoxProfile),
            1 => Ok(found.remove(0)),
            _ => Err(AuthBrokerError::FirefoxProfile),
        }
    }

    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var_os("APPDATA").ok_or(AuthBrokerError::FirefoxProfile)?;
        let candidate = PathBuf::from(appdata).join(r"Mozilla\Firefox");
        if candidate.is_dir() && candidate.join("profiles.ini").is_file() {
            Ok(candidate)
        } else {
            Err(AuthBrokerError::FirefoxProfile)
        }
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Err(AuthBrokerError::FirefoxProfile)
    }
}

fn resolve_default_profile(root: &Path) -> Result<PathBuf, AuthBrokerError> {
    let profiles_ini = root.join("profiles.ini");
    require_regular_nonsymlink(&profiles_ini).map_err(|_| AuthBrokerError::FirefoxProfile)?;
    let metadata = fs::metadata(&profiles_ini).map_err(|_| AuthBrokerError::FirefoxProfile)?;
    if metadata.len() > MAX_PROFILES_INI_BYTES {
        return Err(AuthBrokerError::FirefoxProfile);
    }
    let contents =
        fs::read_to_string(&profiles_ini).map_err(|_| AuthBrokerError::FirefoxProfile)?;
    let sections = parse_ini_sections(&contents);
    let mut defaults = Vec::new();
    for (section, fields) in sections {
        if !section.starts_with("Profile") || fields.get("Default").map(String::as_str) != Some("1")
        {
            continue;
        }
        if fields.get("IsRelative").map(String::as_str) != Some("1") {
            continue;
        }
        let Some(rel_path) = fields.get("Path") else {
            continue;
        };
        let rel_normalized: PathBuf = rel_path.replace('\\', "/").split('/').collect();
        let target = root.join(rel_normalized);
        if target.is_dir() {
            defaults.push(target);
        }
    }
    match defaults.len() {
        1 => Ok(defaults.remove(0)),
        _ => Err(AuthBrokerError::FirefoxProfile),
    }
}

fn require_regular_nonsymlink(path: &Path) -> Result<(), std::io::Error> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "target must be a non-symlink regular file",
        ));
    }
    Ok(())
}

fn parse_ini_sections(contents: &str) -> Vec<(String, BTreeMap<String, String>)> {
    let mut sections = Vec::new();
    let mut current_section = None;
    let mut current_fields = BTreeMap::new();
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with(';') || trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            if let Some(section_name) = current_section.take() {
                sections.push((section_name, std::mem::take(&mut current_fields)));
            }
            current_section = Some(trimmed[1..trimmed.len() - 1].to_string());
            continue;
        }
        if let Some((key, value)) = trimmed.split_once('=') {
            current_fields.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    if let Some(section_name) = current_section {
        sections.push((section_name, current_fields));
    }
    sections
}

fn moz_cookie_columns(connection: &Connection) -> Result<BTreeSet<String>, AuthBrokerError> {
    let mut statement = connection
        .prepare("PRAGMA table_info(moz_cookies)")
        .map_err(|_| AuthBrokerError::FirefoxCookieStore)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|_| AuthBrokerError::FirefoxCookieStore)?;
    let mut columns = BTreeSet::new();
    for column in rows {
        columns.insert(column.map_err(|_| AuthBrokerError::FirefoxCookieStore)?);
    }
    Ok(columns)
}

fn authorized_cookie_hosts(grant: &PendingAuthGrant) -> Vec<String> {
    let mut hosts = Vec::new();
    for domain in &grant.cookie_domains {
        hosts.push(domain.clone());
        if grant.bundle == AuthSiteBundle::YouTube {
            hosts.push(format!(".{domain}"));
        }
    }
    hosts.sort();
    hosts.dedup();
    hosts
}

fn cookie_host_allowed(grant: &PendingAuthGrant, host: &str) -> bool {
    match grant.bundle {
        AuthSiteBundle::ExactDomain => grant
            .cookie_domains
            .iter()
            .any(|domain| host == domain || host == format!(".{domain}")),
        AuthSiteBundle::YouTube => grant.cookie_domains.iter().any(|domain| {
            host == domain || host == format!(".{domain}") || host.ends_with(&format!(".{domain}"))
        }),
    }
}

fn unix_seconds() -> Result<i64, AuthBrokerError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .map_err(|_| AuthBrokerError::TransferLimit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use portus_window_protocol::AuthSessionScope;

    fn requested_authority(
        domain: &str,
        browser: BrowserKind,
    ) -> (Arc<AuthenticatedSessionAuthority>, AuthGrantTarget) {
        let authority = Arc::new(AuthenticatedSessionAuthority::new());
        let result = authority
            .request(
                super::super::ValidatedAuthRequest::Request {
                    target: Some(super::super::validate_domain(domain).unwrap()),
                    window_session_id: None,
                    browser,
                    scope: AuthSessionScope::Session,
                    reason: None,
                    presentation_window_session_id: Some(
                        "wsess_00000000000000000000000000000000".to_string(),
                    ),
                },
                None,
            )
            .unwrap();
        (
            authority,
            AuthGrantTarget::Domain(result.state.canonical_domain),
        )
    }

    fn fixture_root(cookies: &[(&str, &str, &str, &str)]) -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        let profile = directory.path().join("Profiles/default.default-release");
        fs::create_dir_all(&profile).unwrap();
        fs::write(
            directory.path().join("profiles.ini"),
            "[Profile0]\nName=default-release\nIsRelative=1\nPath=Profiles/default.default-release\nDefault=1\n",
        )
        .unwrap();

        let db_path = profile.join("cookies.sqlite");
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE moz_cookies (
                id INTEGER PRIMARY KEY,
                name TEXT,
                value TEXT,
                host TEXT,
                path TEXT,
                expiry INTEGER,
                lastAccessed INTEGER,
                creationTime INTEGER,
                isSecure INTEGER,
                isHttpOnly INTEGER,
                inBrowserElement INTEGER,
                sameSite INTEGER,
                rawSameSite INTEGER,
                schemeMap INTEGER,
                originAttributes TEXT
            );",
        )
        .unwrap();

        for (name, value, host, origin) in cookies {
            conn.execute(
                "INSERT INTO moz_cookies (name, value, host, path, expiry, isSecure, isHttpOnly, originAttributes, sameSite)
                 VALUES (?1, ?2, ?3, '/', 4102444800, 1, 1, ?4, 2)",
                rusqlite::params![name, value, host, origin],
            )
            .unwrap();
        }
        directory
    }

    #[test]
    fn loads_authorized_exact_domain_cookies() {
        let fixture = fixture_root(&[
            ("host_only", "secret-a", "example.com", ""),
            ("other_host", "ignore", "sub.example.com", ""),
            ("partitioned", "ignore", "example.com", "^partitionKey=1"),
        ]);
        let (authority, key) = requested_authority("example.com", BrowserKind::Firefox);
        authority.authorize(&key).unwrap();
        let broker = AuthenticatedSessionBroker::with_firefox_root(
            Arc::clone(&authority),
            fixture.path().to_path_buf(),
        );
        let grant = authority.authorized_grant(&key).unwrap();
        let cookies = broker.firefox.load(&grant).unwrap();
        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].name, "host_only");
        assert_eq!(cookies[0].value, "secret-a");
        assert_eq!(cookies[0].domain, "example.com");
        assert_eq!(cookies[0].path, "/");
        assert!(cookies[0].expires_unix.is_some());
        assert!(cookies[0].secure);
        assert!(cookies[0].http_only);
        assert_eq!(cookies[0].same_site, Some(2));
        let metadata = broker.acquire(&key).unwrap();
        assert_eq!(metadata.cookie_count, 1);
    }

    #[test]
    fn loads_authorized_youtube_bundle_cookies() {
        let fixture = fixture_root(&[
            ("yt_root", "val-1", "youtube.com", ""),
            ("yt_dot", "val-2", ".youtube.com", ""),
            ("google", "val-3", ".google.com", ""),
        ]);
        let (authority, key) = requested_authority("youtube.com", BrowserKind::Firefox);
        authority.authorize(&key).unwrap();
        let broker = AuthenticatedSessionBroker::with_firefox_root(
            Arc::clone(&authority),
            fixture.path().to_path_buf(),
        );
        let grant = authority.authorized_grant(&key).unwrap();
        let cookies = broker.firefox.load(&grant).unwrap();
        assert_eq!(cookies.len(), 2);
        assert!(cookies
            .iter()
            .all(|cookie| cookie.domain.ends_with("youtube.com")));
        assert!(cookies
            .iter()
            .all(|cookie| !cookie.domain.contains("google.com")));
        let metadata = broker.acquire(&key).unwrap();
        assert_eq!(metadata.cookie_count, 2);
    }

    #[test]
    fn marks_authority_available_only_after_successful_broker_load() {
        let fixture = fixture_root(&[("sid", "value", "example.com", "")]);
        let (authority, key) = requested_authority("example.com", BrowserKind::Firefox);
        let broker = AuthenticatedSessionBroker::with_firefox_root(
            Arc::clone(&authority),
            fixture.path().to_path_buf(),
        );

        authority.authorize(&key).unwrap();
        assert_eq!(broker.acquire(&key).unwrap().cookie_count, 1);
        assert!(
            authority
                .status(
                    super::super::ValidatedAuthRequest::Status {
                        target: Some(super::super::validate_domain("example.com").unwrap()),
                        window_session_id: None,
                    },
                    None,
                )
                .unwrap()
                .state
                .available
        );

        let (chromium_authority, chromium_key) =
            requested_authority("example.com", BrowserKind::Chromium);
        chromium_authority.authorize(&chromium_key).unwrap();
        let chromium_broker = AuthenticatedSessionBroker::with_firefox_root(
            Arc::clone(&chromium_authority),
            fixture.path().to_path_buf(),
        );
        assert_eq!(
            chromium_broker.acquire(&chromium_key).unwrap_err(),
            AuthBrokerError::ChromiumUnavailable
        );
    }

    #[test]
    fn metadata_debug_representation_never_exposes_cookie_values() {
        let fixture = fixture_root(&[("sid", "highly-secret-cookie-value", "example.com", "")]);
        let (authority, key) = requested_authority("example.com", BrowserKind::Firefox);
        authority.authorize(&key).unwrap();
        let broker =
            AuthenticatedSessionBroker::with_firefox_root(authority, fixture.path().to_path_buf());
        let metadata = broker.acquire(&key).unwrap();
        let rendered = format!("{metadata:?}");
        assert!(!rendered.contains("highly-secret-cookie-value"));
        assert!(!rendered.contains("sid"));
    }

    #[test]
    fn parse_ini_sections_extracts_default_relative_profile() {
        let contents = "[General]\nStartWithLastProfile=1\n\n[Profile0]\nName=default\nIsRelative=1\nPath=Profiles/abc.default\nDefault=1\n";
        let sections = parse_ini_sections(contents);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[1].0, "Profile0");
        assert_eq!(
            sections[1].1.get("Path").map(String::as_str),
            Some("Profiles/abc.default")
        );
    }
}
