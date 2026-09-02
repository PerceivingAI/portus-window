use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const PROTOCOL_VERSION: u16 = 7;
pub const MAX_FRAME_BYTES: usize = 64 * 1024;
pub const MAX_DESCRIPTION_CHARS: usize = 256;
pub const MAX_TITLE_CHARS: usize = 1_024;
pub const MAX_URL_CHARS: usize = 4_096;
pub const MAX_SOURCE_BYTES: usize = 4_096;
pub const MAX_HISTORY_QUERY_CHARS: usize = 512;
pub const MAX_RETENTION_DAYS: u32 = 3650;
pub const MAX_PROFILE_NAME_CHARS: usize = 64;
pub const MAX_OPEN_BATCH_WINDOWS: usize = 32;
pub const DEFAULT_INTERACTION_TIMEOUT_MS: u64 = 4_000;
pub const MIN_INTERACTION_TIMEOUT_MS: u64 = 100;
pub const MAX_INTERACTION_TIMEOUT_MS: u64 = 60_000;
pub const MAX_INTERACTION_ACTIONS: usize = 32;
pub const MAX_INTERACTION_SELECTOR_CHARS: usize = 512;
pub const MAX_INTERACTION_VALUE_CHARS: usize = 4_096;
pub const MAX_INTERACTION_TEXT_CHARS: usize = 1_024;
pub const MAX_INTERACTION_KEY_CHARS: usize = 64;
pub const MAX_INTERACTION_POST_ERRORS: usize = 8;
pub const MAX_CONSOLE_ENTRIES: usize = 50;
pub const MAX_CONSOLE_MESSAGE_CHARS: usize = 4_096;
pub const MAX_CONSOLE_SOURCE_CHARS: usize = 1_024;
pub const MAX_LOAD_ERROR_CHARS: usize = 1_024;
pub const MAX_URL_HISTORY_ENTRIES: usize = 128;
pub const MAX_URL_HISTORY_BYTES: usize = 32 * 1024;
pub const MAX_WORKSPACE_HISTORY_ENTRIES: usize = 128;
pub const MAX_STATUS_CONSOLE_BYTES: usize = 16 * 1024;
pub const MAX_HISTORY_RESULT_BYTES: usize = 48 * 1024;
pub const MAX_INTERACTION_RESULT_BYTES: usize = 48 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Web,
    LocalMedia,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentKind {
    Web,
    Image,
    Video,
    Audio,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoadState {
    Started,
    Loaded,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    Explicit,
    Destroyed,
    AbruptTermination,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsoleEntry {
    pub level: ConsoleLevel,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsoleLevel {
    Debug,
    Log,
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaState {
    pub playing: bool,
    pub paused: bool,
    pub ended: bool,
    pub duration_seconds: Option<f64>,
    pub position_seconds: Option<f64>,
    pub volume: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MediaAction {
    Play,
    Pause,
    Seek { seconds: f64 },
    SetVolume { level: f64 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WebVideoAction {
    State,
    Play,
    Pause,
    Seek { seconds: f64 },
    Mute,
    Unmute,
    SetVolume { level: f64 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebVideoResult {
    pub window_session_id: String,
    pub state: WebVideoState,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebVideoState {
    pub playing: bool,
    pub paused: bool,
    pub ended: bool,
    pub muted: bool,
    pub volume: Option<f64>,
    pub duration_seconds: Option<f64>,
    pub current_time_seconds: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowStateAction {
    Maximize,
    Minimize,
    Restore,
    Fullscreen,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkspaceTarget {
    Index { index: u32 },
    Name { name: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceInfo {
    pub index: u32,
    pub name: String,
    pub is_current: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConfigAction {
    Show,
    SetHistoryEnabled { enabled: bool },
    SetRetentionDays { days: Option<u32> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    pub history_enabled: bool,
    pub retention_days: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserKind {
    Firefox,
    Chromium,
    Chrome,
    Brave,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthSessionScope {
    Once,
    Session,
    Remembered,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthSessionDecision {
    Pending,
    Authorized,
    Denied,
    Revoked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthSessionTarget {
    Domain { domain: String },
    Window { window_session_id: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthSessionAction {
    Request {
        target: AuthSessionTarget,
        browser: BrowserKind,
        #[serde(default = "default_auth_scope")]
        scope: AuthSessionScope,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        presentation_window_session_id: Option<String>,
    },
    Status {
        target: AuthSessionTarget,
    },
    Apply {
        target: AuthSessionTarget,
        window_session_id: String,
    },
    Revoke {
        target: AuthSessionTarget,
    },
}

fn default_auth_scope() -> AuthSessionScope {
    AuthSessionScope::Session
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthSessionState {
    pub target: AuthSessionTarget,
    pub canonical_domain: String,
    pub browser: BrowserKind,
    pub scope: AuthSessionScope,
    pub decision: AuthSessionDecision,
    pub available: bool,
    pub applied: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthSessionResult {
    pub state: AuthSessionState,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OpenSource {
    Web { url: String },
    LocalMedia { path: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenResult {
    pub window_session_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaResult {
    pub window_session_id: String,
    pub state: MediaState,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordResult {
    pub window_session_id: String,
    pub path: String,
    pub duration_seconds: f64,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowGeometrySpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<WindowStateAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub always_on_top: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceTarget>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub source: OpenSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<WindowGeometrySpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<MediaAction>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSpec {
    pub version: u16,
    pub windows: Vec<WindowSpec>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyResult {
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub closed: Vec<String>,
    pub unchanged: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionActionKind {
    Click,
    Fill,
    PressKey,
    WaitForSelector,
    WaitForText,
    CheckSelector,
    CheckText,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InteractionAction {
    Click {
        selector: String,
    },
    Fill {
        selector: String,
        value: String,
    },
    PressKey {
        key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
    },
    WaitForSelector {
        selector: String,
    },
    WaitForText {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
    },
    CheckSelector {
        selector: String,
    },
    CheckText {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionStepCode {
    Ok,
    SelectorNotFound,
    SelectorAmbiguous,
    SelectorNotInteractable,
    TextNotFound,
    Timeout,
    ScriptError,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InteractionStepResult {
    pub index: usize,
    pub kind: InteractionActionKind,
    pub selector: Option<String>,
    pub ok: bool,
    pub code: InteractionStepCode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub elapsed_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InteractionScreenshot {
    pub out: String,
    pub overwrite: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionPostErrorCode {
    StatusFailed,
    ConsoleFailed,
    ScreenshotFailed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InteractionPostError {
    pub code: InteractionPostErrorCode,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InteractionResult {
    pub window_session_id: String,
    pub completed: bool,
    pub actions: Vec<InteractionStepResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_status: Option<ActiveWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub console: Option<ConsoleResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<ScreenshotResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_errors: Vec<InteractionPostError>,
    pub output_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActiveWindow {
    pub window_session_id: String,
    pub source_kind: SourceKind,
    pub content_kind: ContentKind,
    pub requested_source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendered_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_history: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_history_truncated: Option<bool>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authenticated: Option<bool>,
    pub load_state: LoadState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_error: Option<String>,
    #[serde(default)]
    pub console_errors: Vec<ConsoleEntry>,
    #[serde(default)]
    pub console_errors_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_state: Option<MediaState>,
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
    pub is_maximized: bool,
    pub is_minimized: bool,
    pub is_focused: bool,
    pub is_always_on_top: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<u32>,
    pub is_on_all_workspaces: bool,
    pub workspace_history: Vec<u32>,
    pub workspace_history_truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoricalWindow {
    pub historical_id: String,
    pub window_session_id: String,
    pub source_kind: SourceKind,
    pub content_kind: ContentKind,
    pub requested_source: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendered_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_url: Option<String>,
    pub url_history: Vec<String>,
    pub url_history_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<u32>,
    pub is_on_all_workspaces: bool,
    pub workspace_history: Vec<u32>,
    pub workspace_history_truncated: bool,
    pub opened_at: String,
    pub closed_at: String,
    pub close_reason: CloseReason,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryResult {
    pub entries: Vec<HistoricalWindow>,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClearHistoryResult {
    pub deleted: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsoleResult {
    pub window_session_id: String,
    pub entries: Vec<ConsoleEntry>,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenshotResult {
    pub window_session_id: String,
    pub path: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Ping {
        version: u16,
    },
    OpenBatch {
        version: u16,
        windows: Vec<WindowSpec>,
    },
    Open {
        version: u16,
        source: OpenSource,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        profile: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        geometry: Option<WindowGeometrySpec>,
    },
    Apply {
        version: u16,
        spec: WorkspaceSpec,
        #[serde(default)]
        prune: bool,
    },
    Export {
        version: u16,
    },
    List {
        version: u16,
    },
    History {
        version: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query: Option<String>,
        #[serde(default)]
        clear: bool,
    },
    Config {
        version: u16,
        action: ConfigAction,
    },
    Workspaces {
        version: u16,
    },
    Status {
        version: u16,
        target: String,
    },
    Console {
        version: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        #[serde(default)]
        all: bool,
    },
    Screenshot {
        version: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        out: String,
        #[serde(default)]
        overwrite: bool,
        #[serde(default)]
        all: bool,
    },
    Record {
        version: u16,
        target: String,
        out: String,
        #[serde(default = "default_record_duration_seconds")]
        duration_seconds: f64,
        #[serde(default)]
        overwrite: bool,
    },
    Media {
        version: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        action: MediaAction,
        #[serde(default)]
        all: bool,
    },
    WebVideo {
        version: u16,
        target: String,
        action: WebVideoAction,
    },
    AuthSession {
        version: u16,
        action: AuthSessionAction,
    },
    Interact {
        version: u16,
        target: String,
        actions: Vec<InteractionAction>,
        #[serde(default = "default_interaction_timeout_ms")]
        timeout_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        screenshot: Option<InteractionScreenshot>,
    },
    Tag {
        version: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        description: String,
        #[serde(default)]
        all: bool,
    },
    Focus {
        version: u16,
        target: String,
    },
    Resize {
        version: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        width: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        height: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        state: Option<WindowStateAction>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        always_on_top: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace: Option<WorkspaceTarget>,
        #[serde(default)]
        all: bool,
    },
    Close {
        version: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        #[serde(default)]
        all: bool,
    },
}

fn default_interaction_timeout_ms() -> u64 {
    DEFAULT_INTERACTION_TIMEOUT_MS
}

fn default_record_duration_seconds() -> f64 {
    30.0
}

impl Request {
    pub fn ping() -> Self {
        Self::Ping {
            version: PROTOCOL_VERSION,
        }
    }

    pub fn version(&self) -> u16 {
        match self {
            Self::Ping { version }
            | Self::OpenBatch { version, .. }
            | Self::Open { version, .. }
            | Self::Apply { version, .. }
            | Self::Export { version }
            | Self::List { version }
            | Self::History { version, .. }
            | Self::Config { version, .. }
            | Self::Workspaces { version }
            | Self::Status { version, .. }
            | Self::Console { version, .. }
            | Self::Screenshot { version, .. }
            | Self::Record { version, .. }
            | Self::Media { version, .. }
            | Self::WebVideo { version, .. }
            | Self::AuthSession { version, .. }
            | Self::Interact { version, .. }
            | Self::Tag { version, .. }
            | Self::Focus { version, .. }
            | Self::Resize { version, .. }
            | Self::Close { version, .. } => *version,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    Ok { version: u16, data: Value },
    Error { version: u16, error: ResponseError },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseError {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    ValidationFailed,
    VersionMismatch,
    DaemonUnavailable,
    InvalidResponse,
    Timeout,
    FrameTooLarge,
    DisplayUnavailable,
    InvalidWorkspace,
    PersistenceFailed,
    MediaFailed,
    WebVideoFailed,
    AuthSessionFailed,
    ScreenshotFailed,
    TargetAmbiguous,
    TargetNotFound,
    WindowOperationFailed,
    Internal,
}

impl ErrorCode {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::ValidationFailed => 1,
            Self::VersionMismatch => 2,
            Self::DaemonUnavailable | Self::InvalidResponse => 3,
            Self::TargetAmbiguous | Self::TargetNotFound => 4,
            Self::Timeout => 5,
            Self::DisplayUnavailable
            | Self::MediaFailed
            | Self::WebVideoFailed
            | Self::AuthSessionFailed
            | Self::PersistenceFailed
            | Self::ScreenshotFailed
            | Self::WindowOperationFailed => 6,
            Self::FrameTooLarge | Self::InvalidWorkspace | Self::Internal => 7,
        }
    }
}

impl Response {
    pub fn ok(data: Value) -> Self {
        Self::Ok {
            version: PROTOCOL_VERSION,
            data,
        }
    }

    pub fn error(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Error {
            version: PROTOCOL_VERSION,
            error: ResponseError {
                code,
                message: message.into(),
            },
        }
    }

    pub fn version(&self) -> u16 {
        match self {
            Self::Ok { version, .. } | Self::Error { version, .. } => *version,
        }
    }

    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Ok { .. } => 0,
            Self::Error { error, .. } => error.code.exit_code(),
        }
    }
}

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("payload exceeds maximum frame size")]
    TooLarge { actual: usize, maximum: usize },
    #[error("payload is invalid JSON: {0}")]
    InvalidJson(String),
}

pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, FrameError> {
    let mut encoded =
        serde_json::to_vec(value).map_err(|error| FrameError::InvalidJson(error.to_string()))?;
    encoded.push(b'\n');
    if encoded.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge {
            actual: encoded.len(),
            maximum: MAX_FRAME_BYTES,
        });
    }
    Ok(encoded)
}

pub fn decode_frame<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, FrameError> {
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge {
            actual: bytes.len(),
            maximum: MAX_FRAME_BYTES,
        });
    }
    let trimmed = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    serde_json::from_slice(trimmed).map_err(|error| FrameError::InvalidJson(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_encodes_to_valid_newline_terminated_frame() {
        let ping = Request::ping();
        let encoded = encode_frame(&ping).unwrap();
        assert!(encoded.ends_with(b"\n"));
        let decoded: Request = decode_frame(&encoded).unwrap();
        assert_eq!(decoded, ping);
    }

    #[test]
    fn workspace_spec_serialization_round_trip() {
        let spec = WorkspaceSpec {
            version: 1,
            windows: vec![WindowSpec {
                name: Some("test-window".to_string()),
                source: OpenSource::Web {
                    url: "https://example.com".to_string(),
                },
                description: Some("monitoring".to_string()),
                profile: Some("dev".to_string()),
                geometry: Some(WindowGeometrySpec {
                    width: Some(1280),
                    height: Some(720),
                    x: Some(0),
                    y: Some(0),
                    state: Some(WindowStateAction::Restore),
                    always_on_top: Some(false),
                    workspace: Some(WorkspaceTarget::Index { index: 1 }),
                }),
                media: None,
            }],
        };

        let json = serde_json::to_string(&spec).unwrap();
        let decoded: WorkspaceSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, spec);
    }

    #[test]
    fn apply_request_round_trips() {
        let apply = Request::Apply {
            version: PROTOCOL_VERSION,
            spec: WorkspaceSpec {
                version: 1,
                windows: vec![],
            },
            prune: true,
        };
        let encoded = encode_frame(&apply).unwrap();
        let decoded: Request = decode_frame(&encoded).unwrap();
        assert_eq!(decoded, apply);
    }
}
