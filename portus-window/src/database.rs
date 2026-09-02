use parking_lot::Mutex;
use portus_window_protocol::{
    ActiveWindow, ClearHistoryResult, CloseReason, ConfigAction, Configuration, ContentKind,
    HistoricalWindow, HistoryResult, SourceKind, MAX_HISTORY_QUERY_CHARS, MAX_HISTORY_RESULT_BYTES,
    MAX_RETENTION_DAYS, MAX_URL_HISTORY_BYTES, MAX_URL_HISTORY_ENTRIES,
    MAX_WORKSPACE_HISTORY_ENTRIES,
};
use rusqlite::{params, Connection, OpenFlags, Transaction};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;
use thiserror::Error;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use uuid::Uuid;

pub const DATABASE_FILE_NAME: &str = "history.sqlite3";
pub const DATABASE_SCHEMA_VERSION: i64 = 3;
pub const DEFAULT_RETENTION_DAYS: u32 = 15;
const MAX_HISTORY_SCAN_ROWS: usize = 10_000;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PersistenceError {
    #[error("persistence validation failed: {0}")]
    Validation(String),
    #[error("persistence storage failed: {0}")]
    Storage(String),
    #[error("persistence worker is unavailable")]
    WorkerUnavailable,
}

impl From<rusqlite::Error> for PersistenceError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

#[derive(Clone)]
pub struct DatabaseService {
    inner: Arc<DatabaseInner>,
}

struct DatabaseInner {
    tx: mpsc::Sender<DbCommand>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Drop for DatabaseInner {
    fn drop(&mut self) {
        let _ = self.tx.send(DbCommand::Shutdown);
        if let Some(worker) = self.worker.get_mut().take() {
            let _ = worker.join();
        }
    }
}

enum DbCommand {
    Open {
        window: ActiveWindow,
        reset_timeline: bool,
        reply: mpsc::Sender<Result<(), PersistenceError>>,
    },
    AbortOpen {
        window_session_id: String,
        reply: mpsc::Sender<Result<(), PersistenceError>>,
    },
    Sync {
        window: ActiveWindow,
        reply: Option<mpsc::Sender<Result<(), PersistenceError>>>,
    },
    Close {
        window: ActiveWindow,
        reason: CloseReason,
        reply: Option<mpsc::Sender<Result<(), PersistenceError>>>,
    },
    History {
        query: Option<String>,
        reply: mpsc::Sender<Result<HistoryResult, PersistenceError>>,
    },
    ClearHistory {
        reply: mpsc::Sender<Result<ClearHistoryResult, PersistenceError>>,
    },
    Config {
        action: ConfigAction,
        active_windows: Vec<ActiveWindow>,
        reply: mpsc::Sender<Result<Configuration, PersistenceError>>,
    },
    #[cfg(test)]
    PruneAt {
        now: i64,
        reply: mpsc::Sender<Result<u64, PersistenceError>>,
    },
    Flush {
        reply: mpsc::Sender<Result<(), PersistenceError>>,
    },
    Shutdown,
}

#[derive(Clone, Debug)]
struct ActiveSession {
    session_id: String,
    reset_timeline: bool,
    url_history: Vec<String>,
    url_history_truncated: bool,
    workspace_history: Vec<u32>,
    workspace_history_truncated: bool,
}

impl ActiveSession {
    fn new(session_id: String, window: &ActiveWindow, reset_timeline: bool) -> Self {
        let (url_history, url_history_truncated, workspace_history, workspace_history_truncated) =
            if reset_timeline {
                (
                    window
                        .current_url
                        .as_ref()
                        .map(|url| vec![url.clone()])
                        .unwrap_or_default(),
                    false,
                    window.workspace.into_iter().collect(),
                    false,
                )
            } else {
                (
                    window.url_history.clone().unwrap_or_default(),
                    window.url_history_truncated.unwrap_or(false),
                    window.workspace_history.clone(),
                    window.workspace_history_truncated,
                )
            };
        Self {
            session_id,
            reset_timeline,
            url_history,
            url_history_truncated,
            workspace_history,
            workspace_history_truncated,
        }
    }

