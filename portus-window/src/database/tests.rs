use super::*;
use portus_window_protocol::{LoadState, MediaState};
use tempfile::TempDir;

fn database_path(dir: &TempDir) -> PathBuf {
    dir.path().join("history.sqlite3")
}

fn web_window(window_session_id: &str, url: &str) -> ActiveWindow {
    ActiveWindow {
        window_session_id: window_session_id.to_string(),
        source_kind: SourceKind::Web,
        content_kind: ContentKind::Web,
        requested_source: url.to_string(),
        current_url: Some(url.to_string()),
        rendered_url: None,
        url_history: Some(vec![url.to_string()]),
        url_history_truncated: Some(false),
        title: "Example".to_string(),
        load_state: LoadState::Loaded,
        load_error: None,
        console_errors: Vec::new(),
        console_errors_truncated: false,
        media_state: None,
        description: Some("demo".to_string()),
        width: 1024,
        height: 768,
        x: 0,
        y: 0,
        is_maximized: false,
        is_minimized: false,
        is_focused: true,
        is_always_on_top: false,
        workspace: Some(1),
        is_on_all_workspaces: false,
        workspace_history: vec![0, 1],
        workspace_history_truncated: false,
        authenticated: Some(false),
        profile: None,
    }
}

fn video_window(window_session_id: &str) -> ActiveWindow {
    let mut window = web_window(window_session_id, "/tmp/demo.mp4");
    window.source_kind = SourceKind::LocalMedia;
    window.content_kind = ContentKind::Video;
    window.current_url = None;
    window.url_history = None;
    window.url_history_truncated = None;
    window.media_state = Some(MediaState::default());
    window.title = "demo.mp4".to_string();
    window
}

#[test]
fn fresh_database_initializes_current_schema_and_default_config() {
    let dir = TempDir::new().unwrap();
    let path = database_path(&dir);
    let service = DatabaseService::open(path.clone()).unwrap();
    let config = service.config(ConfigAction::Show, vec![]).unwrap();
    assert_eq!(
        config,
        Configuration {
            history_enabled: true,
            retention_days: Some(DEFAULT_RETENTION_DAYS)
        }
    );
    service.flush().unwrap();
    let conn = Connection::open(path).unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, DATABASE_SCHEMA_VERSION);
}

fn assert_unsupported_schema_is_untouched(version: i64) {
    let dir = TempDir::new().unwrap();
    let path = database_path(&dir);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE marker (value TEXT NOT NULL);
         INSERT INTO marker(value) VALUES('preserve-me');",
    )
    .unwrap();
    conn.pragma_update(None, "user_version", version).unwrap();
    let journal_mode_before: String = conn
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    drop(conn);

    assert!(matches!(
        DatabaseService::open(path.clone()),
        Err(PersistenceError::Storage(message))
            if message.contains("unsupported")
                && message.contains(&DATABASE_SCHEMA_VERSION.to_string())
    ));

    let conn = Connection::open(path).unwrap();
    let version_after: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    let journal_mode_after: String = conn
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    let marker: String = conn
        .query_row("SELECT value FROM marker", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version_after, version);
    assert_eq!(journal_mode_after, journal_mode_before);
    assert_eq!(marker, "preserve-me");
}

#[test]
fn unsupported_nonzero_schema_versions_are_rejected_without_mutation() {
    for version in [1, 2, DATABASE_SCHEMA_VERSION + 1] {
        assert_unsupported_schema_is_untouched(version);
    }
}

#[test]
fn nonempty_unversioned_database_is_rejected_without_adoption() {
    let dir = TempDir::new().unwrap();
    let path = database_path(&dir);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE marker (value TEXT NOT NULL);
         INSERT INTO marker(value) VALUES('preserve-me');",
    )
    .unwrap();
    let journal_mode_before: String = conn
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    drop(conn);

    assert!(matches!(
        DatabaseService::open(path.clone()),
        Err(PersistenceError::Storage(message))
            if message.contains("unversioned") && message.contains("not empty")
    ));

    let conn = Connection::open(path).unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    let journal_mode_after: String = conn
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    let marker: String = conn
        .query_row("SELECT value FROM marker", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 0);
    assert_eq!(journal_mode_after, journal_mode_before);
    assert_eq!(marker, "preserve-me");
}
#[test]
fn schema_v1_is_rejected_without_mutation() {
    assert_unsupported_schema_is_untouched(1);
}

#[test]
fn schema_v2_is_rejected_without_mutation() {
    assert_unsupported_schema_is_untouched(2);
}

#[test]
fn future_schema_is_rejected_without_mutation() {
    assert_unsupported_schema_is_untouched(DATABASE_SCHEMA_VERSION + 1);
}

