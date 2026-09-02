use super::{request_for, Cli, CliOutcome, Commands};
use portus_window_protocol::{
    decode_frame, encode_frame, ErrorCode, FrameError, Request, Response, MAX_FRAME_BYTES,
    PROTOCOL_VERSION,
};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;

#[cfg(unix)]
pub async fn execute(cli: Cli) -> CliOutcome {
    use tokio::net::UnixStream;

    let duration = Duration::from_millis(cli.timeout_ms);
    if let Commands::Interact {
        interaction_timeout_ms,
        ..
    } = &cli.command
    {
        if cli.timeout_ms <= *interaction_timeout_ms {
            return CliOutcome::from_response(Response::error(
                ErrorCode::ValidationFailed,
                format!(
                    "global --timeout-ms ({}) must be greater than --interaction-timeout-ms ({interaction_timeout_ms})",
                    cli.timeout_ms
                ),
            ));
        }
    }
    let request = match request_for(&cli.command) {
        Ok(request) => request,
        Err(message) => {
            return CliOutcome::from_response(Response::error(
                ErrorCode::ValidationFailed,
                message,
            ));
        }
    };

    let mut stream = match timeout(duration, UnixStream::connect(&cli.socket)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => {
            return CliOutcome::from_response(Response::error(
                ErrorCode::DaemonUnavailable,
                format!(
                    "could not connect to daemon at '{}': {error}",
                    cli.socket.display()
                ),
            ));
        }
        Err(_) => {
            return CliOutcome::from_response(Response::error(
                ErrorCode::Timeout,
                format!("connection timed out after {} ms", cli.timeout_ms),
            ));
        }
    };

    execute_on_stream(&mut stream, request, duration, cli.timeout_ms).await
}

#[cfg(windows)]
pub async fn execute(cli: Cli) -> CliOutcome {
    use tokio::net::windows::named_pipe::ClientOptions;

    let duration = Duration::from_millis(cli.timeout_ms);
    if let Commands::Interact {
        interaction_timeout_ms,
        ..
    } = &cli.command
    {
        if cli.timeout_ms <= *interaction_timeout_ms {
            return CliOutcome::from_response(Response::error(
                ErrorCode::ValidationFailed,
                format!(
                    "global --timeout-ms ({}) must be greater than --interaction-timeout-ms ({interaction_timeout_ms})",
                    cli.timeout_ms
                ),
            ));
        }
    }
    let request = match request_for(&cli.command) {
        Ok(request) => request,
        Err(message) => {
            return CliOutcome::from_response(Response::error(
                ErrorCode::ValidationFailed,
                message,
            ));
        }
    };

    let pipe_name = {
        let s = cli.socket.to_string_lossy();
        if s.starts_with(r"\\.\pipe\") {
            s.into_owned()
        } else {
            let file_name = cli
                .socket
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("portus-window");
            format!(r"\\.\pipe\{file_name}")
        }
    };

    let mut client = match timeout(duration, async { ClientOptions::new().open(&pipe_name) }).await
    {
        Ok(Ok(client)) => client,
        Ok(Err(error)) => {
            return CliOutcome::from_response(Response::error(
                ErrorCode::DaemonUnavailable,
                format!(
                    "could not connect to daemon named pipe '{}': {error}",
                    pipe_name
                ),
            ));
        }
        Err(_) => {
            return CliOutcome::from_response(Response::error(
                ErrorCode::Timeout,
                format!("connection timed out after {} ms", cli.timeout_ms),
            ));
        }
    };

    execute_on_stream(&mut client, request, duration, cli.timeout_ms).await
}

async fn execute_on_stream<S: AsyncReadExt + AsyncWriteExt + Unpin>(
    stream: &mut S,
    request: Request,
    duration: Duration,
    timeout_ms: u64,
) -> CliOutcome {
    let interaction_request = matches!(request, Request::Interact { .. });

    let payload = match encode_frame(&request) {
        Ok(payload) => payload,
        Err(FrameError::TooLarge { actual, maximum }) => {
            return CliOutcome::from_response(Response::error(
                ErrorCode::FrameTooLarge,
                format!("request is {actual} bytes; maximum protocol frame is {maximum} bytes"),
            ));
        }
        Err(error) => {
            return CliOutcome::from_response(Response::error(
                ErrorCode::Internal,
                format!("failed to encode request: {error}"),
            ));
        }
    };

    match timeout(duration, stream.write_all(&payload)).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            return CliOutcome::from_response(Response::error(
                ErrorCode::DaemonUnavailable,
                format!("failed to send request: {error}"),
            ));
        }
        Err(_) => {
            return CliOutcome::from_response(Response::error(
                ErrorCode::Timeout,
                format!("request write timed out after {timeout_ms} ms"),
            ));
        }
    }

    let frame = match timeout(duration, read_frame(stream)).await {
        Ok(Ok(frame)) => frame,
        Ok(Err(error)) => {
            return CliOutcome::from_response(Response::error(
                ErrorCode::InvalidResponse,
                format!("failed to read daemon response: {error}"),
            ));
        }
        Err(_) => {
            return CliOutcome::from_response(Response::error(
                ErrorCode::Timeout,
                format!("daemon response timed out after {timeout_ms} ms"),
            ));
        }
    };

    match decode_frame::<Response>(&frame) {
        Ok(response) => outcome_from_daemon_response_for(response, interaction_request),
        Err(error) => CliOutcome::from_response(Response::error(
            ErrorCode::InvalidResponse,
            format!("daemon returned an invalid response: {error}"),
        )),
    }
}

#[cfg(test)]
pub(super) fn outcome_from_daemon_response(response: Response) -> CliOutcome {
    outcome_from_daemon_response_for(response, false)
}

#[cfg(any(unix, windows, test))]
pub(super) fn outcome_from_daemon_response_for(
    response: Response,
    interaction_request: bool,
) -> CliOutcome {
    let version = response.version();
    if version != PROTOCOL_VERSION {
        return CliOutcome::from_response(Response::error(
            ErrorCode::VersionMismatch,
            format!("daemon response uses protocol version {version}; expected {PROTOCOL_VERSION}"),
        ));
    }
    if interaction_request {
        if let Response::Ok { data, .. } = &response {
            let result = match serde_json::from_value::<portus_window_protocol::InteractionResult>(
                data.clone(),
            ) {
                Ok(result) => result,
                Err(error) => {
                    return CliOutcome::from_response(Response::error(
                        ErrorCode::InvalidResponse,
                        format!("daemon returned invalid interaction result data: {error}"),
                    ));
                }
            };
            let mut outcome = CliOutcome::from_response(response);
            if !result.completed {
                outcome.exit_code = if result.actions.iter().any(|step| {
                    !step.ok && step.code == portus_window_protocol::InteractionStepCode::Timeout
                }) {
                    5
                } else {
                    6
                };
            }
            return outcome;
        }
    }
    CliOutcome::from_response(response)
}

pub(super) async fn read_frame<R: AsyncReadExt + Unpin>(
    stream: &mut R,
) -> std::io::Result<Vec<u8>> {
    let mut frame = Vec::with_capacity(256);
    let mut byte = [0_u8; 1];

    loop {
        let count = stream.read(&mut byte).await?;
        if count == 0 {
            if frame.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "daemon closed the connection before sending a response",
                ));
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "daemon response was not newline terminated",
            ));
        }

        frame.push(byte[0]);
        if byte[0] == b'\n' {
            return Ok(frame);
        }
        if frame.len() > MAX_FRAME_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "daemon response exceeded the protocol frame limit",
            ));
        }
    }
}
