use crate::handler::CommandHandler;
use portus_window_protocol::{
    decode_frame, encode_frame, ErrorCode, FrameError, Request, Response, MAX_FRAME_BYTES,
    PROTOCOL_VERSION,
};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const DEFAULT_SOCKET_PATH: &str = "/tmp/portus-window.socket";
pub const DEFAULT_PIPE_PATH: &str = r"\\.\pipe\portus-window";

#[cfg(unix)]
pub const DEFAULT_IPC_PATH: &str = DEFAULT_SOCKET_PATH;
#[cfg(windows)]
pub const DEFAULT_IPC_PATH: &str = DEFAULT_PIPE_PATH;

#[derive(Debug)]
pub enum PlatformListener {
    #[cfg(unix)]
    Unix(tokio::net::UnixListener),
    #[cfg(windows)]
    NamedPipe {
        pipe_name: String,
        server: tokio::net::windows::named_pipe::NamedPipeServer,
    },
}

#[cfg(unix)]
pub struct SocketCleanup {
    path: PathBuf,
    device: u64,
    inode: u64,
}

#[cfg(unix)]
impl SocketCleanup {
    pub fn for_bound_socket(path: PathBuf) -> io::Result<Self> {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};

        let metadata = std::fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_socket() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("'{}' is not a Unix socket", path.display()),
            ));
        }

        Ok(Self {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
}

#[cfg(unix)]
impl Drop for SocketCleanup {
    fn drop(&mut self) {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};

        if let Ok(metadata) = std::fs::symlink_metadata(&self.path) {
            if metadata.file_type().is_socket()
                && metadata.dev() == self.device
                && metadata.ino() == self.inode
            {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

#[cfg(windows)]
pub struct SocketCleanup {
    #[allow(dead_code)]
    pipe_name: String,
}

#[cfg(windows)]
impl SocketCleanup {
    pub fn for_bound_socket(path: PathBuf) -> io::Result<Self> {
        Ok(Self {
            pipe_name: path.to_string_lossy().into_owned(),
        })
    }
}

#[cfg(unix)]
pub fn bind_listener(path: &Path) -> io::Result<PlatformListener> {
    use std::os::unix::fs::PermissionsExt;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    recover_stale_socket(path)?;

    let listener = std::os::unix::net::UnixListener::bind(path)?;

    if let Err(error) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    if let Err(error) = listener.set_nonblocking(true) {
        let _ = std::fs::remove_file(path);
        return Err(error);
    }

    match tokio::net::UnixListener::from_std(listener) {
        Ok(listener) => Ok(PlatformListener::Unix(listener)),
        Err(error) => {
            let _ = std::fs::remove_file(path);
            Err(error)
        }
    }
}

#[cfg(windows)]
pub fn sanitize_pipe_name(path: &Path) -> String {
    let s = path.to_string_lossy();
    if s.starts_with(r"\\.\pipe\") {
        s.into_owned()
    } else {
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("portus-window");
        format!(r"\\.\pipe\{file_name}")
    }
}

#[cfg(windows)]
pub fn bind_listener(path: &Path) -> io::Result<PlatformListener> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let pipe_name = sanitize_pipe_name(path);
    let server = ServerOptions::new()
        .first_pipe_instance(true)
        .create(&pipe_name)?;

    Ok(PlatformListener::NamedPipe { pipe_name, server })
}

#[cfg(unix)]
fn recover_stale_socket(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::FileTypeExt;

    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };

    if !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("refusing to replace non-socket path '{}'", path.display()),
        ));
    }

    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("daemon socket '{}' is already active", path.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
            std::fs::remove_file(path)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub async fn serve(
    listener: PlatformListener,
    handler: Arc<impl CommandHandler + 'static>,
) -> io::Result<()> {
    match listener {
        #[cfg(unix)]
        PlatformListener::Unix(unix_listener) => loop {
            let (stream, _) = unix_listener.accept().await?;
            let handler = Arc::clone(&handler);
            tokio::spawn(async move {
                if let Err(error) = handle_connection(stream, handler).await {
                    eprintln!("Portus Window IPC connection failed: {error}");
                }
            });
        },
        #[cfg(windows)]
        PlatformListener::NamedPipe { pipe_name, server } => {
            use tokio::net::windows::named_pipe::ServerOptions;

            let mut current_server = server;
            loop {
                current_server.connect().await?;
                let connected_client = current_server;
                current_server = ServerOptions::new().create(&pipe_name)?;
                let handler = Arc::clone(&handler);
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(connected_client, handler).await {
                        eprintln!("Portus Window IPC named pipe connection failed: {error}");
                    }
                });
            }
        }
    }
}
#[cfg(unix)]
pub async fn handle_connection<S: AsyncReadExt + AsyncWriteExt + Unpin>(
    mut stream: S,
    handler: Arc<impl CommandHandler>,
) -> io::Result<()> {
    let frame_result = {
        #[cfg(target_os = "linux")]
        {
            match tokio::time::timeout(Duration::from_secs(5), read_frame(&mut stream)).await {
                Ok(result) => result,
                Err(_) => Err(FrameReadError::Timeout),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            read_frame(&mut stream).await
        }
    };
    let response = match frame_result {
        Ok(frame) => process_frame(&frame, &handler),
        Err(FrameReadError::TooLarge) => Response::error(
            ErrorCode::FrameTooLarge,
            format!("request exceeded the {MAX_FRAME_BYTES}-byte frame limit"),
        ),
        Err(FrameReadError::Empty) => {
            Response::error(ErrorCode::ValidationFailed, "request frame was empty")
        }
        Err(FrameReadError::MissingTerminator) => Response::error(
            ErrorCode::ValidationFailed,
            "request frame was not newline terminated",
        ),
        Err(FrameReadError::Timeout) => {
            Response::error(ErrorCode::Timeout, "request frame read timed out")
        }
        Err(FrameReadError::Io(error)) => return Err(error),
    };

    let payload = encode_frame(&response).map_err(frame_error_to_io)?;
    stream.write_all(&payload).await?;
    stream.flush().await
}

#[cfg(windows)]
pub async fn handle_connection<S: AsyncReadExt + AsyncWriteExt + Unpin>(
    mut stream: S,
    handler: Arc<impl CommandHandler>,
) -> io::Result<()> {
    loop {
        let response = match read_frame(&mut stream).await {
            Ok(frame) => process_frame(&frame, &handler),
            Err(FrameReadError::TooLarge) => Response::error(
                ErrorCode::FrameTooLarge,
                format!("request exceeded the {MAX_FRAME_BYTES}-byte frame limit"),
            ),
            Err(FrameReadError::Empty) => return Ok(()),
            Err(FrameReadError::MissingTerminator) => Response::error(
                ErrorCode::ValidationFailed,
                "request frame was not newline terminated",
            ),
            Err(FrameReadError::Timeout) => {
                Response::error(ErrorCode::Timeout, "request frame read timed out")
            }
            Err(FrameReadError::Io(error)) => return Err(error),
        };

        let payload = encode_frame(&response).map_err(frame_error_to_io)?;
        stream.write_all(&payload).await?;
        stream.flush().await?;
    }
}
fn process_frame(frame: &[u8], handler: &Arc<impl CommandHandler>) -> Response {
    let request = match decode_frame::<Request>(frame) {
        Ok(request) => request,
        Err(FrameError::TooLarge { .. }) => {
            return Response::error(ErrorCode::FrameTooLarge, "request frame was too large")
        }
        Err(error) => {
            return Response::error(
                ErrorCode::ValidationFailed,
                format!("could not decode request: {error}"),
            )
        }
    };

    if request.version() != PROTOCOL_VERSION {
        return Response::error(
            ErrorCode::VersionMismatch,
            format!(
                "protocol version {} is unsupported; expected {PROTOCOL_VERSION}",
                request.version()
            ),
        );
    }

    handler.handle(request)
}

pub async fn read_frame<R: AsyncReadExt + Unpin>(
    stream: &mut R,
) -> Result<Vec<u8>, FrameReadError> {
    let mut frame = Vec::with_capacity(256);
    let mut byte = [0_u8; 1];

    loop {
        let count = stream.read(&mut byte).await.map_err(FrameReadError::Io)?;
        if count == 0 {
            return if frame.is_empty() {
                Err(FrameReadError::Empty)
            } else {
                Err(FrameReadError::MissingTerminator)
            };
        }

        frame.push(byte[0]);
        if byte[0] == b'\n' {
            return Ok(frame);
        }
        if frame.len() > MAX_FRAME_BYTES {
            return Err(FrameReadError::TooLarge);
        }
    }
}

fn frame_error_to_io(error: FrameError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

#[derive(Debug)]
pub enum FrameReadError {
    Empty,
    Io(io::Error),
    MissingTerminator,
    Timeout,
    TooLarge,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handler::HealthHandler;
    use portus_window_protocol::{decode_frame, encode_frame, Response};

    #[cfg(unix)]
    use tempfile::tempdir;

    #[cfg(unix)]
    #[tokio::test]
    async fn ping_round_trip_over_unix_socket() {
        let directory = tempdir().expect("temporary directory should be created");
        let socket_path = directory.path().join("portus-window.socket");
        let listener = bind_listener(&socket_path).expect("listener should bind");
        let handler = Arc::new(HealthHandler);

        let server = tokio::spawn(async move {
            let PlatformListener::Unix(unix_listener) = listener;
            let (stream, _) = unix_listener.accept().await.expect("server should accept");
            handle_connection(stream, handler)
                .await
                .expect("server should handle ping");
        });

        let mut client = tokio::net::UnixStream::connect(&socket_path)
            .await
            .expect("client should connect");
        client
            .write_all(&encode_frame(&Request::ping()).expect("request should encode"))
            .await
            .expect("request should write");

        let frame = read_frame(&mut client).await.expect("response should read");
        let response: Response = decode_frame(&frame).expect("response should decode");
        assert!(matches!(response, Response::Ok { .. }));
        server.await.expect("server task should finish");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn ping_round_trip_over_windows_named_pipe() {
        use tokio::net::windows::named_pipe::ClientOptions;

        let pipe_name = format!(r"\\.\pipe\portus-window-test-{}", uuid::Uuid::new_v4());
        let pipe_path = PathBuf::from(&pipe_name);
        let listener = bind_listener(&pipe_path).expect("named pipe listener should bind");
        let handler = Arc::new(HealthHandler);

        let server = tokio::spawn(async move {
            match listener {
                PlatformListener::NamedPipe { server, .. } => {
                    server
                        .connect()
                        .await
                        .expect("server should connect client");
                    handle_connection(server, handler)
                        .await
                        .expect("server should handle ping");
                }
                #[cfg(unix)]
                PlatformListener::Unix(_) => unreachable!(),
            }
        });

        let mut client = ClientOptions::new()
            .open(&pipe_name)
            .expect("client should open named pipe");
        client
            .write_all(&encode_frame(&Request::ping()).expect("request should encode"))
            .await
            .expect("request should write");

        let frame = read_frame(&mut client).await.expect("response should read");
        let response: Response = decode_frame(&frame).expect("response should decode");
        assert!(matches!(response, Response::Ok { .. }));
        drop(client);
        server.await.expect("server task should finish");
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn version_mismatch_returns_deterministic_error() {
        let directory = tempdir().expect("temporary directory should be created");
        let socket_path = directory.path().join("portus-window.socket");
        let listener = bind_listener(&socket_path).expect("listener should bind");
        let handler = Arc::new(HealthHandler);

        let server = tokio::spawn(async move {
            let PlatformListener::Unix(unix_listener) = listener;
            let (stream, _) = unix_listener.accept().await.expect("server should accept");
            handle_connection(stream, handler)
                .await
                .expect("server should answer version mismatch");
        });

        let mut client = tokio::net::UnixStream::connect(&socket_path)
            .await
            .expect("client should connect");
        let request = Request::Ping {
            version: PROTOCOL_VERSION + 1,
        };
        client
            .write_all(&encode_frame(&request).expect("request should encode"))
            .await
            .expect("request should write");

        let frame = read_frame(&mut client).await.expect("response should read");
        let response: Response = decode_frame(&frame).expect("response should decode");
        assert!(matches!(
            response,
            Response::Error {
                error: portus_window_protocol::ResponseError {
                    code: ErrorCode::VersionMismatch,
                    ..
                },
                ..
            }
        ));
        server.await.expect("server task should finish");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stale_socket_is_recovered_but_regular_file_is_preserved() {
        let directory = tempdir().expect("temporary directory should be created");
        let socket_path = directory.path().join("stale.socket");
        let stale_listener = std::os::unix::net::UnixListener::bind(&socket_path)
            .expect("stale listener should bind");
        drop(stale_listener);

        let recovered = bind_listener(&socket_path).expect("stale socket should be replaced");
        drop(recovered);

        let file_path = directory.path().join("not-a-socket");
        std::fs::write(&file_path, b"preserve me").expect("regular file should be created");
        let error = bind_listener(&file_path).expect_err("regular file must not be replaced");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read(&file_path).expect("regular file should remain"),
            b"preserve me"
        );
    }
}
