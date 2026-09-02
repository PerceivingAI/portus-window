use super::*;

impl WindowManager {
    pub fn close(&self, target: &str) -> Result<Vec<String>, WindowManagerError> {
        let _lifecycle = self.lifecycle_lock.write();
        let window_session_id = self.resolve_window_session_id(target)?;
        self.close_by_window_session_id(&window_session_id)?;
        Ok(vec![window_session_id])
    }

    pub fn close_all(&self) -> Result<Vec<String>, WindowManagerError> {
        let _lifecycle = self.lifecycle_lock.write();
        let ids: Vec<String> = self
            .registry
            .lock()
            .list()
            .into_iter()
            .map(|window| window.window_session_id)
            .collect();
        let mut closed = Vec::with_capacity(ids.len());
        for window_session_id in ids {
            self.close_by_window_session_id(&window_session_id)?;
            closed.push(window_session_id);
        }
        Ok(closed)
    }

    fn close_by_window_session_id(
        &self,
        window_session_id: &str,
    ) -> Result<(), WindowManagerError> {
        self.prepare_authenticated_window_close(window_session_id)?;
        let window = self
            .app
            .get_webview_window(window_session_id)
            .ok_or_else(|| missing_handle(window_session_id))?;
        self.pending_close_reasons
            .lock()
            .insert(window_session_id.to_string(), CloseReason::Explicit);
        if self.registry.lock().is_last_web_window(window_session_id) {
            if let Err(error) = prune_web_cache(&window) {
                eprintln!("Portus Window cache prune failed during explicit close: {error}");
            }
        }
        if let Err(error) = window.destroy() {
            self.pending_close_reasons.lock().remove(window_session_id);
            return Err(WindowManagerError::Operation(error.to_string()));
        }
        if let Some(removed) = self.registry.lock().remove(window_session_id) {
            let reason = self
                .pending_close_reasons
                .lock()
                .remove(window_session_id)
                .unwrap_or(CloseReason::Explicit);
            self.persistence.close_async(removed, reason);
        }
        if let Some(workspace) = &self.workspace {
            workspace.unwatch_window(window_session_id);
        }
        self.media.revoke_window(window_session_id);
        self.web_video.revoke_window(window_session_id);
        // Close persistence is already queued in FIFO order. Do not force every user close
        // to wait for SQLite; DatabaseService drains queued work during orderly shutdown.
        Ok(())
    }
}
