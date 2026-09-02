use portus_window_protocol::{
    ActiveWindow, ConsoleEntry, ContentKind, LoadState, MediaAction, MediaState, SourceKind,
    WindowStateAction, WorkspaceTarget, MAX_CONSOLE_ENTRIES, MAX_CONSOLE_MESSAGE_CHARS,
    MAX_CONSOLE_SOURCE_CHARS, MAX_DESCRIPTION_CHARS, MAX_LOAD_ERROR_CHARS, MAX_TITLE_CHARS,
    MAX_URL_CHARS, MAX_URL_HISTORY_BYTES, MAX_URL_HISTORY_ENTRIES,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use thiserror::Error;
use url::Url;
use uuid::Uuid;

pub const DEFAULT_WIDTH: u32 = 1024;
pub const DEFAULT_HEIGHT: u32 = 768;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum WindowCoreError {
    #[error("validation error: {0}")]
    Validation(String),
    #[error("target not found: {0}")]
    TargetNotFound(String),
    #[error("target ambiguous: {0}")]
    TargetAmbiguous(String),
    #[error("window capacity exhausted")]
    CapacityExhausted,
    #[error("window state error: {0}")]
    State(String),
    #[error("window operation failed: {0}")]
    Operation(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScreenshotSpec {
    pub out: PathBuf,
    pub overwrite: bool,
}

impl ScreenshotSpec {
    pub fn from_request(out: &str, overwrite: bool) -> Result<Self, WindowCoreError> {
        let trimmed = out.trim();
        if trimmed.is_empty() {
            return Err(WindowCoreError::Validation(
                "screenshot output path must not be empty".to_string(),
            ));
        }
        let path = PathBuf::from(trimmed);
        if !path.is_absolute() {
            return Err(WindowCoreError::Validation(
                "screenshot output path must be absolute".to_string(),
            ));
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("png") {
            return Err(WindowCoreError::Validation(
                "screenshot output path must have .png extension".to_string(),
            ));
        }
        Ok(Self {
            out: path,
            overwrite,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResizeSpec {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub state: Option<WindowStateAction>,
    pub always_on_top: Option<bool>,
    pub workspace: Option<WorkspaceTarget>,
}

impl ResizeSpec {
    pub fn validate(&self) -> Result<(), WindowCoreError> {
        if let Some(width) = self.width {
            if width == 0 || width > 16_384 {
                return Err(WindowCoreError::Validation(
                    "width must be between 1 and 16384".to_string(),
                ));
            }
        }
        if let Some(height) = self.height {
            if height == 0 || height > 16_384 {
                return Err(WindowCoreError::Validation(
                    "height must be between 1 and 16384".to_string(),
                ));
            }
        }
        Ok(())
    }
}

pub fn validate_media_action(action: &MediaAction) -> Result<(), WindowCoreError> {
    match action {
        MediaAction::Play | MediaAction::Pause => Ok(()),
        MediaAction::Seek { seconds } => {
            if !seconds.is_finite() || *seconds < 0.0 {
                return Err(WindowCoreError::Validation(
                    "seek seconds must be a non-negative finite number".to_string(),
                ));
            }
            Ok(())
        }
        MediaAction::SetVolume { level } => {
            if !level.is_finite() || !(0.0..=1.0).contains(level) {
                return Err(WindowCoreError::Validation(
                    "volume level must be between 0.0 and 1.0".to_string(),
                ));
            }
            Ok(())
        }
    }
}

pub fn sanitize_console_result(mut entries: Vec<ConsoleEntry>) -> (Vec<ConsoleEntry>, bool) {
    let truncated = entries.len() > MAX_CONSOLE_ENTRIES;
    if truncated {
        entries.truncate(MAX_CONSOLE_ENTRIES);
    }
    for entry in &mut entries {
        if entry.message.chars().count() > MAX_CONSOLE_MESSAGE_CHARS {
            entry.message = entry
                .message
                .chars()
                .take(MAX_CONSOLE_MESSAGE_CHARS)
                .collect();
        }
        if let Some(source) = &mut entry.source {
            if source.chars().count() > MAX_CONSOLE_SOURCE_CHARS {
                *source = source.chars().take(MAX_CONSOLE_SOURCE_CHARS).collect();
            }
        }
    }
    (entries, truncated)
}

#[derive(Clone, Debug)]
pub struct WindowRecord {
    pub window: ActiveWindow,
    pub focus_order: u64,
}

#[derive(Clone, Debug, Default)]
pub struct WindowRegistry {
    windows: BTreeMap<String, WindowRecord>,
    focus_counter: u64,
}

impl WindowRegistry {
    pub fn next_id(&self) -> Result<String, WindowCoreError> {
        loop {
            let candidate = format!("wsess_{}", Uuid::new_v4().simple());
            if !self.windows.contains_key(&candidate) {
                return Ok(candidate);
            }
        }
    }

    pub fn insert_opened_web(
        &mut self,
        window_session_id: String,
        requested_url: String,
        description: Option<String>,
        profile: Option<String>,
    ) -> ActiveWindow {
        self.insert(ActiveWindow {
            window_session_id,
            source_kind: SourceKind::Web,
            content_kind: ContentKind::Web,
            requested_source: requested_url.clone(),
            current_url: Some(requested_url.clone()),
            rendered_url: None,
            url_history: Some(vec![requested_url]),
            url_history_truncated: Some(false),
            title: "Portus Window".to_string(),
            description,
            profile,
            authenticated: None,
            load_state: LoadState::Started,
            load_error: None,
            console_errors: Vec::new(),
            console_errors_truncated: false,
            media_state: None,
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            x: 0,
            y: 0,
            is_maximized: false,
            is_minimized: false,
            is_focused: false,
            is_always_on_top: false,
            workspace: None,
            is_on_all_workspaces: false,
            workspace_history: Vec::new(),
            workspace_history_truncated: false,
        })
    }

    pub fn insert_opened_web_video(
        &mut self,
        window_session_id: String,
        requested_url: String,
        rendered_url: String,
        description: Option<String>,
        profile: Option<String>,
    ) -> ActiveWindow {
        self.insert(ActiveWindow {
            window_session_id,
            source_kind: SourceKind::Web,
            content_kind: ContentKind::Video,
            requested_source: requested_url.clone(),
            current_url: Some(requested_url),
            rendered_url: Some(rendered_url),
            url_history: None,
            url_history_truncated: None,
            title: "YouTube Video".to_string(),
            description,
            profile,
            authenticated: None,
            load_state: LoadState::Started,
            load_error: None,
            console_errors: Vec::new(),
            console_errors_truncated: false,
            media_state: None,
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            x: 0,
            y: 0,
            is_maximized: false,
            is_minimized: false,
            is_focused: false,
            is_always_on_top: false,
            workspace: None,
            is_on_all_workspaces: false,
            workspace_history: Vec::new(),
            workspace_history_truncated: false,
        })
    }

    pub fn insert_opened_media(
        &mut self,
        window_session_id: String,
        requested_source: String,
        content_kind: ContentKind,
        title: String,
        description: Option<String>,
        profile: Option<String>,
    ) -> Result<ActiveWindow, WindowCoreError> {
        let media_state = matches!(content_kind, ContentKind::Video | ContentKind::Audio)
            .then(MediaState::default);
        Ok(self.insert(ActiveWindow {
            window_session_id,
            source_kind: SourceKind::LocalMedia,
            content_kind,
            requested_source,
            current_url: None,
            rendered_url: None,
            url_history: None,
            url_history_truncated: None,
            title,
            description,
            profile,
            authenticated: None,
            load_state: LoadState::Loaded,
            load_error: None,
            console_errors: Vec::new(),
            console_errors_truncated: false,
            media_state,
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            x: 0,
            y: 0,
            is_maximized: false,
            is_minimized: false,
            is_focused: false,
            is_always_on_top: false,
            workspace: None,
            is_on_all_workspaces: false,
            workspace_history: Vec::new(),
            workspace_history_truncated: false,
        }))
    }

    fn insert(&mut self, window: ActiveWindow) -> ActiveWindow {
        let window_session_id = window.window_session_id.clone();
        self.windows.insert(
            window_session_id,
            WindowRecord {
                window: window.clone(),
                focus_order: 0,
            },
        );
        window
    }

    pub fn status(&self, window_session_id: &str) -> Result<ActiveWindow, WindowCoreError> {
        self.windows
            .get(window_session_id)
            .map(|record| record.window.clone())
            .ok_or_else(|| WindowCoreError::TargetNotFound(window_session_id.to_string()))
    }

    pub fn list(&self) -> Vec<ActiveWindow> {
        self.windows
            .values()
            .map(|record| record.window.clone())
            .collect()
    }

    pub fn resolve_target(&self, target: &str) -> Result<String, WindowCoreError> {
        if self.windows.contains_key(target) {
            return Ok(target.to_string());
        }
        if target.starts_with("wsess_") {
            return Err(WindowCoreError::TargetNotFound(target.to_string()));
        }
        let target_lower = target.to_lowercase();
        let mut matches: Vec<(&String, &WindowRecord)> = match target_lower.as_str() {
            "focused" => self
                .windows
                .iter()
                .filter(|(_, record)| record.window.is_focused)
                .collect(),
            "maximized" => self
                .windows
                .iter()
                .filter(|(_, record)| record.window.is_maximized)
                .collect(),
            _ => self
                .windows
                .iter()
                .filter(|(_, record)| {
                    record
                        .window
                        .description
                        .as_deref()
                        .map(str::to_lowercase)
                        .is_some_and(|d| d.contains(&target_lower))
                        || record.window.title.to_lowercase().contains(&target_lower)
                        || record
                            .window
                            .requested_source
                            .to_lowercase()
                            .contains(&target_lower)
                        || record
                            .window
                            .current_url
                            .as_deref()
                            .map(str::to_lowercase)
                            .is_some_and(|u| u.contains(&target_lower))
                })
                .collect(),
        };

        if matches.is_empty() {
            return Err(WindowCoreError::TargetNotFound(target.to_string()));
        }
        if matches.len() == 1 {
            return Ok(matches[0].0.clone());
        }
        matches.sort_by_key(|(_, record)| std::cmp::Reverse(record.focus_order));
        let newest = matches[0].1;
        if matches.len() > 1
            && matches[1].1.focus_order == newest.focus_order
            && matches[1].1.window.window_session_id != newest.window.window_session_id
        {
            return Err(WindowCoreError::TargetAmbiguous(target.to_string()));
        }
        Ok(newest.window.window_session_id.clone())
    }

    pub fn resolve_workspace_index(&self, index: u32) -> Result<String, WindowCoreError> {
        let mut matches: Vec<(&String, &WindowRecord)> = self
            .windows
            .iter()
            .filter(|(_, record)| {
                record.window.workspace == Some(index) || record.window.is_on_all_workspaces
            })
            .collect();
        if matches.is_empty() {
            return Err(WindowCoreError::TargetNotFound(format!(
                "workspace:{index}"
            )));
        }
        if matches.len() == 1 {
            return Ok(matches[0].0.clone());
        }
        matches.sort_by_key(|(_, record)| std::cmp::Reverse(record.focus_order));
        let newest = matches[0].1;
        if matches.len() > 1
            && matches[1].1.focus_order == newest.focus_order
            && matches[1].1.window.window_session_id != newest.window.window_session_id
        {
            return Err(WindowCoreError::TargetAmbiguous(format!(
                "workspace:{index}"
            )));
        }
        Ok(newest.window.window_session_id.clone())
    }

    pub fn record_workspace(
        &mut self,
        window_session_id: &str,
        index: Option<u32>,
        all: bool,
    ) -> Result<ActiveWindow, WindowCoreError> {
        if let Some(index) = index {
            if index > 255 {
                return Err(WindowCoreError::Validation(
                    "workspace index must be between 0 and 255".to_string(),
                ));
            }
        }
        let record = self
            .windows
            .get_mut(window_session_id)
            .ok_or_else(|| WindowCoreError::TargetNotFound(window_session_id.to_string()))?;
        record.window.workspace = if all { None } else { index };
        record.window.is_on_all_workspaces = all;
        if let Some(index) = record.window.workspace {
            if record.window.workspace_history.last() != Some(&index) {
                if record.window.workspace_history.len() >= 128 {
                    record.window.workspace_history.remove(0);
                    record.window.workspace_history_truncated = true;
                }
                record.window.workspace_history.push(index);
            }
        }
        Ok(record.window.clone())
    }

    pub fn remove(&mut self, window_session_id: &str) -> Option<ActiveWindow> {
        self.windows
            .remove(window_session_id)
            .map(|record| record.window)
    }

    pub fn is_last_web_window(&self, window_session_id: &str) -> bool {
        self.windows
            .get(window_session_id)
            .is_some_and(|record| record.window.source_kind == SourceKind::Web)
            && self
                .windows
                .values()
                .filter(|record| record.window.source_kind == SourceKind::Web)
                .count()
                == 1
    }

    pub fn record_focus(&mut self, window_session_id: &str, focused: bool) {
        if !self.windows.contains_key(window_session_id) {
            return;
        }
        if focused {
            self.focus_counter += 1;
            for record in self.windows.values_mut() {
                record.window.is_focused = false;
            }
            let record = self
                .windows
                .get_mut(window_session_id)
                .expect("window existence was checked");
            record.window.is_focused = true;
            record.focus_order = self.focus_counter;
        } else if let Some(record) = self.windows.get_mut(window_session_id) {
            record.window.is_focused = false;
        }
    }

    pub fn record_observed_url(&mut self, window_session_id: &str, url: String) {
        if let Some(record) = self.windows.get_mut(window_session_id) {
            if record.window.source_kind == SourceKind::Web
                && record.window.content_kind == ContentKind::Web
            {
                record.window.current_url = Some(truncate_chars(url, MAX_URL_CHARS));
            }
        }
    }

    pub fn record_page_started(&mut self, window_session_id: &str, url: String) {
        if let Some(record) = self.windows.get_mut(window_session_id) {
            let same_failed_url = record.window.load_state == LoadState::Failed
                && record.window.current_url.as_deref() == Some(url.as_str());
            if record.window.source_kind == SourceKind::Web
                && record.window.content_kind == ContentKind::Web
                && !same_failed_url
            {
                record.window.load_state = LoadState::Started;
                record.window.load_error = None;
                record.window.current_url = Some(truncate_chars(url, MAX_URL_CHARS));
            }
        }
    }

    pub fn record_page_finished(&mut self, window_session_id: &str, url: String) {
        if let Some(record) = self.windows.get_mut(window_session_id) {
            if record.window.source_kind == SourceKind::Web
                && record.window.content_kind == ContentKind::Web
                && record.window.load_state != LoadState::Failed
                && record.window.load_error.is_none()
            {
                record.window.load_state = LoadState::Loaded;
                record.window.load_error = None;
                let url = truncate_chars(url, MAX_URL_CHARS);
                record.window.current_url = Some(url.clone());
                let history = record.window.url_history.get_or_insert_with(Vec::new);
                if history.last() != Some(&url) {
                    history.push(url);
                    trim_url_history(history, &mut record.window.url_history_truncated);
                }
            }
        }
    }

    pub fn record_page_failed(&mut self, window_session_id: &str, url: String, error: String) {
        if let Some(record) = self.windows.get_mut(window_session_id) {
            if record.window.source_kind == SourceKind::Web {
                record.window.load_state = LoadState::Failed;
                record.window.load_error = Some(truncate_chars(error, MAX_LOAD_ERROR_CHARS));
                record.window.current_url = Some(truncate_chars(url, MAX_URL_CHARS));
            }
        }
    }

    pub fn record_title(&mut self, window_session_id: &str, title: String) {
        if let Some(record) = self.windows.get_mut(window_session_id) {
            if record.window.source_kind == SourceKind::Web {
                record.window.title = truncate_chars(title, MAX_TITLE_CHARS);
            }
        }
    }

    pub fn record_authenticated(&mut self, window_session_id: &str, authenticated: Option<bool>) {
        if let Some(record) = self.windows.get_mut(window_session_id) {
            if record.window.source_kind == SourceKind::Web {
                record.window.authenticated = authenticated;
            }
        }
    }

    pub fn record_console_entries(
        &mut self,
        window_session_id: &str,
        entries: &[ConsoleEntry],
        source_truncated: bool,
    ) {
        if let Some(record) = self.windows.get_mut(window_session_id) {
            if record.window.source_kind != SourceKind::Web {
                return;
            }
            let mut current = std::mem::take(&mut record.window.console_errors);
            for entry in entries {
                current.push(entry.clone());
            }
            if current.len() > MAX_CONSOLE_ENTRIES {
                let overflow = current.len() - MAX_CONSOLE_ENTRIES;
                current.drain(0..overflow);
                record.window.console_errors_truncated = true;
            } else {
                record.window.console_errors_truncated = source_truncated;
            }
            record.window.console_errors = current;
        }
    }

    pub fn record_media_state(
        &mut self,
        window_session_id: &str,
        state: MediaState,
    ) -> Result<ActiveWindow, WindowCoreError> {
        let record = self
            .windows
            .get_mut(window_session_id)
            .ok_or_else(|| WindowCoreError::TargetNotFound(window_session_id.to_string()))?;
        if record.window.source_kind != SourceKind::LocalMedia
            || !matches!(
                record.window.content_kind,
                ContentKind::Audio | ContentKind::Video
            )
        {
            return Err(WindowCoreError::Validation(
                "media state can only be recorded for audio/video windows".to_string(),
            ));
        }
        record.window.media_state = Some(state);
        Ok(record.window.clone())
    }

    pub fn tag(
        &mut self,
        window_session_id: &str,
        description: String,
    ) -> Result<ActiveWindow, WindowCoreError> {
        let validated = validate_description(Some(description))?;
        let record = self
            .windows
            .get_mut(window_session_id)
            .ok_or_else(|| WindowCoreError::TargetNotFound(window_session_id.to_string()))?;
        record.window.description = validated;
        Ok(record.window.clone())
    }

    pub fn set_description(
        &mut self,
        window_session_id: &str,
        description: Option<String>,
    ) -> Result<ActiveWindow, WindowCoreError> {
        let record = self
            .windows
            .get_mut(window_session_id)
            .ok_or_else(|| WindowCoreError::TargetNotFound(window_session_id.to_string()))?;
        record.window.description = description;
        Ok(record.window.clone())
    }

    pub fn set_maximized(
        &mut self,
        window_session_id: &str,
        value: bool,
    ) -> Result<ActiveWindow, WindowCoreError> {
        let record = self
            .windows
            .get_mut(window_session_id)
            .ok_or_else(|| WindowCoreError::TargetNotFound(window_session_id.to_string()))?;
        record.window.is_maximized = value;
        Ok(record.window.clone())
    }

    pub fn set_minimized(
        &mut self,
        window_session_id: &str,
        value: bool,
    ) -> Result<ActiveWindow, WindowCoreError> {
        let record = self
            .windows
            .get_mut(window_session_id)
            .ok_or_else(|| WindowCoreError::TargetNotFound(window_session_id.to_string()))?;
        record.window.is_minimized = value;
        Ok(record.window.clone())
    }

    pub fn apply_resize(
        &mut self,
        window_session_id: &str,
        spec: &ResizeSpec,
    ) -> Result<ActiveWindow, WindowCoreError> {
        spec.validate()?;
        let record = self
            .windows
            .get_mut(window_session_id)
            .ok_or_else(|| WindowCoreError::TargetNotFound(window_session_id.to_string()))?;
        if let Some(width) = spec.width {
            record.window.width = width;
        }
        if let Some(height) = spec.height {
            record.window.height = height;
        }
        if let Some(x) = spec.x {
            record.window.x = x;
        }
        if let Some(y) = spec.y {
            record.window.y = y;
        }
        if let Some(state) = &spec.state {
            match state {
                WindowStateAction::Maximize => {
                    record.window.is_maximized = true;
                    record.window.is_minimized = false;
                }
                WindowStateAction::Minimize => {
                    record.window.is_maximized = false;
                    record.window.is_minimized = true;
                }
                WindowStateAction::Restore | WindowStateAction::Fullscreen => {
                    record.window.is_maximized = false;
                    record.window.is_minimized = false;
                }
            }
        }
        if let Some(always_on_top) = spec.always_on_top {
            record.window.is_always_on_top = always_on_top;
        }
        Ok(record.window.clone())
    }
}

pub fn validate_description(
    description: Option<String>,
) -> Result<Option<String>, WindowCoreError> {
    let Some(description) = description else {
        return Ok(None);
    };
    let trimmed = description.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(WindowCoreError::Validation(format!(
            "description must be at most {MAX_DESCRIPTION_CHARS} characters"
        )));
    }
    Ok(Some(trimmed.to_string()))
}

pub fn validate_url(url: &str) -> Result<Url, WindowCoreError> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(WindowCoreError::Validation(
            "URL must not be empty".to_string(),
        ));
    }
    if trimmed.len() > MAX_URL_CHARS {
        return Err(WindowCoreError::Validation(format!(
            "URL must be at most {MAX_URL_CHARS} characters"
        )));
    }
    let parsed = Url::parse(trimmed)
        .map_err(|error| WindowCoreError::Validation(format!("invalid URL: {error}")))?;
    match parsed.scheme() {
        "http" | "https" => Ok(parsed),
        "file" => Err(WindowCoreError::Validation(
            "file:// URLs are not permitted; pass the local media path directly".to_string(),
        )),
        _ => Err(WindowCoreError::Validation(
            "only http and https URLs are permitted".to_string(),
        )),
    }
}

pub fn validate_navigation_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
}

fn truncate_chars(value: String, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value
    } else {
        value.chars().take(max_chars).collect()
    }
}

fn trim_url_history(history: &mut Vec<String>, truncated: &mut Option<bool>) {
    while history.len() > MAX_URL_HISTORY_ENTRIES
        || history.iter().map(String::len).sum::<usize>() > MAX_URL_HISTORY_BYTES
    {
        if history.is_empty() {
            break;
        }
        history.remove(0);
        *truncated = Some(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry_with_three() -> (WindowRegistry, String, String, String) {
        let mut registry = WindowRegistry::default();
        let first = registry.next_id().unwrap();
        registry.insert_opened_web(
            first.clone(),
            "https://example.com/one".to_string(),
            None,
            None,
        );
        let second = registry.next_id().unwrap();
        registry.insert_opened_web(
            second.clone(),
            "https://example.com/two".to_string(),
            None,
            None,
        );
        let third = registry.next_id().unwrap();
        registry.insert_opened_web(
            third.clone(),
            "https://example.com/three".to_string(),
            None,
            None,
        );
        (registry, first, second, third)
    }

    #[test]
    fn allocator_generates_unique_non_recyclable_session_ids() {
        let (mut registry, ..) = registry_with_three();
        let first = registry.next_id().unwrap();
        assert!(first.starts_with("wsess_"));
        assert_eq!(first.len(), "wsess_".len() + 32);
        assert!(first["wsess_".len()..]
            .chars()
            .all(|ch| ch.is_ascii_hexdigit()));
        registry.insert_opened_web(
            first.clone(),
            "https://example.com/generated".to_string(),
            None,
            None,
        );
        registry.remove(&first);
        let second = registry.next_id().unwrap();
        assert_ne!(first, second);
        assert_eq!(second.len(), "wsess_".len() + 32);
        assert!(second["wsess_".len()..]
            .chars()
            .all(|ch| ch.is_ascii_hexdigit()));
    }

    #[test]
    fn stale_window_session_target_never_falls_through_to_semantic_matching() {
        let (mut registry, first, _second, _third) = registry_with_three();
        let stale = "wsess_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        registry.tag(&first, format!("reference {stale}")).unwrap();
        assert_eq!(
            registry.resolve_target(stale),
            Err(WindowCoreError::TargetNotFound(stale.to_string()))
        );
    }

    #[test]
    fn exact_window_session_target_has_precedence_over_semantic_matches() {
        let (mut registry, first, second, _third) = registry_with_three();
        registry.tag(&second, first.clone()).unwrap();
        assert_eq!(registry.resolve_target(&first).unwrap(), first);
    }
}
