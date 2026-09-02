use std::path::{Path, PathBuf};
use thiserror::Error;

pub const WEB_PROFILE_DIR_NAME: &str = "web-profile";
pub const PROFILES_DIR_NAME: &str = "profiles";
pub const AUTH_SESSION_PROFILE_DIR_NAME: &str = "auth-session-profiles";

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum WebProfileError {
    #[error("web profile validation failed: {0}")]
    Validation(String),
    #[error("web profile storage operation failed: {0}")]
    Storage(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebProfile {
    data_directory: PathBuf,
    profiles_root: PathBuf,
    auth_session_root: PathBuf,
}

impl WebProfile {
    pub fn open_default() -> Result<Self, WebProfileError> {
        #[cfg(target_os = "linux")]
        if let Some(override_dir) = std::env::var_os("PORTUS_WINDOW_DATA_DIR") {
            if override_dir.is_empty() {
                return Err(WebProfileError::Validation(
                    "PORTUS_WINDOW_DATA_DIR must not be empty".to_string(),
                ));
            }
            let directory = PathBuf::from(override_dir);
            if !directory.is_absolute() {
                return Err(WebProfileError::Validation(
                    "PORTUS_WINDOW_DATA_DIR must be an absolute path".to_string(),
                ));
            }
            return Self::open(directory.join("portus-window"));
        }
        let local_dir = dirs::data_local_dir().ok_or_else(|| {
            WebProfileError::Storage("could not resolve local data directory".to_string())
        })?;
        Self::open(local_dir.join("portus-window"))
    }

    pub fn open(data_directory: PathBuf) -> Result<Self, WebProfileError> {
        if data_directory.as_os_str().is_empty() {
            return Err(WebProfileError::Validation(
                "web profile data directory must not be empty".to_string(),
            ));
        }
        if !data_directory.is_absolute() {
            return Err(WebProfileError::Validation(
                "web profile data directory must be an absolute path".to_string(),
            ));
        }
        match std::fs::symlink_metadata(&data_directory) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(WebProfileError::Validation(format!(
                        "web profile data directory '{}' must be a real directory, not a symbolic link",
                        data_directory.display()
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(&data_directory).map_err(|error| {
                    WebProfileError::Storage(format!(
                        "could not create web profile directory '{}': {error}",
                        data_directory.display()
                    ))
                })?;
            }
            Err(error) => {
                return Err(WebProfileError::Storage(format!(
                    "could not inspect web profile directory '{}': {error}",
                    data_directory.display()
                )));
            }
        }
        let metadata = std::fs::symlink_metadata(&data_directory).map_err(|error| {
            WebProfileError::Storage(format!(
                "could not inspect web profile directory '{}': {error}",
                data_directory.display()
            ))
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(WebProfileError::Validation(format!(
                "web profile data directory '{}' must be a real directory, not a symbolic link",
                data_directory.display()
            )));
        }
        migrate_legacy_browser_storage(&data_directory)?;
        secure_private_directory(&data_directory)?;

        let app_directory = data_directory.parent().ok_or_else(|| {
            WebProfileError::Validation("web profile directory has no parent".to_string())
        })?;
        let profiles_root = app_directory.join(PROFILES_DIR_NAME);
        prepare_private_root(&profiles_root)?;

        let auth_session_root = app_directory.join(AUTH_SESSION_PROFILE_DIR_NAME);
        prepare_private_root(&auth_session_root)?;
        purge_private_directory_contents(&auth_session_root)?;
        Ok(Self {
            data_directory,
            profiles_root,
            auth_session_root,
        })
    }

    pub fn data_directory(&self) -> &Path {
        &self.data_directory
    }

    pub fn profile_data_directory(&self, name: Option<&str>) -> Result<PathBuf, WebProfileError> {
        match name {
            None | Some("default") => Ok(self.data_directory.clone()),
            Some(named) => self.get_or_create_named_profile(named),
        }
    }

    pub fn get_or_create_named_profile(&self, name: &str) -> Result<PathBuf, WebProfileError> {
        validate_profile_name(name)?;
        let path = self.profiles_root.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(WebProfileError::Validation(format!(
                        "named profile '{}' must be a real directory",
                        path.display()
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(&path).map_err(|error| {
                    WebProfileError::Storage(format!(
                        "could not create named profile directory '{}': {error}",
                        path.display()
                    ))
                })?;
            }
            Err(error) => {
                return Err(WebProfileError::Storage(format!(
                    "could not inspect named profile directory '{}': {error}",
                    path.display()
                )));
            }
        }
        migrate_legacy_browser_storage(&path)?;
        secure_private_directory(&path)?;
        Ok(path)
    }

    pub(crate) fn prepare_auth_session_directory(
        &self,
        window_session_id: &str,
    ) -> Result<PathBuf, WebProfileError> {
        validate_window_session_id(window_session_id)?;
        let path = self.auth_session_root.join(window_session_id);
        if path.exists() {
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
                WebProfileError::Storage(format!(
                    "could not inspect auth-session profile directory '{}': {error}",
                    path.display()
                ))
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(WebProfileError::Validation(format!(
                    "auth-session profile directory '{}' must be a real directory",
                    path.display()
                )));
            }
            purge_private_directory_contents(&path)?;
        } else {
            std::fs::create_dir_all(&path).map_err(|error| {
                WebProfileError::Storage(format!(
                    "could not create auth-session profile directory '{}': {error}",
                    path.display()
                ))
            })?;
        }
        secure_private_directory(&path)?;
        Ok(path)
    }

    pub(crate) fn remove_auth_session_directory(
        &self,
        window_session_id: &str,
    ) -> Result<(), WebProfileError> {
        validate_window_session_id(window_session_id)?;
        let path = self.auth_session_root.join(window_session_id);
        if !path.exists() {
            return Ok(());
        }
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            WebProfileError::Storage(format!(
                "could not inspect auth-session profile directory '{}': {error}",
                path.display()
            ))
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(WebProfileError::Validation(format!(
                "auth-session profile directory '{}' must be a real directory",
                path.display()
            )));
        }
        std::fs::remove_dir_all(&path).map_err(|error| {
            WebProfileError::Storage(format!(
                "could not remove auth-session profile directory '{}': {error}",
                path.display()
            ))
        })
    }
}

pub fn validate_profile_name(name: &str) -> Result<(), WebProfileError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(WebProfileError::Validation(
            "profile name must not be empty".to_string(),
        ));
    }
    if trimmed.len() > 64 {
        return Err(WebProfileError::Validation(
            "profile name must be at most 64 characters".to_string(),
        ));
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(WebProfileError::Validation(
            "profile name may only contain alphanumeric characters, dashes, and underscores"
                .to_string(),
        ));
    }
    Ok(())
}

pub fn is_auth_cookie_name(name: &str) -> bool {
    let normalized = name.trim().to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "sid"
            | "ssid"
            | "hsid"
            | "sapisid"
            | "user_session"
            | "logged_in"
            | "dotcom_user"
            | "d"
            | "d-s"
            | "token"
            | "session"
            | "connect.sid"
            | "auth_token"
            | "authtoken"
            | "__session"
            | "user_id"
            | "auth"
            | "jwt"
    ) || normalized.contains("session")
        || normalized.contains("authtoken")
}