    fn observe(&mut self, window: &ActiveWindow) {
        if !self.reset_timeline {
            self.url_history = window.url_history.clone().unwrap_or_default();
            self.url_history_truncated = window.url_history_truncated.unwrap_or(false);
            self.workspace_history = window.workspace_history.clone();
            self.workspace_history_truncated = window.workspace_history_truncated;
            return;
        }

        if let Some(url) = &window.current_url {
            if self.url_history.last() != Some(url) {
                self.url_history.push(url.clone());
                trim_url_history(&mut self.url_history, &mut self.url_history_truncated);
            }
        }
        if let Some(workspace) = window.workspace {
            if self.workspace_history.last() != Some(&workspace) {
                self.workspace_history.push(workspace);
                if self.workspace_history.len() > MAX_WORKSPACE_HISTORY_ENTRIES {
                    let overflow = self.workspace_history.len() - MAX_WORKSPACE_HISTORY_ENTRIES;
                    self.workspace_history.drain(0..overflow);
                    self.workspace_history_truncated = true;
                }
            }
        }
    }
}

struct DbWorker {
    conn: Connection,
    config: Configuration,
    active_sessions: HashMap<String, ActiveSession>,
}

#[derive(Debug)]
struct RawHistoricalWindow {
    historical_id: String,
    window_session_id: String,
    source_kind: String,
    content_kind: String,
    requested_source: String,
    title: String,
    rendered_url: Option<String>,
    final_url: Option<String>,
    url_history_json: String,
    url_history_truncated: bool,
    description: Option<String>,
    workspace: Option<u32>,
    is_on_all_workspaces: bool,
    workspace_history_json: String,
    workspace_history_truncated: bool,
    opened_at: i64,
    closed_at: i64,
    close_reason: String,
}

fn recv<T>(receiver: mpsc::Receiver<Result<T, PersistenceError>>) -> Result<T, PersistenceError> {
    receiver
        .recv()
        .map_err(|_| PersistenceError::WorkerUnavailable)?
}

pub fn default_database_path() -> Result<PathBuf, PersistenceError> {
    if let Some(override_dir) = std::env::var_os("PORTUS_WINDOW_DATA_DIR") {
        if override_dir.is_empty() {
            return Err(PersistenceError::Validation(
                "PORTUS_WINDOW_DATA_DIR must not be empty".to_string(),
            ));
        }
        let directory = PathBuf::from(override_dir);
        if !directory.is_absolute() {
            return Err(PersistenceError::Validation(
                "PORTUS_WINDOW_DATA_DIR must be an absolute path".to_string(),
            ));
        }
        return Ok(directory.join("portus-window").join(DATABASE_FILE_NAME));
    }
    let directory = dirs::data_local_dir().ok_or_else(|| {
        PersistenceError::Storage("could not determine the local data directory".to_string())
    })?;
    Ok(directory.join("portus-window").join(DATABASE_FILE_NAME))
}

fn validate_persistence_directory(path: &Path) -> Result<(), PersistenceError> {
    let parent = path.parent().ok_or_else(|| {
        PersistenceError::Validation("database path must have a parent directory".to_string())
    })?;
    let metadata = std::fs::symlink_metadata(parent).map_err(|error| {
        PersistenceError::Storage(format!(
            "could not inspect persistence directory '{}': {error}",
            parent.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PersistenceError::Validation(format!(
            "persistence directory '{}' must be a real directory, not a symbolic link",
            parent.display()
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn secure_persistence_directory(path: &Path) -> Result<(), PersistenceError> {
    use std::os::unix::fs::PermissionsExt;

    let parent = path.parent().ok_or_else(|| {
        PersistenceError::Validation("database path must have a parent directory".to_string())
    })?;
    let metadata = std::fs::symlink_metadata(parent).map_err(|error| {
        PersistenceError::Storage(format!(
            "could not inspect persistence directory '{}': {error}",
            parent.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(PersistenceError::Validation(format!(
            "persistence directory '{}' must be a real directory, not a symbolic link",
            parent.display()
        )));
    }
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).map_err(|error| {
        PersistenceError::Storage(format!(
            "could not secure persistence directory '{}': {error}",
            parent.display()
        ))
    })
}

#[cfg(not(unix))]
fn secure_persistence_directory(_path: &Path) -> Result<(), PersistenceError> {
    Ok(())
}

#[cfg(unix)]
fn secure_database_file(path: &Path) -> Result<(), PersistenceError> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|error| {
        PersistenceError::Storage(format!(
            "could not secure database file '{}': {error}",
            path.display()
        ))
    })
}

#[cfg(not(unix))]
fn secure_database_file(_path: &Path) -> Result<(), PersistenceError> {
    Ok(())
}

fn reject_symlink_path(path: &Path, purpose: &str) -> Result<(), PersistenceError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(PersistenceError::Validation(
            format!("{purpose} '{}' must not be a symbolic link", path.display()),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(PersistenceError::Storage(format!(
            "could not inspect {purpose} '{}': {error}",
            path.display()
        ))),
    }
}

fn validate_history_query(query: Option<String>) -> Result<Option<String>, PersistenceError> {
    let Some(query) = query else {
        return Ok(None);
    };
    let query = query.trim();
    if query.is_empty() {
        return Err(PersistenceError::Validation(
            "history query must not be blank".to_string(),
        ));
    }
    if query.chars().count() > MAX_HISTORY_QUERY_CHARS {
        return Err(PersistenceError::Validation(format!(
            "history query must be at most {MAX_HISTORY_QUERY_CHARS} characters"
        )));
    }
    Ok(Some(query.to_string()))
}

fn validate_config_action(action: &ConfigAction) -> Result<(), PersistenceError> {
    if let ConfigAction::SetRetentionDays { days: Some(days) } = action {
        if !(1..=MAX_RETENTION_DAYS).contains(days) {
            return Err(PersistenceError::Validation(format!(
                "retention_days must be null or between 1 and {MAX_RETENTION_DAYS}"
            )));
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DatabaseSchemaState {
    Fresh,
    Current,
}

fn database_has_user_objects(conn: &Connection) -> Result<bool, PersistenceError> {
    let exists: i64 = conn.query_row(
        "SELECT EXISTS(
            SELECT 1
            FROM sqlite_schema
            WHERE type IN ('table', 'view', 'trigger')
              AND name NOT LIKE 'sqlite_%'
        )",
        [],
        |row| row.get(0),
    )?;
    Ok(exists != 0)
}

fn inspect_database_schema(conn: &Connection) -> Result<DatabaseSchemaState, PersistenceError> {
    let version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    match version {
        0 if !database_has_user_objects(conn)? => Ok(DatabaseSchemaState::Fresh),
        0 => Err(PersistenceError::Storage(format!(
            "database is unversioned and not empty; expected schema version {DATABASE_SCHEMA_VERSION}"
        ))),
        DATABASE_SCHEMA_VERSION => Ok(DatabaseSchemaState::Current),
        other => Err(PersistenceError::Storage(format!(
            "database schema version {other} is unsupported; expected version {DATABASE_SCHEMA_VERSION}"
        ))),
    }
}

fn initialize_schema(
    conn: &mut Connection,
    state: DatabaseSchemaState,
) -> Result<(), PersistenceError> {
    if state == DatabaseSchemaState::Current {
        return Ok(());
    }

    let tx = conn.transaction()?;
    tx.execute_batch(
        "CREATE TABLE sessions (
            session_id TEXT PRIMARY KEY NOT NULL,
            window_session_id TEXT NOT NULL,
            source_kind TEXT NOT NULL,
            content_kind TEXT NOT NULL,
            requested_source TEXT NOT NULL,
            title TEXT NOT NULL,
            rendered_url TEXT,
            final_url TEXT,
            url_history_json TEXT NOT NULL,
            url_history_truncated INTEGER NOT NULL,
            description TEXT,
            workspace INTEGER,
            is_on_all_workspaces INTEGER NOT NULL,
            workspace_history_json TEXT NOT NULL,
            workspace_history_truncated INTEGER NOT NULL,
            opened_at INTEGER NOT NULL,
            closed_at INTEGER,
            close_reason TEXT
        );
        CREATE INDEX sessions_closed_at_idx ON sessions(closed_at);
        CREATE INDEX sessions_window_session_id_idx ON sessions(window_session_id);
        CREATE TABLE config (
            key TEXT PRIMARY KEY NOT NULL,
            value TEXT NOT NULL
        );",
    )?;
    tx.pragma_update(None, "user_version", DATABASE_SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(())
}

fn initialize_config(conn: &Connection) -> Result<(), PersistenceError> {
    conn.execute(
        "INSERT OR IGNORE INTO config(key, value) VALUES('history_enabled', 'true')",
        [],
    )?;
    conn.execute(
        "INSERT OR IGNORE INTO config(key, value) VALUES('retention_days', ?1)",
        params![DEFAULT_RETENTION_DAYS.to_string()],
    )?;
    Ok(())
}

fn load_config(conn: &Connection) -> Result<Configuration, PersistenceError> {
    let history_enabled = config_value(conn, "history_enabled")?;
    let retention_days = config_value(conn, "retention_days")?;
    let history_enabled = match history_enabled.as_str() {
        "true" => true,
        "false" => false,
        _ => {
            return Err(PersistenceError::Storage(
                "stored history_enabled value is invalid".to_string(),
            ))
        }
    };
    let retention_days = if retention_days == "null" {
        None
    } else {
        let days = retention_days.parse::<u32>().map_err(|_| {
            PersistenceError::Storage("stored retention_days value is invalid".to_string())
        })?;
        if !(1..=MAX_RETENTION_DAYS).contains(&days) {
            return Err(PersistenceError::Storage(
                "stored retention_days value is outside the supported range".to_string(),
            ));
        }
        Some(days)
    };
    Ok(Configuration {
        history_enabled,
        retention_days,
    })
}

fn config_value(conn: &Connection, key: &str) -> Result<String, PersistenceError> {
    conn.query_row(
        "SELECT value FROM config WHERE key = ?1",
        params![key],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn set_config_value(conn: &Connection, key: &str, value: &str) -> Result<(), PersistenceError> {
    conn.execute(
        "INSERT INTO config(key, value) VALUES(?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

fn insert_session(
    conn: &Connection,
    session_id: &str,
    window: &ActiveWindow,
    opened_at: i64,
    reset_timeline: bool,
) -> Result<(), PersistenceError> {
    insert_session_impl(conn, session_id, window, opened_at, reset_timeline)
}

fn insert_session_tx(
    tx: &Transaction<'_>,
    session_id: &str,
    window: &ActiveWindow,
    opened_at: i64,
    reset_timeline: bool,
) -> Result<(), PersistenceError> {
    insert_session_impl(tx, session_id, window, opened_at, reset_timeline)
}

fn insert_session_impl(
    conn: &Connection,
    session_id: &str,
    window: &ActiveWindow,
    opened_at: i64,
    reset_timeline: bool,
) -> Result<(), PersistenceError> {
    let (url_history, url_history_truncated, workspace_history, workspace_history_truncated) =
        if reset_timeline {
            (
                window
                    .current_url
                    .as_ref()
                    .map(|url| vec![url.clone()])
                    .unwrap_or_default(),
                false,
                window.workspace.into_iter().collect(),
                false,
            )
        } else {
            (
                window.url_history.clone().unwrap_or_default(),
                window.url_history_truncated.unwrap_or(false),
                window.workspace_history.clone(),
                window.workspace_history_truncated,
            )
        };
    conn.execute(
        "INSERT INTO sessions(
            session_id, window_session_id, source_kind, content_kind, requested_source, title,
            rendered_url, final_url, url_history_json, url_history_truncated, description, workspace,
            is_on_all_workspaces, workspace_history_json, workspace_history_truncated,
            opened_at, closed_at, close_reason
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, NULL, NULL)",
        params![
            session_id,
            window.window_session_id,
            source_kind_str(window.source_kind),
            content_kind_str(window.content_kind),
            window.requested_source,
            window.title,
            window.rendered_url,
            window.current_url,
            to_json(&url_history)?,
            bool_int(url_history_truncated),
            window.description,
            window.workspace,
            bool_int(window.is_on_all_workspaces),
            to_json(&workspace_history)?,
            bool_int(workspace_history_truncated),
            opened_at,
        ],
    )?;
    Ok(())
}

fn update_session(
    conn: &Connection,
    session: &ActiveSession,
    window: &ActiveWindow,
) -> Result<(), PersistenceError> {
    conn.execute(
        "UPDATE sessions SET
            title = ?2,
            rendered_url = ?3,
            final_url = ?4,
            url_history_json = ?5,
            url_history_truncated = ?6,
            description = ?7,
            workspace = ?8,
            is_on_all_workspaces = ?9,
            workspace_history_json = ?10,
            workspace_history_truncated = ?11
         WHERE session_id = ?1 AND closed_at IS NULL",
        params![
            session.session_id,
            window.title,
            window.rendered_url,
            window.current_url,
            to_json(&session.url_history)?,
            bool_int(session.url_history_truncated),
            window.description,
            window.workspace,
            bool_int(window.is_on_all_workspaces),
            to_json(&session.workspace_history)?,
            bool_int(session.workspace_history_truncated),
        ],
    )?;
    Ok(())
}

fn close_session_row(
    conn: &Connection,
    session: &ActiveSession,
    window: &ActiveWindow,
    reason: CloseReason,
    closed_at: i64,
) -> Result<(), PersistenceError> {
    conn.execute(
        "UPDATE sessions SET
            title = ?2,
            rendered_url = ?3,
            final_url = ?4,
            url_history_json = ?5,
            url_history_truncated = ?6,
            description = ?7,
            workspace = ?8,
            is_on_all_workspaces = ?9,
            workspace_history_json = ?10,
            workspace_history_truncated = ?11,
            closed_at = ?12,
            close_reason = ?13
         WHERE session_id = ?1 AND closed_at IS NULL",
        params![
            session.session_id,
            window.title,
            window.rendered_url,
            window.current_url,
            to_json(&session.url_history)?,
            bool_int(session.url_history_truncated),
            window.description,
            window.workspace,
            bool_int(window.is_on_all_workspaces),
            to_json(&session.workspace_history)?,
            bool_int(session.workspace_history_truncated),
            closed_at,
            close_reason_str(reason),
        ],
    )?;
    Ok(())
}

fn trim_url_history(history: &mut Vec<String>, truncated: &mut bool) {
    while history.len() > MAX_URL_HISTORY_ENTRIES
        || history.iter().map(String::len).sum::<usize>() > MAX_URL_HISTORY_BYTES
    {
        if history.is_empty() {
            break;
        }
        history.remove(0);
        *truncated = true;
    }
}

fn prune_closed(
    conn: &Connection,
    config: &Configuration,
    now: i64,
) -> Result<u64, PersistenceError> {
    let Some(days) = config.retention_days else {
        return Ok(0);
    };
    let cutoff = now.saturating_sub(i64::from(days) * 86_400);
    let deleted = conn.execute(
        "DELETE FROM sessions WHERE closed_at IS NOT NULL AND closed_at < ?1",
        params![cutoff],
    )?;
    Ok(deleted as u64)
}

fn raw_to_historical(raw: RawHistoricalWindow) -> Result<HistoricalWindow, PersistenceError> {
    Ok(HistoricalWindow {
        historical_id: raw.historical_id,
        window_session_id: raw.window_session_id,
        source_kind: parse_source_kind(&raw.source_kind)?,
        content_kind: parse_content_kind(&raw.content_kind)?,
        requested_source: raw.requested_source,
        title: raw.title,
        rendered_url: raw.rendered_url,
        final_url: raw.final_url,
        url_history: from_json(&raw.url_history_json)?,
        url_history_truncated: raw.url_history_truncated,
        description: raw.description,
        workspace: raw.workspace,
        is_on_all_workspaces: raw.is_on_all_workspaces,
        workspace_history: from_json(&raw.workspace_history_json)?,
        workspace_history_truncated: raw.workspace_history_truncated,
        opened_at: format_timestamp(raw.opened_at)?,
        closed_at: format_timestamp(raw.closed_at)?,
        close_reason: parse_close_reason(&raw.close_reason)?,
    })
}

fn history_matches(entry: &HistoricalWindow, query: &str) -> bool {
    entry.window_session_id.to_lowercase().contains(query)
        || entry.title.to_lowercase().contains(query)
        || entry.requested_source.to_lowercase().contains(query)
        || entry
            .description
            .as_deref()
            .is_some_and(|value| value.to_lowercase().contains(query))
        || entry
            .final_url
            .as_deref()
            .is_some_and(|value| value.to_lowercase().contains(query))
}

fn source_kind_str(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Web => "web",
        SourceKind::LocalMedia => "local_media",
    }
}

fn parse_source_kind(value: &str) -> Result<SourceKind, PersistenceError> {
    match value {
        "web" => Ok(SourceKind::Web),
        "local_media" => Ok(SourceKind::LocalMedia),
        _ => Err(PersistenceError::Storage(format!(
            "stored source_kind '{value}' is invalid"
        ))),
    }
}

fn content_kind_str(kind: ContentKind) -> &'static str {
    match kind {
        ContentKind::Web => "web",
        ContentKind::Image => "image",
        ContentKind::Video => "video",
        ContentKind::Audio => "audio",
    }
}

fn parse_content_kind(value: &str) -> Result<ContentKind, PersistenceError> {
    match value {
        "web" => Ok(ContentKind::Web),
        "image" => Ok(ContentKind::Image),
        "video" => Ok(ContentKind::Video),
        "audio" => Ok(ContentKind::Audio),
        _ => Err(PersistenceError::Storage(format!(
            "stored content_kind '{value}' is invalid"
        ))),
    }
}

fn close_reason_str(reason: CloseReason) -> &'static str {
    match reason {
        CloseReason::Explicit => "explicit",
        CloseReason::Destroyed => "destroyed",
        CloseReason::AbruptTermination => "abrupt_termination",
    }
}

fn parse_close_reason(value: &str) -> Result<CloseReason, PersistenceError> {
    match value {
        "explicit" => Ok(CloseReason::Explicit),
        "destroyed" => Ok(CloseReason::Destroyed),
        "abrupt_termination" => Ok(CloseReason::AbruptTermination),
        _ => Err(PersistenceError::Storage(format!(
            "stored close_reason '{value}' is invalid"
        ))),
    }
}

fn bool_int(value: bool) -> i64 {
    i64::from(value)
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<String, PersistenceError> {
    serde_json::to_string(value).map_err(|error| PersistenceError::Storage(error.to_string()))
}

fn from_json<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, PersistenceError> {
    serde_json::from_str(value).map_err(|error| PersistenceError::Storage(error.to_string()))
}

fn now_unix() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp()
}

fn format_timestamp(timestamp: i64) -> Result<String, PersistenceError> {
    OffsetDateTime::from_unix_timestamp(timestamp)
        .map_err(|error| PersistenceError::Storage(error.to_string()))?
        .format(&Rfc3339)
        .map_err(|error| PersistenceError::Storage(error.to_string()))
}

#[cfg(test)]
mod tests;

mod service;