#[test]
fn source_aware_sessions_sync_close_query_and_reuse_window_session_ids() {
    let dir = TempDir::new().unwrap();
    let service = DatabaseService::open(database_path(&dir)).unwrap();
    let mut first = web_window(
        "wsess_00000000000000000000000000000001",
        "https://example.com/",
    );
    service.track_open(first.clone()).unwrap();
    first.title = "Updated".to_string();
    first.current_url = Some("https://example.com/next".to_string());
    first
        .url_history
        .as_mut()
        .unwrap()
        .push("https://example.com/next".to_string());
    service.sync(first.clone()).unwrap();
    service.close(first, CloseReason::Explicit).unwrap();

    let second = video_window("wsess_00000000000000000000000000000001");
    service.track_open(second.clone()).unwrap();
    service.close(second, CloseReason::Destroyed).unwrap();

    let history = service.history(None).unwrap();
    assert_eq!(history.entries.len(), 2);
    assert_ne!(
        history.entries[0].historical_id,
        history.entries[1].historical_id
    );
    assert_eq!(
        history.entries[0].window_session_id,
        "wsess_00000000000000000000000000000001"
    );
    assert!(history.entries.iter().any(|entry| entry.title == "Updated"
        && entry.final_url.as_deref() == Some("https://example.com/next")));
    assert!(history
        .entries
        .iter()
        .any(|entry| entry.content_kind == ContentKind::Video && entry.final_url.is_none()));
}

#[test]
fn web_video_history_preserves_requested_and_rendered_urls_separately() {
    let dir = TempDir::new().unwrap();
    let service = DatabaseService::open(database_path(&dir)).unwrap();
    let mut window = web_window(
        "wsess_00000000000000000000000000000001",
        "https://youtu.be/M7lc1UVf-VE",
    );
    window.content_kind = ContentKind::Video;
    window.rendered_url =
        Some("https://www.youtube-nocookie.com/embed/M7lc1UVf-VE?autoplay=1".to_string());
    service.track_open(window.clone()).unwrap();
    service.close(window, CloseReason::Explicit).unwrap();

    let history = service.history(None).unwrap();
    assert_eq!(history.entries.len(), 1);
    let entry = &history.entries[0];
    assert_eq!(entry.requested_source, "https://youtu.be/M7lc1UVf-VE");
    assert_eq!(
        entry.final_url.as_deref(),
        Some("https://youtu.be/M7lc1UVf-VE")
    );
    assert_eq!(
        entry.rendered_url.as_deref(),
        Some("https://www.youtube-nocookie.com/embed/M7lc1UVf-VE?autoplay=1")
    );
}

#[test]
fn failed_abort_retains_session_for_retry() {
    let dir = TempDir::new().unwrap();
    let path = database_path(&dir);
    let service = DatabaseService::open(path.clone()).unwrap();
    service
        .track_open(web_window(
            "wsess_00000000000000000000000000000001",
            "https://example.com/",
        ))
        .unwrap();

    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fail_abort BEFORE DELETE ON sessions
             BEGIN SELECT RAISE(ABORT, 'forced abort failure'); END;",
    )
    .unwrap();
    assert!(service
        .abort_open("wsess_00000000000000000000000000000001")
        .is_err());
    conn.execute_batch("DROP TRIGGER fail_abort;").unwrap();
    service
        .abort_open("wsess_00000000000000000000000000000001")
        .unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn failed_close_retains_session_for_retry() {
    let dir = TempDir::new().unwrap();
    let path = database_path(&dir);
    let service = DatabaseService::open(path.clone()).unwrap();
    let window = web_window(
        "wsess_00000000000000000000000000000001",
        "https://example.com/",
    );
    service.track_open(window.clone()).unwrap();

    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER fail_close BEFORE UPDATE OF closed_at ON sessions
             WHEN NEW.closed_at IS NOT NULL
             BEGIN SELECT RAISE(ABORT, 'forced close failure'); END;",
    )
    .unwrap();
    assert!(service
        .close(window.clone(), CloseReason::Explicit)
        .is_err());
    conn.execute_batch("DROP TRIGGER fail_close;").unwrap();
    service.close(window, CloseReason::Explicit).unwrap();
    let history = service.history(None).unwrap();
    assert_eq!(history.entries.len(), 1);
    assert_eq!(history.entries[0].close_reason, CloseReason::Explicit);
}

#[test]
fn aborted_open_is_removed_instead_of_becoming_recovery_history() {
    let dir = TempDir::new().unwrap();
    let path = database_path(&dir);
    {
        let service = DatabaseService::open(path.clone()).unwrap();
        service
            .track_open(web_window(
                "wsess_00000000000000000000000000000001",
                "https://example.com/",
            ))
            .unwrap();
        service
            .abort_open("wsess_00000000000000000000000000000001")
            .unwrap();
    }
    let service = DatabaseService::open(path).unwrap();
    assert!(service.history(None).unwrap().entries.is_empty());
}

#[test]
fn startup_recovery_marks_unclosed_sessions_abrupt() {
    let dir = TempDir::new().unwrap();
    let path = database_path(&dir);
    {
        let service = DatabaseService::open(path.clone()).unwrap();
        service
            .track_open(web_window(
                "wsess_00000000000000000000000000000001",
                "https://example.com/",
            ))
            .unwrap();
        service.flush().unwrap();
    }
    let service = DatabaseService::open(path).unwrap();
    let history = service.history(None).unwrap();
    assert_eq!(history.entries.len(), 1);
    assert_eq!(
        history.entries[0].close_reason,
        CloseReason::AbruptTermination
    );
}