fn validate_window_session_id(value: &str) -> Result<(), WebProfileError> {
    let Some(suffix) = value.strip_prefix("wsess_") else {
        return Err(WebProfileError::Validation(
            "window session ID must start with 'wsess_'".to_string(),
        ));
    };
    if suffix.len() != 32 || !suffix.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(WebProfileError::Validation(
            "window session ID must be 'wsess_' followed by 32 hexadecimal digits".to_string(),
        ));
    }
    Ok(())
}

fn prepare_private_root(path: &Path) -> Result<(), WebProfileError> {
    if path.exists() {
        let metadata = std::fs::symlink_metadata(path).map_err(|error| {
            WebProfileError::Storage(format!(
                "could not inspect profile root '{}': {error}",
                path.display()
            ))
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(WebProfileError::Validation(format!(
                "profile root '{}' must be a real directory",
                path.display()
            )));
        }
    } else {
        std::fs::create_dir_all(path).map_err(|error| {
            WebProfileError::Storage(format!(
                "could not create profile root '{}': {error}",
                path.display()
            ))
        })?;
    }
    secure_private_directory(path)
}

fn migrate_legacy_browser_storage(path: &Path) -> Result<(), WebProfileError> {
    const LEGACY_BROWSER_DIRECTORIES: &[&str] = &[
        "cookies",
        "local-storage",
        "indexeddb",
        "service-workers",
        "hsts",
    ];

    for name in LEGACY_BROWSER_DIRECTORIES {
        let legacy_path = path.join(name);
        match std::fs::symlink_metadata(&legacy_path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                continue;
            }
            Ok(_) => {
                std::fs::remove_dir_all(&legacy_path).map_err(|error| {
                    WebProfileError::Storage(format!(
                        "could not remove legacy browser storage directory '{}': {error}",
                        legacy_path.display()
                    ))
                })?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(WebProfileError::Storage(format!(
                    "could not inspect legacy browser storage path '{}': {error}",
                    legacy_path.display()
                )));
            }
        }
    }
    Ok(())
}

