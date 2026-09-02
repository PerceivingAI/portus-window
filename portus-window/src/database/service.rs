use super::*;

impl DatabaseService {
    pub fn open_default() -> Result<Self, PersistenceError> {
        Self::open(default_database_path()?)
    }

    pub(crate) fn open(path: PathBuf) -> Result<Self, PersistenceError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                PersistenceError::Storage(format!(
                    "could not create persistence directory '{}': {error}",
                    parent.display()
                ))
            })?;
        }
        validate_persistence_directory(&path)?;
        secure_persistence_directory(&path)?;

        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("portus-window-database".to_string())
            .spawn(move || match DbWorker::initialize(&path) {
                Ok(mut worker) => {
                    let _ = ready_tx.send(Ok(()));
                    worker.run(rx);
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                }
            })
            .map_err(|error| PersistenceError::Storage(error.to_string()))?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                inner: Arc::new(DatabaseInner {
                    tx,
                    worker: Mutex::new(Some(worker)),
                }),
            }),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                let _ = worker.join();
                Err(PersistenceError::WorkerUnavailable)
            }
        }
    }

    pub fn track_open(&self, window: ActiveWindow) -> Result<(), PersistenceError> {
        self.track_open_with_mode(window, false)
    }

    fn track_open_with_mode(
        &self,
        window: ActiveWindow,
        reset_timeline: bool,
    ) -> Result<(), PersistenceError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.send(DbCommand::Open {
            window,
            reset_timeline,
            reply: reply_tx,
        })?;
        recv(reply_rx)
    }

    pub fn abort_open(&self, window_session_id: &str) -> Result<(), PersistenceError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.send(DbCommand::AbortOpen {
            window_session_id: window_session_id.to_string(),
            reply: reply_tx,
        })?;
        recv(reply_rx)
    }

    pub fn sync(&self, window: ActiveWindow) -> Result<(), PersistenceError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.send(DbCommand::Sync {
            window,
            reply: Some(reply_tx),
        })?;
        recv(reply_rx)
    }

    pub fn sync_async(&self, window: ActiveWindow) {
        if let Err(error) = self.send(DbCommand::Sync {
            window,
            reply: None,
        }) {
            eprintln!("Portus Window could not enqueue history sync: {error}");
        }
    }

    pub fn close(&self, window: ActiveWindow, reason: CloseReason) -> Result<(), PersistenceError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.send(DbCommand::Close {
            window,
            reason,
            reply: Some(reply_tx),
        })?;
        recv(reply_rx)
    }

    pub fn close_async(&self, window: ActiveWindow, reason: CloseReason) {
        if let Err(error) = self.send(DbCommand::Close {
            window,
            reason,
            reply: None,
        }) {
            eprintln!("Portus Window could not enqueue history close: {error}");
        }
    }

    pub fn history(&self, query: Option<String>) -> Result<HistoryResult, PersistenceError> {
        let query = validate_history_query(query)?;
        let (reply_tx, reply_rx) = mpsc::channel();
        self.send(DbCommand::History {
            query,
            reply: reply_tx,
        })?;
        recv(reply_rx)
    }

    pub fn clear_history(&self) -> Result<ClearHistoryResult, PersistenceError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.send(DbCommand::ClearHistory { reply: reply_tx })?;
        recv(reply_rx)
    }

    pub fn config(
        &self,
        action: ConfigAction,
        active_windows: Vec<ActiveWindow>,
    ) -> Result<Configuration, PersistenceError> {
        validate_config_action(&action)?;
        let (reply_tx, reply_rx) = mpsc::channel();
        self.send(DbCommand::Config {
            action,
            active_windows,
            reply: reply_tx,
        })?;
        recv(reply_rx)
    }

    pub fn flush(&self) -> Result<(), PersistenceError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.send(DbCommand::Flush { reply: reply_tx })?;
        recv(reply_rx)
    }

    #[cfg(test)]
    pub(super) fn prune_at(&self, now: i64) -> Result<u64, PersistenceError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.send(DbCommand::PruneAt {
            now,
            reply: reply_tx,
        })?;
        recv(reply_rx)
    }

    fn send(&self, command: DbCommand) -> Result<(), PersistenceError> {
        self.inner
            .tx
            .send(command)
            .map_err(|_| PersistenceError::WorkerUnavailable)
    }
}

