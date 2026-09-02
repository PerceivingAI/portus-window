pub mod auth_session;
pub mod database;
pub mod handler;
pub mod interaction;
pub mod ipc;
pub mod linux_webkit;
pub mod media;
pub mod media_runtime;
pub mod observability;
pub mod screenshot;
pub mod tauri_window;
pub mod web_profile;
pub mod web_video;
pub mod web_video_runtime;
pub mod window_core;
pub mod workspace;

pub use auth_session::{
    AuthConsentController, AuthConsentError, AuthConsentView, AuthenticatedSessionAuthority,
};
pub use database::DatabaseService;
pub use handler::{CommandHandler, DaemonHandler};
pub use ipc::{
    bind_listener, handle_connection, read_frame, serve, FrameReadError, PlatformListener,
    SocketCleanup, DEFAULT_IPC_PATH, DEFAULT_PIPE_PATH, DEFAULT_SOCKET_PATH,
};
pub use media::MediaAuthority;
pub use tauri_window::WindowManager;
pub use web_profile::{WebProfile, WebProfileError};
pub use web_video::{WebVideoAuthority, WebVideoError, YouTubeVideo};
