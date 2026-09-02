use clap::{Parser, Subcommand};
use portus_window_protocol::{
    DEFAULT_INTERACTION_TIMEOUT_MS, MAX_INTERACTION_TIMEOUT_MS, MIN_INTERACTION_TIMEOUT_MS,
};
use std::path::PathBuf;

#[cfg(unix)]
pub const DEFAULT_SOCKET_PATH: &str = "/tmp/portus-window.socket";
#[cfg(windows)]
pub const DEFAULT_SOCKET_PATH: &str = r"\\.\pipe\portus-window";

#[derive(Parser, Debug)]
#[command(
    name = "portus-window-cli",
    version,
    about = "CLI controller for Portus Window"
)]
pub struct Cli {
    #[arg(
        long,
        env = "PORTUS_WINDOW_SOCKET",
        default_value = DEFAULT_SOCKET_PATH,
        help = "Unix domain socket or Windows named pipe path for communication with the host daemon"
    )]
    pub socket: PathBuf,

    #[arg(
        long,
        default_value_t = 5_000,
        value_parser = clap::value_parser!(u64).range(1..),
        help = "Global IPC timeout in milliseconds"
    )]
    pub timeout_ms: u64,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Clone, Debug, PartialEq)]
pub enum Commands {
    #[command(about = "Check daemon availability and protocol version")]
    Ping,
    #[command(
        about = "Open a web URL or local media file in a new Portus Window, with optional initial geometry and state"
    )]
    #[command(about = "Open multiple web or local-media windows from a JSON manifest")]
    OpenBatch {
        #[arg(
            help = "Path to JSON manifest containing a windows array (reads STDIN if omitted or '-')"
        )]
        file: Option<PathBuf>,
    },
    Open {
        #[arg(
            help = "HTTP/HTTPS URL or local media path (PNG, JPG, MP4, WebM, WAV, FLAC, etc.; file:// URLs are rejected)"
        )]
        source: String,
        #[arg(long, help = "Optional custom context tag or description")]
        description: Option<String>,
        #[arg(long, help = "Named persistent profile to isolate cookies and storage")]
        profile: Option<String>,
        #[arg(
            long,
            help = "Wait until initial page load settles before returning status"
        )]
        wait_loaded: bool,
        #[arg(
            long,
            help = "Set initial window width in pixels (can be combined with height, position, and state)"
        )]
        width: Option<u32>,
        #[arg(
            long,
            help = "Set initial window height in pixels (can be combined with width, position, and state)"
        )]
        height: Option<u32>,
        #[arg(long, help = "Set initial window X coordinate")]
        x: Option<i32>,
        #[arg(long, help = "Set initial window Y coordinate")]
        y: Option<i32>,
        #[arg(long, conflicts_with_all = ["minimize", "restore"], help = "Open window maximized")]
        maximize: bool,
        #[arg(long, conflicts_with_all = ["maximize", "restore"], help = "Open window minimized")]
        minimize: bool,
        #[arg(long, conflicts_with_all = ["maximize", "minimize"], help = "Open window restored to normal size")]
        restore: bool,
        #[arg(long, conflicts_with_all = ["maximize", "minimize", "restore"], help = "Open window in fullscreen")]
        fullscreen: bool,
        #[arg(long, value_parser = clap::value_parser!(bool), help = "Set initial always-on-top state")]
        always_on_top: Option<bool>,
        #[arg(
            long,
            help = "Open window on workspace index or name (Windows: 1-based; Linux/X11: 0-based)"
        )]
        workspace: Option<String>,
    },

    #[command(
        about = "Apply a declarative workspace specification from a file or STDIN (e.g. cat spec.json | portus-window-cli apply -)"
    )]
    Apply {
        #[arg(
            short = 'f',
            long,
            help = "Path to JSON workspace spec file (reads from STDIN if omitted or '-')"
        )]
        file: Option<PathBuf>,
        #[arg(
            long,
            help = "Close active windows not specified in the declarative file"
        )]
        prune: bool,
    },
    #[command(
        about = "Export active windows and layouts into a declarative workspace specification"
    )]
    Export {
        #[arg(
            short = 'o',
            long,
            help = "Output file path (writes to STDOUT if omitted)"
        )]
        out: Option<PathBuf>,
    },
    #[command(about = "List all active windows with live titles, URLs, and states")]
    List,
    #[command(about = "Query or clear persistent closed-window history")]
    History {
        #[arg(
            long,
            conflicts_with = "clear",
            help = "Search keyword query across closed window history"
        )]
        query: Option<String>,
        #[arg(
            long,
            conflicts_with = "query",
            help = "Permanently purge closed history logs"
        )]
        clear: bool,
    },
    #[command(about = "Show or update persistent daemon configuration")]
    Config {
        #[arg(
            long,
            conflicts_with = "set",
            help = "Display active configuration values"
        )]
        show: bool,
        #[arg(
            long,
            conflicts_with = "show",
            help = "Set configuration property (history_enabled=<bool> or retention_days=<int|null>)"
        )]
        set: Option<String>,
    },
    #[command(about = "Request, inspect, apply, or revoke brokered browser credentials")]
    AuthSession {
        #[command(subcommand)]
        action: AuthSessionCommands,
    },
    #[command(about = "List host virtual desktops / workspaces")]
    Workspaces,
    #[command(
        about = "Return observed status, title, URL, auth state, and console errors for an active window"
    )]
    Status {
        #[arg(help = "Target window ID, title, URL, or tag substring")]
        target: String,
    },
    #[command(about = "Return bounded console output for active web windows")]
    Console {
        #[arg(help = "Target window ID, title, or URL substring")]
        target: Option<String>,
        #[arg(
            long,
            conflicts_with = "target",
            help = "Retrieve console logs across all active web windows"
        )]
        all: bool,
    },
    #[command(about = "Capture an active window to a PNG screenshot")]
    Screenshot {
        #[arg(help = "Target window ID, title, or URL substring")]
        target: Option<String>,
        #[arg(long, help = "Output PNG file path (e.g. /tmp/capture.png)")]
        out: Option<PathBuf>,
        #[arg(long, help = "Overwrite output file if it already exists")]
        overwrite: bool,
        #[arg(
            long,
            conflicts_with = "target",
            help = "Capture screenshots of all active windows"
        )]
        all: bool,
    },
    #[command(about = "Record window video for a specified duration")]
    Record {
        #[arg(help = "Target window ID, title, or URL substring")]
        target: String,
        #[arg(long, help = "Output video file path (.mp4 / .webm)")]
        out: PathBuf,
        #[arg(long, default_value_t = 30.0, help = "Duration in seconds to record")]
        duration_seconds: f64,
        #[arg(long, help = "Overwrite output file if it already exists")]
        overwrite: bool,
    },
    #[command(
        about = "Control local audio/video playback (local media windows only; not YouTube/web video)"
    )]
    Media {
        #[arg(
            help = "Target local-media window ID, title, or URL substring (omit when using --all)"
        )]
        target: Option<String>,
        #[command(subcommand)]
        action: MediaCommands,
        #[arg(
            long,
            conflicts_with = "target",
            help = "Apply media command to all active media windows"
        )]
        all: bool,
    },
    #[command(about = "Run ordered minimal DOM interactions against an open web window")]
    Interact {
        #[arg(help = "Target web window ID, title, or URL substring")]
        target: String,
        #[arg(
            long = "action",
            required = true,
            help = "Repeated JSON action: click, fill, press_key, wait_for_selector, wait_for_text, check_selector, check_text; use fill with value for text input"
        )]
        actions: Vec<String>,
        #[arg(
            long = "interaction-timeout-ms",
            default_value_t = DEFAULT_INTERACTION_TIMEOUT_MS,
            value_parser = clap::value_parser!(u64).range(MIN_INTERACTION_TIMEOUT_MS..=MAX_INTERACTION_TIMEOUT_MS),
            help = "Total batch execution timeout in milliseconds"
        )]
        interaction_timeout_ms: u64,
        #[arg(
            long = "screenshot-out",
            help = "Optional path to capture a screenshot after action batch finishes"
        )]
        screenshot_out: Option<PathBuf>,
        #[arg(
            long = "screenshot-overwrite",
            requires = "screenshot_out",
            help = "Overwrite post-action screenshot file"
        )]
        screenshot_overwrite: bool,
    },
    #[command(about = "Set or update an active window's description tag")]
    Tag {
        #[arg(help = "Target window ID, title, or URL substring (omit when using --all)")]
        target: Option<String>,
        #[arg(help = "Custom description text to assign (up to 256 characters)")]
        description: Option<String>,
        #[arg(long, help = "Apply tag to all active windows")]
        all: bool,
    },
    #[command(about = "Focus and raise an active window")]
    Focus {
        #[arg(help = "Target window ID, title, or URL substring")]
        target: String,
    },
    #[command(
        about = "Change active window geometry, position, workspace, or state (at least one operation required)"
    )]
    Resize {
        #[arg(help = "Target window ID, title, or URL substring")]
        target: Option<String>,
        #[arg(long, help = "Set window width in pixels")]
        width: Option<u32>,
        #[arg(long, help = "Set window height in pixels")]
        height: Option<u32>,
        #[arg(long, help = "Set window X coordinate")]
        x: Option<i32>,
        #[arg(long, help = "Set window Y coordinate")]
        y: Option<i32>,
        #[arg(long, conflicts_with_all = ["minimize", "restore"], help = "Maximize window")]
        maximize: bool,
        #[arg(long, conflicts_with_all = ["maximize", "restore"], help = "Minimize window")]
        minimize: bool,
        #[arg(long, conflicts_with_all = ["maximize", "minimize"], help = "Restore window to normal size")]
        restore: bool,
        #[arg(long, conflicts_with_all = ["maximize", "minimize", "restore"], help = "Enter fullscreen mode")]
        fullscreen: bool,
        #[arg(long, value_parser = clap::value_parser!(bool), help = "Keep window always on top")]
        always_on_top: Option<bool>,
        #[arg(
            long,
            help = "Move window to workspace index or name (Windows: 1-based; Linux/X11: 0-based)"
        )]
        workspace: Option<String>,
        #[arg(
            long,
            conflicts_with = "target",
            help = "Apply resize/state operation to all active windows"
        )]
        all: bool,
    },
    #[command(about = "Close one or all active windows")]
    Close {
        #[arg(help = "Target window ID, title, or URL substring")]
        target: Option<String>,
        #[arg(long, conflicts_with = "target", help = "Close all active windows")]
        all: bool,
    },
}

