use portus_window::{
    auth_session::{AuthenticatedSessionBroker, AuthenticatedSessionWebKit},
    bind_listener,
    media::MEDIA_SCHEME,
    serve,
    workspace::WorkspaceService,
    AuthConsentController, AuthenticatedSessionAuthority, DaemonHandler, DatabaseService,
    MediaAuthority, SocketCleanup, WebProfile, WebVideoAuthority, WindowManager, DEFAULT_IPC_PATH,
};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::Manager;
fn main() {
    #[cfg(target_os = "linux")]
    {
        std::env::set_var("GDK_BACKEND", "x11");
        if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
            std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        }
    }

    let socket_path = std::env::var("PORTUS_WINDOW_SOCKET")
        .or_else(|_| std::env::var("PORTUS_WINDOW_PIPE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_IPC_PATH));

    let media = Arc::new(MediaAuthority::new());
    let media_protocol = Arc::clone(&media);

    tauri::Builder::default()
        .register_uri_scheme_protocol(MEDIA_SCHEME, move |context, request| {
            media_protocol.handle_protocol(context.webview_label(), &request)
        })
        .setup(move |app| {
            let listener = tauri::async_runtime::block_on(async { bind_listener(&socket_path) })?;
            if !app.manage(SocketCleanup::for_bound_socket(socket_path.clone())?) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "socket cleanup state was already registered",
                )
                .into());
            }

            let database = Arc::new(DatabaseService::open_default()?);
            let web_profile = Arc::new(WebProfile::open_default()?);
            let web_video = WebVideoAuthority::new()?;
            let workspace = match WorkspaceService::connect() {
                Ok(service) => Some(Arc::new(service)),
                Err(error) => {
                    eprintln!("Portus Window workspace service disabled: {error}");
                    None
                }
            };
            let windows = Arc::new(WindowManager::new(
                app.handle().clone(),
                workspace,
                Arc::clone(&database),
                Arc::clone(&media),
                Arc::clone(&web_profile),
                web_video,
            ));
            let auth_authority = Arc::new(AuthenticatedSessionAuthority::new());
            let auth_broker = Arc::new(AuthenticatedSessionBroker::new(Arc::clone(&auth_authority)));
            let auth_webkit = Arc::new(AuthenticatedSessionWebKit::new(
                Arc::clone(&windows),
                Arc::clone(&auth_authority),
                Arc::clone(&auth_broker),
            ));
            let auth_consent = Arc::new(AuthConsentController::new(
                app.handle().clone(),
                Arc::clone(&auth_authority),
            ));
            windows.install_auth_consent_controller(Arc::downgrade(&auth_consent));

            let before_close_webkit = Arc::clone(&auth_webkit);
            windows.install_auth_lifecycle_hooks(
                Arc::new(move |window_session_id| {
                    if let Some(target) = before_close_webkit.applied_target_for_window(window_session_id) {
                        before_close_webkit
                            .revoke(&target)
                            .map_err(|error| error.to_string())?;
                    }
                    Ok(())
                }),
                {
                    let after_destroy_authority = Arc::clone(&auth_authority);
                    let after_destroy_webkit = Arc::clone(&auth_webkit);
                    let after_destroy_consent = Arc::clone(&auth_consent);
                    Arc::new(move |window_session_id| {
                        after_destroy_authority.revoke_window(window_session_id);
                        after_destroy_consent.cancel_window(window_session_id);
                        if let Some(target) = after_destroy_webkit.applied_target_for_window(window_session_id) {
                            if let Err(error) = after_destroy_webkit.revoke(&target) {
                                eprintln!("Portus Window authenticated-session destroy cleanup failed: {error}");
                            }
                        }
                    })
                },
            );

            let handler = Arc::new(DaemonHandler::new(
                windows,
                auth_authority,
                auth_webkit,
                auth_consent,
            ));
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) = serve(listener, handler).await {
                    eprintln!("Portus Window IPC server stopped: {error}");
                    app_handle.exit(1);
                }
            });

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("Portus Window daemon build failed")
        .run(|_app_handle, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                api.prevent_exit();
            }
        });
}