#[test]
fn history_toggle_is_transition_aware_and_preserves_closed_history() {
    let dir = TempDir::new().unwrap();
    let service = DatabaseService::open(database_path(&dir)).unwrap();
    let closed = web_window(
        "wsess_00000000000000000000000000000001",
        "https://closed.example/",
    );
    service.track_open(closed.clone()).unwrap();
    service.close(closed, CloseReason::Explicit).unwrap();

    let mut active = web_window(
        "wsess_00000000000000000000000000000002",
        "https://active.example/",
    );
    service.track_open(active.clone()).unwrap();
    let config = service
        .config(
            ConfigAction::SetHistoryEnabled { enabled: false },
            vec![active.clone()],
        )
        .unwrap();
    assert!(!config.history_enabled);
    assert_eq!(service.history(None).unwrap().entries.len(), 1);

    active.current_url = Some("https://active.example/later".to_string());
    active
        .url_history
        .as_mut()
        .unwrap()
        .push("https://active.example/later".to_string());
    service.sync(active.clone()).unwrap();
    let config = service
        .config(
            ConfigAction::SetHistoryEnabled { enabled: true },
            vec![active.clone()],
        )
        .unwrap();
    assert!(config.history_enabled);
    service.close(active, CloseReason::Explicit).unwrap();

    let history = service.history(Some("active.example".to_string())).unwrap();
    assert_eq!(history.entries.len(), 1);
    assert_eq!(
        history.entries[0].url_history,
        vec!["https://active.example/later".to_string()]
    );
}

#[test]
fn config_updates_are_idempotent_and_validation_is_bounded() {
    let dir = TempDir::new().unwrap();
    let service = DatabaseService::open(database_path(&dir)).unwrap();
    let first = service
        .config(ConfigAction::SetRetentionDays { days: Some(30) }, vec![])
        .unwrap();
    let second = service
        .config(ConfigAction::SetRetentionDays { days: Some(30) }, vec![])
        .unwrap();
    assert_eq!(first, second);
    assert!(service
        .config(
            ConfigAction::SetRetentionDays {
                days: Some(MAX_RETENTION_DAYS + 1)
            },
            vec![]
        )
        .is_err());
    assert!(service.history(Some(" ".to_string())).is_err());
}

#[test]
fn configuration_persists_across_worker_restart() {
    let dir = TempDir::new().unwrap();
    let path = database_path(&dir);
    {
        let service = DatabaseService::open(path.clone()).unwrap();
        service
            .config(ConfigAction::SetHistoryEnabled { enabled: false }, vec![])
            .unwrap();
        service
            .config(ConfigAction::SetRetentionDays { days: None }, vec![])
            .unwrap();
    }
    let service = DatabaseService::open(path).unwrap();
    assert_eq!(
        service.config(ConfigAction::Show, vec![]).unwrap(),
        Configuration {
            history_enabled: false,
            retention_days: None,
        }
    );
}

#[test]
fn clear_history_does_not_delete_active_tracking() {
    let dir = TempDir::new().unwrap();
    let service = DatabaseService::open(database_path(&dir)).unwrap();
    let closed = web_window(
        "wsess_00000000000000000000000000000001",
        "https://closed.example/",
    );
    service.track_open(closed.clone()).unwrap();
    service.close(closed, CloseReason::Explicit).unwrap();
    let active = web_window(
        "wsess_00000000000000000000000000000002",
        "https://active.example/",
    );
    service.track_open(active.clone()).unwrap();

    assert_eq!(service.clear_history().unwrap().deleted, 1);
    service.close(active, CloseReason::Explicit).unwrap();
    let history = service.history(None).unwrap();
    assert_eq!(history.entries.len(), 1);
    assert_eq!(
        history.entries[0].window_session_id,
        "wsess_00000000000000000000000000000002"
    );
}

#[test]
fn retention_pruning_removes_only_expired_closed_rows() {
    let dir = TempDir::new().unwrap();
    let path = database_path(&dir);
    let service = DatabaseService::open(path.clone()).unwrap();
    let old = web_window(
        "wsess_00000000000000000000000000000001",
        "https://old.example/",
    );
    service.track_open(old.clone()).unwrap();
    service.close(old, CloseReason::Explicit).unwrap();
    service.flush().unwrap();

    let conn = Connection::open(&path).unwrap();
    conn.execute(
            "UPDATE sessions SET closed_at = ?1 WHERE window_session_id = 'wsess_00000000000000000000000000000001'",
            params![1_000_i64],
        )
        .unwrap();
    drop(conn);

    let deleted = service
        .prune_at(1_000 + i64::from(DEFAULT_RETENTION_DAYS + 1) * 86_400)
        .unwrap();
    assert_eq!(deleted, 1);
    assert!(service.history(None).unwrap().entries.is_empty());
}