fn purge_private_directory_contents(path: &Path) -> Result<(), WebProfileError> {
    if !path.exists() {
        return Ok(());
    }
    let entries = std::fs::read_dir(path).map_err(|error| {
        WebProfileError::Storage(format!(
            "could not read directory '{}' during purge: {error}",
            path.display()
        ))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            WebProfileError::Storage(format!(
                "could not inspect entry in '{}': {error}",
                path.display()
            ))
        })?;
        let entry_path = entry.path();
        let metadata = std::fs::symlink_metadata(&entry_path).map_err(|error| {
            WebProfileError::Storage(format!(
                "could not inspect metadata for '{}': {error}",
                entry_path.display()
            ))
        })?;
        if metadata.file_type().is_symlink() || metadata.is_file() {
            std::fs::remove_file(&entry_path).map_err(|error| {
                WebProfileError::Storage(format!(
                    "could not remove file '{}': {error}",
                    entry_path.display()
                ))
            })?;
        } else if metadata.is_dir() {
            std::fs::remove_dir_all(&entry_path).map_err(|error| {
                WebProfileError::Storage(format!(
                    "could not remove directory '{}': {error}",
                    entry_path.display()
                ))
            })?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn secure_private_directory(path: &Path) -> Result<(), WebProfileError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(|error| {
        WebProfileError::Storage(format!(
            "could not set private permissions on '{}': {error}",
            path.display()
        ))
    })
}

#[cfg(not(unix))]
fn secure_private_directory(_path: &Path) -> Result<(), WebProfileError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_browser_storage_directories_are_removed_on_open() {
        let root = tempfile::tempdir().unwrap();
        let web = root.path().join(WEB_PROFILE_DIR_NAME);
        std::fs::create_dir_all(web.join("cookies")).unwrap();
        std::fs::create_dir_all(web.join("local-storage")).unwrap();
        std::fs::write(web.join("unrelated.txt"), b"keep").unwrap();

        let _profile = WebProfile::open(web.clone()).unwrap();

        assert!(!web.join("cookies").exists());
        assert!(!web.join("local-storage").exists());
        assert!(web.join("unrelated.txt").exists());
    }

    #[test]
    fn persistent_cookie_file_is_preserved() {
        let root = tempfile::tempdir().unwrap();
        let web = root.path().join(WEB_PROFILE_DIR_NAME);
        std::fs::create_dir_all(&web).unwrap();
        std::fs::write(web.join("cookies"), b"persistent-cookie-store").unwrap();

        let _profile = WebProfile::open(web.clone()).unwrap();

        assert_eq!(
            std::fs::read(web.join("cookies")).unwrap(),
            b"persistent-cookie-store"
        );
    }

    #[test]
    fn web_profile_creates_only_the_profile_root() {
        let root = tempfile::tempdir().unwrap();
        let web = root.path().join(WEB_PROFILE_DIR_NAME);
        let profile = WebProfile::open(web.clone()).unwrap();
        assert_eq!(profile.data_directory(), web.as_path());
        assert!(web.is_dir());
        for subdir in [
            "cookies",
            "local-storage",
            "indexeddb",
            "service-workers",
            "hsts",
        ] {
            assert!(!web.join(subdir).exists());
        }
    }

    #[test]
    fn named_profiles_are_isolated_without_managed_storage_layout() {
        let root = tempfile::tempdir().unwrap();
        let web = root.path().join(WEB_PROFILE_DIR_NAME);
        let profile = WebProfile::open(web.clone()).unwrap();

        let default_dir = profile.profile_data_directory(None).unwrap();
        assert_eq!(default_dir, web);

        let work_dir = profile.profile_data_directory(Some("work")).unwrap();
        assert!(work_dir.ends_with("profiles/work"));
        assert!(work_dir.is_dir());
        assert!(!work_dir.join("cookies").exists());
        assert!(!work_dir.join("indexeddb").exists());
        assert!(!work_dir.join("local-storage").exists());

        assert!(profile
            .profile_data_directory(Some("invalid name!"))
            .is_err());
    }

    #[test]
    fn auth_cookie_names_are_recognized() {
        assert!(is_auth_cookie_name("SID"));
        assert!(is_auth_cookie_name("user_session"));
        assert!(is_auth_cookie_name("myAuthToken"));
        assert!(!is_auth_cookie_name("theme"));
    }

    #[test]
    fn empty_or_relative_profile_directory_is_rejected() {
        assert!(matches!(
            WebProfile::open(PathBuf::from("")),
            Err(WebProfileError::Validation(_))
        ));
        assert!(matches!(
            WebProfile::open(PathBuf::from("relative-profile")),
            Err(WebProfileError::Validation(_))
        ));
    }
}