#[derive(Subcommand, Clone, Debug, PartialEq)]
pub enum AuthSessionCommands {
    #[command(about = "Request explicit user permission to use browser credentials")]
    Request {
        #[arg(
            long,
            conflicts_with = "window",
            requires = "browser",
            help = "Domain whose browser credentials may be used"
        )]
        domain: Option<String>,
        #[arg(
            long,
            conflicts_with = "domain",
            requires = "browser",
            help = "Exact open Portus Window session ID"
        )]
        window: Option<String>,
        #[arg(long, value_parser = ["firefox", "chromium", "chrome", "brave"], help = "Browser whose credentials are requested")]
        browser: String,
        #[arg(long, value_parser = ["once", "session", "remembered"], default_value = "session", help = "Permission lifetime")]
        scope: String,
        #[arg(long, help = "Human-readable reason shown to the user")]
        reason: Option<String>,
        #[arg(
            long,
            help = "Existing Portus Window session that hosts the consent modal for a domain request"
        )]
        presentation_window: Option<String>,
    },
    #[command(about = "Inspect an authenticated-session grant")]
    Status {
        #[arg(
            long,
            conflicts_with = "window",
            required_unless_present = "window",
            help = "Domain grant to inspect"
        )]
        domain: Option<String>,
        #[arg(
            long,
            conflicts_with = "domain",
            required_unless_present = "domain",
            help = "Exact window grant to inspect"
        )]
        window: Option<String>,
    },
    #[command(about = "Apply an authorized browser credential grant to an open window")]
    Apply {
        #[arg(long, conflicts_with = "window", help = "Domain grant to apply")]
        domain: Option<String>,
        #[arg(long, conflicts_with = "domain", help = "Exact window grant to apply")]
        window: Option<String>,
        #[arg(long, help = "Open destination Portus Window session ID")]
        window_session_id: String,
    },
    #[command(about = "Revoke an authenticated-session grant")]
    Revoke {
        #[arg(
            long,
            conflicts_with = "window",
            required_unless_present = "window",
            help = "Domain grant to revoke"
        )]
        domain: Option<String>,
        #[arg(
            long,
            conflicts_with = "domain",
            required_unless_present = "domain",
            help = "Exact window grant to revoke"
        )]
        window: Option<String>,
    },
}

#[derive(Subcommand, Clone, Debug, PartialEq)]
pub enum MediaCommands {
    #[command(about = "Resume local media playback")]
    Play,
    #[command(about = "Pause local media playback")]
    Pause,
    #[command(about = "Seek local media playback to an absolute position in seconds")]
    Seek {
        #[arg(long, help = "Target playback position in seconds")]
        seconds: f64,
    },
    #[command(about = "Set local media playback volume level (0.0 to 1.0)")]
    SetVolume {
        #[arg(long, help = "Volume level from 0.0 to 1.0")]
        level: f64,
    },
}