impl DbWorker {
    fn initialize(path: &Path) -> Result<Self, PersistenceError> {
        reject_symlink_path(path, "database file")?;
        let mut conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        conn.busy_timeout(Duration::from_secs(5))?;
        let schema_state = inspect_database_schema(&conn)?;
        secure_database_file(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        initialize_schema(&mut conn, schema_state)?;
        initialize_config(&conn)?;
        let config = load_config(&conn)?;
        let now = now_unix();
        conn.execute(
            "UPDATE sessions SET closed_at = ?1, close_reason = 'abrupt_termination' WHERE closed_at IS NULL",
            params![now],
        )?;
        prune_closed(&conn, &config, now)?;

        Ok(Self {
            conn,
            config,
            active_sessions: HashMap::new(),
        })
    }

    fn run(&mut self, rx: mpsc::Receiver<DbCommand>) {
        while let Ok(command) = rx.recv() {
            match command {
                DbCommand::Open {
                    window,
                    reset_timeline,
                    reply,
                } => {
                    let _ = reply.send(self.open_session(&window, reset_timeline));
                }
                DbCommand::AbortOpen {
                    window_session_id,
                    reply,
                } => {
                    let _ = reply.send(self.abort_open(&window_session_id));
                }
                DbCommand::Sync { window, reply } => {
                    let result = self.sync_window(&window);
                    if let Some(reply) = reply {
                        let _ = reply.send(result);
                    } else if let Err(error) = result {
                        eprintln!("Portus Window history sync failed: {error}");
                    }
                }
                DbCommand::Close {
                    window,
                    reason,
                    reply,
                } => {
                    let result = self.close_session(&window, reason);
                    if let Some(reply) = reply {
                        let _ = reply.send(result);
                    } else if let Err(error) = result {
                        eprintln!("Portus Window history close sync failed: {error}");
                    }
                }
                DbCommand::History { query, reply } => {
                    let _ = reply.send(self.history(query.as_deref()));
                }
                DbCommand::ClearHistory { reply } => {
                    let _ = reply.send(self.clear_history());
                }
                DbCommand::Config {
                    action,
                    active_windows,
                    reply,
                } => {
                    let _ = reply.send(self.apply_config(action, &active_windows));
                }
                #[cfg(test)]
                DbCommand::PruneAt { now, reply } => {
                    let _ = reply.send(prune_closed(&self.conn, &self.config, now));
                }
                DbCommand::Flush { reply } => {
                    let _ = reply.send(Ok(()));
                }
                DbCommand::Shutdown => break,
            }
        }
    }

    fn open_session(
        &mut self,
        window: &ActiveWindow,
        reset_timeline: bool,
    ) -> Result<(), PersistenceError> {
        if !self.config.history_enabled {
            return Ok(());
        }
        if self.active_sessions.contains_key(&window.window_session_id) {
            return Err(PersistenceError::Storage(format!(
                "window '{}' already has an active persisted session",
                window.window_session_id
            )));
        }
        let session_id = Uuid::new_v4().to_string();
        insert_session(&self.conn, &session_id, window, now_unix(), reset_timeline)?;
        self.active_sessions.insert(
            window.window_session_id.clone(),
            ActiveSession::new(session_id, window, reset_timeline),
        );
        Ok(())
    }

    fn abort_open(&mut self, window_session_id: &str) -> Result<(), PersistenceError> {
        let Some(session) = self.active_sessions.get(window_session_id).cloned() else {
            return Ok(());
        };
        self.conn.execute(
            "DELETE FROM sessions WHERE session_id = ?1 AND closed_at IS NULL",
            params![session.session_id],
        )?;
        self.active_sessions.remove(window_session_id);
        Ok(())
    }

    fn sync_window(&mut self, window: &ActiveWindow) -> Result<(), PersistenceError> {
        if !self.config.history_enabled {
            return Ok(());
        }
        let Some(session) = self.active_sessions.get_mut(&window.window_session_id) else {
            return Ok(());
        };
        session.observe(window);
        update_session(&self.conn, session, window)?;
        Ok(())
    }

    fn close_session(
        &mut self,
        window: &ActiveWindow,
        reason: CloseReason,
    ) -> Result<(), PersistenceError> {
        if !self.config.history_enabled {
            return Ok(());
        }
        let Some(mut session) = self.active_sessions.get(&window.window_session_id).cloned() else {
            return Ok(());
        };
        session.observe(window);
        let now = now_unix();
        let tx = self.conn.transaction()?;
        close_session_row(&tx, &session, window, reason, now)?;
        prune_closed(&tx, &self.config, now)?;
        tx.commit()?;
        self.active_sessions.remove(&window.window_session_id);
        Ok(())
    }

    fn history(&self, query: Option<&str>) -> Result<HistoryResult, PersistenceError> {
        let query = query.map(str::to_lowercase);
        let mut stmt = self.conn.prepare(
            "SELECT session_id, window_session_id, source_kind, content_kind, requested_source, title, \
             rendered_url, final_url, url_history_json, url_history_truncated, description, workspace, \
             is_on_all_workspaces, workspace_history_json, workspace_history_truncated, \
             opened_at, closed_at, close_reason \
             FROM sessions WHERE closed_at IS NOT NULL ORDER BY closed_at DESC, session_id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![(MAX_HISTORY_SCAN_ROWS + 1) as i64], |row| {
            Ok(RawHistoricalWindow {
                historical_id: row.get(0)?,
                window_session_id: row.get(1)?,
                source_kind: row.get(2)?,
                content_kind: row.get(3)?,
                requested_source: row.get(4)?,
                title: row.get(5)?,
                rendered_url: row.get(6)?,
                final_url: row.get(7)?,
                url_history_json: row.get(8)?,
                url_history_truncated: row.get::<_, i64>(9)? != 0,
                description: row.get(10)?,
                workspace: row.get(11)?,
                is_on_all_workspaces: row.get::<_, i64>(12)? != 0,
                workspace_history_json: row.get(13)?,
                workspace_history_truncated: row.get::<_, i64>(14)? != 0,
                opened_at: row.get(15)?,
                closed_at: row.get(16)?,
                close_reason: row.get(17)?,
            })
        })?;

        let mut entries = Vec::new();
        let mut bytes = 0usize;
        let mut scanned = 0usize;
        let mut truncated = false;
        for raw in rows {
            scanned += 1;
            if scanned > MAX_HISTORY_SCAN_ROWS {
                truncated = true;
                break;
            }
            let entry = raw_to_historical(raw?)?;
            if let Some(query) = &query {
                if !history_matches(&entry, query) {
                    continue;
                }
            }
            let entry_bytes = serde_json::to_vec(&entry)
                .map_err(|error| PersistenceError::Storage(error.to_string()))?
                .len();
            if bytes.saturating_add(entry_bytes) > MAX_HISTORY_RESULT_BYTES {
                truncated = true;
                break;
            }
            bytes += entry_bytes;
            entries.push(entry);
        }
        Ok(HistoryResult { entries, truncated })
    }

    fn clear_history(&mut self) -> Result<ClearHistoryResult, PersistenceError> {
        let deleted = self
            .conn
            .execute("DELETE FROM sessions WHERE closed_at IS NOT NULL", [])?;
        Ok(ClearHistoryResult {
            deleted: deleted as u64,
        })
    }

    fn apply_config(
        &mut self,
        action: ConfigAction,
        active_windows: &[ActiveWindow],
    ) -> Result<Configuration, PersistenceError> {
        validate_config_action(&action)?;
        match action {
            ConfigAction::Show => {}
            ConfigAction::SetHistoryEnabled { enabled } => {
                if enabled != self.config.history_enabled {
                    if enabled {
                        self.enable_history(active_windows)?;
                    } else {
                        let tx = self.conn.transaction()?;
                        tx.execute("DELETE FROM sessions WHERE closed_at IS NULL", [])?;
                        set_config_value(&tx, "history_enabled", "false")?;
                        tx.commit()?;
                        self.active_sessions.clear();
                        self.config.history_enabled = false;
                    }
                }
            }
            ConfigAction::SetRetentionDays { days } => {
                if days != self.config.retention_days {
                    let value = days
                        .map(|days| days.to_string())
                        .unwrap_or_else(|| "null".to_string());
                    let new_config = Configuration {
                        history_enabled: self.config.history_enabled,
                        retention_days: days,
                    };
                    let tx = self.conn.transaction()?;
                    set_config_value(&tx, "retention_days", &value)?;
                    prune_closed(&tx, &new_config, now_unix())?;
                    tx.commit()?;
                    self.config = new_config;
                }
            }
        }
        Ok(self.config.clone())
    }

    fn enable_history(&mut self, active_windows: &[ActiveWindow]) -> Result<(), PersistenceError> {
        let now = now_unix();
        let tx = self.conn.transaction()?;
        set_config_value(&tx, "history_enabled", "true")?;
        let mut new_sessions = HashMap::new();
        for window in active_windows {
            let session_id = Uuid::new_v4().to_string();
            insert_session_tx(&tx, &session_id, window, now, true)?;
            new_sessions.insert(
                window.window_session_id.clone(),
                ActiveSession::new(session_id, window, true),
            );
        }
        tx.commit()?;
        self.active_sessions = new_sessions;
        self.config.history_enabled = true;
        Ok(())
    }
}
