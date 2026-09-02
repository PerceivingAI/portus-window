use parking_lot::Mutex;
use portus_window_protocol::{ContentKind, MAX_SOURCE_BYTES};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
#[cfg(target_os = "linux")]
use std::io::{BufRead, BufReader, Write};
use std::io::{Read, Seek, SeekFrom};
#[cfg(target_os = "linux")]
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::thread::JoinHandle;
use tauri::http::{header, Method, Request, Response, StatusCode};
use thiserror::Error;
use uuid::Uuid;
pub const MEDIA_SCHEME: &str = "portus-media";
pub const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_MEDIA_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
const CLASSIFICATION_HEADER_BYTES: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmittedMediaInfo {
    pub token: String,
    pub requested_source: String,
    pub content_kind: ContentKind,
    pub mime: String,
    pub title: String,
    pub bytes: u64,
}

#[derive(Debug, Error)]
pub enum MediaAdmissionError {
    #[error("local media validation failed: {0}")]
    Validation(String),
    #[error("local media I/O failed: {0}")]
    Io(String),
}

#[derive(Debug)]
struct AdmittedMedia {
    window_session_id: String,
    info: AdmittedMediaInfo,
    file: Mutex<File>,
}

#[derive(Debug, Default)]
struct MediaRegistry {
    by_token: HashMap<String, Arc<AdmittedMedia>>,
    by_window: HashMap<String, String>,
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct MediaHttpServer {
    port: u16,
    _thread: JoinHandle<()>,
}
pub struct MediaAuthority {
    registry: Arc<Mutex<MediaRegistry>>,
    #[cfg(target_os = "linux")]
    http_server: Mutex<Option<MediaHttpServer>>,
}

impl Default for MediaAuthority {
    fn default() -> Self {
        Self {
            registry: Arc::new(Mutex::new(MediaRegistry::default())),
            #[cfg(target_os = "linux")]
            http_server: Mutex::new(None),
        }
    }
}

impl MediaAuthority {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn admit(
        &self,
        window_session_id: &str,
        requested_path: &str,
    ) -> Result<AdmittedMediaInfo, MediaAdmissionError> {
        let requested_path = requested_path.trim();
        if requested_path.is_empty() {
            return Err(MediaAdmissionError::Validation(
                "local media path must not be empty".to_string(),
            ));
        }
        if requested_path.len() > MAX_SOURCE_BYTES {
            return Err(MediaAdmissionError::Validation(format!(
                "local media path must be at most {MAX_SOURCE_BYTES} UTF-8 bytes"
            )));
        }

        let supplied = PathBuf::from(requested_path);
        let supplied_metadata = std::fs::symlink_metadata(&supplied).map_err(|error| {
            MediaAdmissionError::Validation(format!(
                "could not inspect local media '{}': {error}",
                supplied.display()
            ))
        })?;
        if supplied_metadata.file_type().is_symlink() {
            return Err(MediaAdmissionError::Validation(
                "local media path must not be a symbolic link".to_string(),
            ));
        }
        let canonical = std::fs::canonicalize(&supplied).map_err(|error| {
            MediaAdmissionError::Validation(format!(
                "could not resolve local media '{}': {error}",
                supplied.display()
            ))
        })?;
        let path_metadata = std::fs::metadata(&canonical).map_err(|error| {
            MediaAdmissionError::Validation(format!(
                "could not inspect local media '{}': {error}",
                canonical.display()
            ))
        })?;
        if !path_metadata.is_file() {
            return Err(MediaAdmissionError::Validation(
                "local media must resolve to a regular file".to_string(),
            ));
        }
        let mut file = open_regular_readonly(&canonical)?;
        let metadata = file.metadata().map_err(|error| {
            MediaAdmissionError::Io(format!(
                "could not inspect opened local media '{}': {error}",
                canonical.display()
            ))
        })?;
        if !metadata.is_file() {
            return Err(MediaAdmissionError::Validation(
                "opened local media object is not a regular file".to_string(),
            ));
        }
        if metadata.len() == 0 {
            return Err(MediaAdmissionError::Validation(
                "local media file must not be empty".to_string(),
            ));
        }

        let mut header = [0_u8; CLASSIFICATION_HEADER_BYTES];
        let read = file.read(&mut header).map_err(|error| {
            MediaAdmissionError::Io(format!("could not inspect local media bytes: {error}"))
        })?;
        file.seek(SeekFrom::Start(0)).map_err(|error| {
            MediaAdmissionError::Io(format!("could not reset local media handle: {error}"))
        })?;
        let classified = classify_media(&canonical, &header[..read]).ok_or_else(|| {
            MediaAdmissionError::Validation(
                "local file is not an allowlisted passive image/audio/video format or its signature does not match its extension"
                    .to_string(),
            )
        })?;
        if classified.content_kind == ContentKind::Image && metadata.len() > MAX_IMAGE_BYTES {
            return Err(MediaAdmissionError::Validation(format!(
                "local image exceeds the {MAX_IMAGE_BYTES}-byte presentation limit"
            )));
        }

        let requested_source = supplied.to_string_lossy().into_owned();
        let title = canonical
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("Local Media")
            .to_string();

        let mut registry = self.registry.lock();
        if registry.by_window.contains_key(window_session_id) {
            return Err(MediaAdmissionError::Validation(format!(
                "window '{window_session_id}' already owns a local-media authorization"
            )));
        }
        let token = loop {
            let candidate = Uuid::new_v4().simple().to_string();
            if !registry.by_token.contains_key(&candidate) {
                break candidate;
            }
        };
        let info = AdmittedMediaInfo {
            token: token.clone(),
            requested_source,
            content_kind: classified.content_kind,
            mime: classified.mime.to_string(),
            title,
            bytes: metadata.len(),
        };
        let admitted = Arc::new(AdmittedMedia {
            window_session_id: window_session_id.to_string(),
            info: info.clone(),
            file: Mutex::new(file),
        });
        registry
            .by_window
            .insert(window_session_id.to_string(), token.clone());
        registry.by_token.insert(token, admitted);
        Ok(info)
    }

    pub fn revoke_window(&self, window_session_id: &str) -> bool {
        let mut registry = self.registry.lock();
        let Some(token) = registry.by_window.remove(window_session_id) else {
            return false;
        };
        registry.by_token.remove(&token).is_some()
    }

    pub fn contains_window(&self, window_session_id: &str) -> bool {
        self.registry
            .lock()
            .by_window
            .contains_key(window_session_id)
    }
    pub fn presentation_url(
        &self,
        info: &AdmittedMediaInfo,
    ) -> Result<url::Url, MediaAdmissionError> {
        #[cfg(target_os = "linux")]
        {
            let mut server = self.http_server.lock();
            if server.is_none() {
                let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(|error| {
                    MediaAdmissionError::Io(format!("could not bind local media server: {error}"))
                })?;
                let port = listener
                    .local_addr()
                    .map_err(|error| {
                        MediaAdmissionError::Io(format!(
                            "could not inspect local media server: {error}"
                        ))
                    })?
                    .port();
                let registry = Arc::clone(&self.registry);
                let thread = std::thread::Builder::new()
                    .name("portus-media".to_string())
                    .spawn(move || {
                        for stream in listener.incoming() {
                            let Ok(stream) = stream else { break };
                            let registry = Arc::clone(&registry);
                            std::thread::spawn(move || handle_http_connection(stream, registry));
                        }
                    })
                    .map_err(|error| {
                        MediaAdmissionError::Io(format!(
                            "could not spawn local media server: {error}"
                        ))
                    })?;
                *server = Some(MediaHttpServer {
                    port,
                    _thread: thread,
                });
            }
            let port = server.as_ref().expect("media server initialized").port;
            url::Url::parse(&format!("http://127.0.0.1:{port}/{}/view", info.token)).map_err(
                |error| MediaAdmissionError::Validation(format!("invalid media URL: {error}")),
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            url::Url::parse(&format!("{MEDIA_SCHEME}://localhost/{}/view", info.token)).map_err(
                |error| MediaAdmissionError::Validation(format!("invalid media URL: {error}")),
            )
        }
    }
    pub fn handle_protocol(
        &self,
        webview_label: &str,
        request: &Request<Vec<u8>>,
    ) -> Response<Vec<u8>> {
        let path = request.uri().path().trim_start_matches('/');
        let mut parts = path.split('/');
        let Some(token) = parts.next().filter(|value| !value.is_empty()) else {
            return plain_response(StatusCode::NOT_FOUND, b"not found".to_vec());
        };
        let Some(route) = parts.next() else {
            return plain_response(StatusCode::NOT_FOUND, b"not found".to_vec());
        };
        if parts.next().is_some() {
            return plain_response(StatusCode::NOT_FOUND, b"not found".to_vec());
        }

        let admitted = {
            let registry = self.registry.lock();
            registry.by_token.get(token).cloned()
        };
        let Some(admitted) = admitted else {
            return plain_response(StatusCode::NOT_FOUND, b"not found".to_vec());
        };
        if admitted.window_session_id != webview_label {
            return plain_response(StatusCode::FORBIDDEN, b"forbidden".to_vec());
        }

        match route {
            "view" => serve_view(&admitted, request.method()),
            "content" => serve_content(&admitted, request),
            _ => plain_response(StatusCode::NOT_FOUND, b"not found".to_vec()),
        }
    }
}
#[cfg(target_os = "linux")]
fn handle_http_connection(mut stream: TcpStream, registry: Arc<Mutex<MediaRegistry>>) {
    let mut reader = BufReader::new(&mut stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 2 {
        return;
    }
    let method = match parts[0] {
        "GET" => Method::GET,
        "HEAD" => Method::HEAD,
        _ => {
            let _ = write_http_response(
                &mut stream,
                405,
                "Method Not Allowed",
                "text/plain",
                b"method not allowed",
                false,
                None,
            );
            return;
        }
    };
    let path = parts[1];
    let mut segments = path.trim_start_matches('/').split('/');
    let token = segments.next().unwrap_or("");
    let route = segments.next().unwrap_or("");
    if token.is_empty() || segments.next().is_some() || !matches!(route, "view" | "content") {
        let _ = write_http_response(
            &mut stream,
            404,
            "Not Found",
            "text/plain",
            b"not found",
            method == Method::HEAD,
            None,
        );
        return;
    }
    let admitted = registry.lock().by_token.get(token).cloned();
    let Some(admitted) = admitted else {
        let _ = write_http_response(
            &mut stream,
            404,
            "Not Found",
            "text/plain",
            b"not found",
            method == Method::HEAD,
            None,
        );
        return;
    };
    let mut range = None;
    for header_line in reader.lines().map_while(Result::ok) {
        if header_line.is_empty() {
            break;
        }
        if let Some(value) = header_line
            .strip_prefix("Range:")
            .or_else(|| header_line.strip_prefix("range:"))
        {
            range = Some(value.trim().to_string());
        }
    }
    let uri = format!("http://127.0.0.1/{token}/{route}");
    let mut builder = Request::builder().method(&method).uri(uri);
    if let Some(value) = range {
        builder = builder.header(header::RANGE, value);
    }
    let request = match builder.body(Vec::new()) {
        Ok(request) => request,
        Err(_) => return,
    };
    let response = if route == "view" {
        serve_view(&admitted, request.method())
    } else {
        serve_content(&admitted, &request)
    };
    let status = response.status().as_u16();
    let reason = match status {
        200 => "OK",
        206 => "Partial Content",
        416 => "Range Not Satisfiable",
        500 => "Internal Server Error",
        _ => "Error",
    };
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream");
    let content_range = response
        .headers()
        .get(header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok());
    let _ = write_http_response(
        &mut stream,
        status,
        reason,
        content_type,
        response.body(),
        method == Method::HEAD,
        content_range,
    );
}

#[cfg(target_os = "linux")]
fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
    head_only: bool,
    content_range: Option<&str>,
) -> Result<(), std::io::Error> {
    let range = content_range
        .map(|value| format!("Content-Range: {value}\r\n"))
        .unwrap_or_default();
    let headers = format!("HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n{range}Connection: close\r\n\r\n", body.len());
    stream.write_all(headers.as_bytes())?;
    if !head_only {
        stream.write_all(body)?;
    }
    stream.flush()
}

fn open_regular_readonly(path: &Path) -> Result<File, MediaAdmissionError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options.open(path).map_err(|error| {
        MediaAdmissionError::Io(format!(
            "could not securely open local media '{}': {error}",
            path.display()
        ))
    })
}

#[derive(Clone, Copy)]
struct ClassifiedMedia {
    content_kind: ContentKind,
    mime: &'static str,
}

fn classify_media(path: &Path, header: &[u8]) -> Option<ClassifiedMedia> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let png = header.starts_with(b"\x89PNG\r\n\x1a\n");
    let jpeg = header.starts_with(&[0xff, 0xd8, 0xff]);
    let gif = header.starts_with(b"GIF87a") || header.starts_with(b"GIF89a");
    let webp = header.len() >= 12 && &header[..4] == b"RIFF" && &header[8..12] == b"WEBP";
    let riff_wave = header.len() >= 12 && &header[..4] == b"RIFF" && &header[8..12] == b"WAVE";
    let mp4_family = header.len() >= 12 && &header[4..8] == b"ftyp";
    let webm = header.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]);
    let ogg = header.starts_with(b"OggS");
    let mp3 = header.starts_with(b"ID3")
        || (header.len() >= 2 && header[0] == 0xff && (header[1] & 0xe0) == 0xe0);
    let flac = header.starts_with(b"fLaC");

    match extension.as_str() {
        "png" if png => Some(ClassifiedMedia {
            content_kind: ContentKind::Image,
            mime: "image/png",
        }),
        "jpg" | "jpeg" if jpeg => Some(ClassifiedMedia {
            content_kind: ContentKind::Image,
            mime: "image/jpeg",
        }),
        "gif" if gif => Some(ClassifiedMedia {
            content_kind: ContentKind::Image,
            mime: "image/gif",
        }),
        "webp" if webp => Some(ClassifiedMedia {
            content_kind: ContentKind::Image,
            mime: "image/webp",
        }),
        "mp4" | "m4v" | "mov" if mp4_family => Some(ClassifiedMedia {
            content_kind: ContentKind::Video,
            mime: if extension == "mov" {
                "video/quicktime"
            } else {
                "video/mp4"
            },
        }),
        "webm" if webm => Some(ClassifiedMedia {
            content_kind: ContentKind::Video,
            mime: "video/webm",
        }),
        "ogv" if ogg => Some(ClassifiedMedia {
            content_kind: ContentKind::Video,
            mime: "video/ogg",
        }),
        "mp3" if mp3 => Some(ClassifiedMedia {
            content_kind: ContentKind::Audio,
            mime: "audio/mpeg",
        }),
        "flac" if flac => Some(ClassifiedMedia {
            content_kind: ContentKind::Audio,
            mime: "audio/flac",
        }),
        "wav" if riff_wave => Some(ClassifiedMedia {
            content_kind: ContentKind::Audio,
            mime: "audio/wav",
        }),
        "ogg" | "opus" if ogg => Some(ClassifiedMedia {
            content_kind: ContentKind::Audio,
            mime: "audio/ogg",
        }),
        "m4a" if mp4_family => Some(ClassifiedMedia {
            content_kind: ContentKind::Audio,
            mime: "audio/mp4",
        }),
        _ => None,
    }
}

pub fn validate_presentation_navigation(url: &url::Url, token: &str) -> bool {
    let is_custom_scheme = url.scheme() == MEDIA_SCHEME
        && url.host_str() == Some("localhost")
        && url.path() == format!("/{token}/view");
    let is_webview2_custom = (url.scheme() == "http" || url.scheme() == "https")
        && url.host_str() == Some(&format!("{MEDIA_SCHEME}.localhost"))
        && url.path() == format!("/{token}/view");
    #[cfg(target_os = "linux")]
    let is_loopback = url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.path() == format!("/{token}/view");
    #[cfg(not(target_os = "linux"))]
    let is_loopback = false;
    (is_custom_scheme || is_webview2_custom || is_loopback || url.as_str() == "about:blank")
        && url.query().is_none()
        && url.fragment().is_none()
}

fn serve_view(admitted: &AdmittedMedia, method: &Method) -> Response<Vec<u8>> {
    if method != Method::GET && method != Method::HEAD {
        return plain_response(
            StatusCode::METHOD_NOT_ALLOWED,
            b"method not allowed".to_vec(),
        );
    }
    let html = presentation_html(admitted.info.content_kind);
    let body = if method == Method::HEAD {
        Vec::new()
    } else {
        html.as_bytes().to_vec()
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-content-type-options", "nosniff")
        .header(
            "content-security-policy",
            "default-src 'none'; img-src 'self' portus-media: http://127.0.0.1:* data:; media-src 'self' portus-media: http://127.0.0.1:* blob:; style-src 'unsafe-inline'; script-src 'unsafe-inline'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
        )
        .header(header::CONTENT_LENGTH, html.len().to_string())
        .body(body)
        .expect("static media view response must be valid")
}

fn presentation_html(kind: ContentKind) -> &'static str {
    match kind {
        ContentKind::Image => "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Portus Window</title><style>html,body{margin:0;width:100%;height:100%;background:#111;overflow:hidden}body{display:flex;align-items:center;justify-content:center}img{max-width:100%;max-height:100%;object-fit:contain}</style></head><body><img src=\"content\" alt=\"Local image\"></body></html>",
        ContentKind::Video => r#"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Portus Window</title><style>
html,body{margin:0;width:100%;height:100%;background:#000;overflow:hidden}body{display:flex;align-items:center;justify-content:center}.video-wrap{position:relative;width:100%;height:100%}video{width:100%;height:100%;object-fit:contain}.video-play{position:absolute;left:50%;top:50%;transform:translate(-50%,-50%);width:64px;height:64px;padding:0;border:0;border-radius:50%;display:flex;align-items:center;justify-content:center;background:rgba(0,0,0,.65);color:#fff;cursor:pointer;z-index:2}.video-play:hover{background:rgba(0,0,0,.8)}.video-play svg{width:30px;height:30px;display:block;fill:none;stroke:currentColor;stroke-width:2;stroke-linecap:round;stroke-linejoin:round}.video-controls{position:absolute;left:12px;right:12px;bottom:12px;display:flex;align-items:center;gap:10px;padding:8px 10px;border-radius:8px;background:rgba(0,0,0,.72);color:#fff;z-index:3;transition:opacity .15s ease}.video-wrap.controls-hidden .video-controls{opacity:0;pointer-events:none}.video-wrap.controls-hidden .video-play{opacity:0;pointer-events:none}.video-transport,.video-volume,.video-fullscreen{width:36px;height:36px;flex:0 0 36px;padding:0;border:0;border-radius:6px;display:inline-flex;align-items:center;justify-content:center;background:transparent;color:#fff;cursor:pointer}.video-transport:hover,.video-volume:hover,.video-fullscreen:hover{background:rgba(255,255,255,.14)}.video-transport svg,.video-volume svg,.video-fullscreen svg{width:20px;height:20px;display:block;fill:none;stroke:currentColor;stroke-width:2;stroke-linecap:round;stroke-linejoin:round}.video-timeline{flex:1;min-width:80px;height:5px;appearance:none;-webkit-appearance:none;background:transparent;cursor:pointer}.video-timeline::-webkit-slider-runnable-track{height:5px;background:linear-gradient(to right,#fff var(--progress,0%),#444 var(--progress,0%))}.video-timeline::-webkit-slider-thumb{appearance:none;-webkit-appearance:none;width:12px;height:12px;margin-top:-3.5px;border:0;border-radius:50%;background:#fff;cursor:pointer}.video-time{min-width:92px;text-align:center;font:13px/1 sans-serif;color:#ddd;white-space:nowrap}.video-volume-range{width:90px;height:5px;accent-color:#fff;cursor:pointer}.sr-only{position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0,0,0,0);white-space:nowrap;border:0}</style></head><body><div class="video-wrap"><video id="portus-media-player" src="content" preload="metadata"></video><button id="portus-video-play-center" class="video-play" type="button" aria-label="Play"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M8 5v14l11-7z"></path></svg></button><div class="video-controls"><button id="portus-video-toggle" class="video-transport" type="button" aria-label="Play"><svg viewBox="0 0 24 24" aria-hidden="true"><path id="portus-video-toggle-icon" d="M8 5v14l11-7z"></path></svg></button><label class="sr-only" for="portus-video-timeline">Seek</label><input id="portus-video-timeline" class="video-timeline" type="range" min="0" max="0" step="0.1" value="0" aria-valuetext="0:00 of 0:00"><span id="portus-video-time" class="video-time" aria-live="off">0:00 / 0:00</span><button id="portus-video-volume" class="video-volume" type="button" aria-label="Mute"><svg viewBox="0 0 24 24" aria-hidden="true"><path id="portus-video-volume-icon" d="M11 5 6 9H3v6h3l5 4zM15.5 8.5a5 5 0 0 1 0 7M18 5.5a9 9 0 0 1 0 13"></path></svg></button><label class="sr-only" for="portus-video-volume-range">Volume</label><input id="portus-video-volume-range" class="video-volume-range" type="range" min="0" max="1" step="0.01" value="1" aria-label="Volume"><button id="portus-video-fullscreen" class="video-fullscreen" type="button" aria-label="Fullscreen"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M8 3H5a2 2 0 0 0-2 2v3M16 3h3a2 2 0 0 1 2 2v3M8 21H5a2 2 0 0 1-2-2v-3M16 21h3a2 2 0 0 0 2-2v-3"></path></svg></button></div></div><script>
var wrap=document.querySelector('.video-wrap');var media=document.getElementById('portus-media-player');var center=document.getElementById('portus-video-play-center');var toggle=document.getElementById('portus-video-toggle');var toggleIcon=document.getElementById('portus-video-toggle-icon');var timeline=document.getElementById('portus-video-timeline');var time=document.getElementById('portus-video-time');var volumeButton=document.getElementById('portus-video-volume');var volumeIcon=document.getElementById('portus-video-volume-icon');var volumeRange=document.getElementById('portus-video-volume-range');var fullscreen=document.getElementById('portus-video-fullscreen');var hideTimer=null;function showControls(){wrap.classList.remove('controls-hidden');if(hideTimer)clearTimeout(hideTimer);hideTimer=null;if(!media.paused&&!media.ended){hideTimer=setTimeout(function(){wrap.classList.add('controls-hidden');},2000);}}function scheduleHide(){showControls();}function formatTime(value){if(!isFinite(value)||value<0)return '0:00';var seconds=Math.floor(value);var hours=Math.floor(seconds/3600);seconds%=3600;var minutes=Math.floor(seconds/60);seconds%=60;return (hours?hours+':':'')+('0'+minutes).slice(-2)+':'+('0'+seconds).slice(-2);}function sync(){var duration=isFinite(media.duration)&&media.duration>=0?media.duration:0;var position=isFinite(media.currentTime)&&media.currentTime>=0?media.currentTime:0;timeline.max=String(duration);timeline.value=String(duration?Math.min(position,duration):0);timeline.style.setProperty('--progress',(duration?Math.min(position,duration)/duration*100:0)+'%');timeline.setAttribute('aria-valuetext',formatTime(position)+' of '+formatTime(duration));time.textContent=formatTime(position)+' / '+formatTime(duration);var paused=media.paused||media.ended;toggleIcon.setAttribute('d',paused?'M8 5v14l11-7z':'M7 5v14M17 5v14');toggle.setAttribute('aria-label',paused?'Play':'Pause');center.style.display=paused?'flex':'none';toggleIcon.setAttribute('fill','none');var muted=media.muted||media.volume===0;volumeButton.setAttribute('aria-label',muted?'Unmute':'Mute');volumeIcon.setAttribute('d',muted?'M11 5 6 9H3v6h3l5 4zM17 9l4 6M21 9l-4 6':'M11 5 6 9H3v6h3l5 4zM15.5 8.5a5 5 0 0 1 0 7M18 5.5a9 9 0 0 1 0 13');volumeRange.value=String(media.muted?0:media.volume);}function togglePlayback(){if(media.paused||media.ended){media.play();}else{media.pause();}showControls();}wrap.addEventListener('pointermove',scheduleHide);wrap.addEventListener('pointerdown',showControls);media.addEventListener('click',function(event){if(event.target===media){togglePlayback();}});center.addEventListener('click',togglePlayback);toggle.addEventListener('click',togglePlayback);timeline.addEventListener('input',function(){var duration=isFinite(media.duration)?media.duration:0;var value=Number(timeline.value);if(duration>0&&isFinite(value))media.currentTime=Math.min(Math.max(value,0),duration);sync();showControls();});volumeButton.addEventListener('click',function(){media.muted=!media.muted;sync();showControls();});volumeRange.addEventListener('input',function(){var value=Math.min(1,Math.max(0,Number(volumeRange.value)));media.volume=value;media.muted=value===0;sync();showControls();});fullscreen.addEventListener('click',function(){showControls();window.location.href='portus-window-action://fullscreen';});media.addEventListener('volumechange',sync);['loadedmetadata','durationchange','timeupdate','play','pause','ended'].forEach(function(event){media.addEventListener(event,sync);});sync();</script></body></html>"#,
        ContentKind::Audio => audio_presentation_html(),
        ContentKind::Web => "",
    }
}
#[cfg(target_os = "linux")]
fn audio_presentation_html() -> &'static str {
    "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Portus Window</title><style>html,body{margin:0;width:100%;height:100%;background:#111;color:#eee}body{display:flex;align-items:center;justify-content:center}.player{width:90vw;max-width:720px;padding:20px;box-sizing:border-box;border:1px solid #333;border-radius:10px;background:#181818}.controls{display:flex;align-items:center;gap:12px}.transport{width:42px;height:34px;padding:0;display:inline-flex;align-items:center;justify-content:center;border:0;border-radius:6px;background:transparent;color:#eee;cursor:pointer}.transport:hover{background:#2a2a2a}.transport svg{width:18px;height:18px;display:block;fill:none;stroke:currentColor;stroke-width:2;stroke-linecap:round;stroke-linejoin:round}.timeline{flex:1;height:6px;cursor:pointer}.time{min-width:92px;text-align:right;font-size:13px;color:#ccc}.sr-only{position:absolute;width:1px;height:1px;padding:0;margin:-1px;overflow:hidden;clip:rect(0,0,0,0);white-space:nowrap;border:0}</style></head><body><main class=\"player\"><audio id=\"portus-media-player\" src=\"content\" preload=\"metadata\"></audio><div class=\"controls\"><button id=\"portus-media-toggle\" class=\"transport\" type=\"button\" aria-label=\"Play\"><svg id=\"portus-media-icon\" viewBox=\"0 0 24 24\" aria-hidden=\"true\"><path d=\"M8 5v14l11-7z\"></path></svg></button><label class=\"sr-only\" for=\"portus-media-timeline\">Seek</label><input id=\"portus-media-timeline\" class=\"timeline\" type=\"range\" min=\"0\" max=\"0\" step=\"0.1\" value=\"0\" aria-valuetext=\"0:00 of 0:00\"><span id=\"portus-media-time\" class=\"time\" aria-live=\"off\">0:00 / 0:00</span></div></main><script>var media=document.getElementById('portus-media-player');var toggle=document.getElementById('portus-media-toggle');var timeline=document.getElementById('portus-media-timeline');var time=document.getElementById('portus-media-time');var icon=document.getElementById('portus-media-icon');function formatTime(value){if(!isFinite(value)||value<0)return '0:00';var seconds=Math.floor(value);var minutes=Math.floor(seconds/60);return minutes+':'+('0'+(seconds%60)).slice(-2);}function sync(){var duration=isFinite(media.duration)&&media.duration>=0?media.duration:0;var position=isFinite(media.currentTime)&&media.currentTime>=0?Math.min(media.currentTime,duration||media.currentTime):0;timeline.max=String(duration);timeline.value=String(Math.min(position,duration||position));timeline.setAttribute('aria-valuetext',formatTime(position)+' of '+formatTime(duration));time.textContent=formatTime(position)+' / '+formatTime(duration);var paused=media.paused||media.ended;icon.innerHTML=paused?'<path d=\"M8 5v14l11-7z\"></path>':'<path d=\"M7 5v14M17 5v14\"></path>';toggle.setAttribute('aria-label',paused?'Play':'Pause');}toggle.addEventListener('click',function(){if(media.paused||media.ended){media.play();}else{media.pause();}});timeline.addEventListener('input',function(){var duration=isFinite(media.duration)?media.duration:0;var value=Number(timeline.value);if(duration>0&&isFinite(value)){media.currentTime=Math.min(Math.max(value,0),duration);sync();}});['loadedmetadata','durationchange','timeupdate','play','pause','ended'].forEach(function(event){media.addEventListener(event,sync);});sync();</script></body></html>"
}

#[cfg(not(target_os = "linux"))]
fn audio_presentation_html() -> &'static str {
    "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Portus Window</title><style>html,body{margin:0;width:100%;height:100%;background:#111;color:#eee}body{display:flex;align-items:center;justify-content:center}audio{width:min(720px,90vw)}</style></head><body><audio id=\"portus-media-player\" src=\"content\" controls preload=\"metadata\"></audio></body></html>"
}

fn serve_content(admitted: &AdmittedMedia, request: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return plain_response(
            StatusCode::METHOD_NOT_ALLOWED,
            b"method not allowed".to_vec(),
        );
    }
    let total = admitted.info.bytes;
    let explicit_range = request
        .headers()
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok());

    let selection = match explicit_range {
        Some(value) => match parse_range(value, total) {
            Ok(range) => range,
            Err(()) => return range_not_satisfiable(total),
        },
        None if admitted.info.content_kind == ContentKind::Image => (0, total - 1),
        None => (0, total.saturating_sub(1).min(MAX_MEDIA_RESPONSE_BYTES - 1)),
    };
    let capped = cap_range(selection.0, selection.1);
    let partial = explicit_range.is_some() || capped.1 + 1 < total;
    let length = capped.1 - capped.0 + 1;

    let body = if request.method() == Method::HEAD {
        Vec::new()
    } else {
        match read_range(admitted, capped.0, length) {
            Ok(body) => body,
            Err(message) => {
                return plain_response(StatusCode::INTERNAL_SERVER_ERROR, message.into_bytes())
            }
        }
    };

    let mut builder = Response::builder()
        .status(if partial {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header(header::CONTENT_TYPE, admitted.info.mime.as_str())
        .header(header::CONTENT_LENGTH, length.to_string())
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-content-type-options", "nosniff");
    if partial {
        builder = builder.header(
            header::CONTENT_RANGE,
            format!("bytes {}-{}/{}", capped.0, capped.1, total),
        );
    }
    builder
        .body(body)
        .expect("validated local-media response must be valid")
}

fn cap_range(start: u64, requested_end: u64) -> (u64, u64) {
    let max_end = start.saturating_add(MAX_MEDIA_RESPONSE_BYTES - 1);
    (start, requested_end.min(max_end))
}

fn parse_range(value: &str, total: u64) -> Result<(u64, u64), ()> {
    let value = value.strip_prefix("bytes=").ok_or(())?;
    if value.contains(',') || total == 0 {
        return Err(());
    }
    let (start, end) = value.split_once('-').ok_or(())?;
    if start.is_empty() {
        let suffix: u64 = end.parse().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        let suffix = suffix.min(total);
        return Ok((total - suffix, total - 1));
    }

    let start: u64 = start.parse().map_err(|_| ())?;
    if start >= total {
        return Err(());
    }
    let end = if end.is_empty() {
        total - 1
    } else {
        end.parse::<u64>().map_err(|_| ())?.min(total - 1)
    };
    if end < start {
        return Err(());
    }
    Ok((start, end))
}

fn read_range(admitted: &AdmittedMedia, start: u64, length: u64) -> Result<Vec<u8>, String> {
    let length = usize::try_from(length).map_err(|_| "media range is too large".to_string())?;
    let mut file = admitted.file.lock();
    file.seek(SeekFrom::Start(start))
        .map_err(|error| format!("could not seek admitted media: {error}"))?;
    let mut body = vec![0_u8; length];
    file.read_exact(&mut body)
        .map_err(|error| format!("could not read admitted media: {error}"))?;
    Ok(body)
}

fn range_not_satisfiable(total: u64) -> Response<Vec<u8>> {
    Response::builder()
        .status(StatusCode::RANGE_NOT_SATISFIABLE)
        .header(header::CONTENT_RANGE, format!("bytes */{total}"))
        .header(header::CACHE_CONTROL, "no-store")
        .body(Vec::new())
        .expect("static range error response must be valid")
}

fn plain_response(status: StatusCode, body: Vec<u8>) -> Response<Vec<u8>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(header::CACHE_CONTROL, "no-store")
        .header("x-content-type-options", "nosniff")
        .body(body)
        .expect("static media error response must be valid")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_file(extension: &str, bytes: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(format!("sample.{extension}"));
        std::fs::write(&path, bytes).unwrap();
        (directory, path)
    }

    #[test]
    fn passive_media_classification_is_signature_and_extension_bound() {
        let (_directory, png) = make_file("png", b"\x89PNG\r\n\x1a\nrest");
        let authority = MediaAuthority::new();
        let admitted = authority
            .admit(
                "wsess_00000000000000000000000000000001",
                png.to_str().unwrap(),
            )
            .unwrap();
        assert_eq!(admitted.content_kind, ContentKind::Image);
        assert_eq!(admitted.mime, "image/png");

        let (_directory, disguised) = make_file("png", b"<html><script>alert(1)</script>");
        assert!(authority
            .admit(
                "wsess_00000000000000000000000000000002",
                disguised.to_str().unwrap()
            )
            .is_err());

        let (_directory, svg) = make_file("svg", b"<svg xmlns='http://www.w3.org/2000/svg'></svg>");
        assert!(authority
            .admit(
                "wsess_00000000000000000000000000000003",
                svg.to_str().unwrap()
            )
            .is_err());
    }

    #[test]
    fn active_local_content_and_media_navigation_are_fail_closed() {
        let authority = MediaAuthority::new();
        for (extension, bytes) in [
            ("html", b"<html></html>".as_slice()),
            ("js", b"alert(1)".as_slice()),
            ("svg", b"<svg></svg>".as_slice()),
        ] {
            let (_directory, path) = make_file(extension, bytes);
            assert!(authority
                .admit(
                    "wsess_00000000000000000000000000000001",
                    path.to_str().unwrap()
                )
                .is_err());
        }

        let token = "0123456789abcdef";
        assert!(validate_presentation_navigation(
            &url::Url::parse(&format!("{MEDIA_SCHEME}://localhost/{token}/view")).unwrap(),
            token
        ));
        assert!(!validate_presentation_navigation(
            &url::Url::parse(&format!("{MEDIA_SCHEME}://localhost/other/view")).unwrap(),
            token
        ));
        assert!(!validate_presentation_navigation(
            &url::Url::parse(&format!("{MEDIA_SCHEME}://localhost/{token}/content")).unwrap(),
            token
        ));
        assert!(!validate_presentation_navigation(
            &url::Url::parse(&format!("{MEDIA_SCHEME}://localhost/{token}/view?x=1")).unwrap(),
            token
        ));
    }

    #[test]
    fn admission_rejects_directories_and_revokes_with_window() {
        let authority = MediaAuthority::new();
        let directory = tempfile::tempdir().unwrap();
        assert!(authority
            .admit(
                "wsess_00000000000000000000000000000001",
                directory.path().to_str().unwrap()
            )
            .is_err());

        let path = directory.path().join("audio.mp3");
        std::fs::write(&path, b"ID3sample-audio").unwrap();
        authority
            .admit(
                "wsess_00000000000000000000000000000001",
                path.to_str().unwrap(),
            )
            .unwrap();
        assert!(authority.contains_window("wsess_00000000000000000000000000000001"));
        assert!(authority.revoke_window("wsess_00000000000000000000000000000001"));
        assert!(!authority.contains_window("wsess_00000000000000000000000000000001"));
    }

    #[test]
    fn protocol_authorization_is_token_and_window_bound() {
        let (_directory, path) = make_file("mp3", b"ID3sample-audio");
        let authority = MediaAuthority::new();
        let info = authority
            .admit(
                "wsess_00000000000000000000000000000001",
                path.to_str().unwrap(),
            )
            .unwrap();
        let request = Request::builder()
            .method(Method::GET)
            .uri(format!("{MEDIA_SCHEME}://localhost/{}/view", info.token))
            .body(Vec::new())
            .unwrap();
        assert_eq!(
            authority
                .handle_protocol("wsess_00000000000000000000000000000001", &request)
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            authority
                .handle_protocol("wsess_00000000000000000000000000009999", &request)
                .status(),
            StatusCode::FORBIDDEN
        );

        authority.revoke_window("wsess_00000000000000000000000000000001");
        assert_eq!(
            authority
                .handle_protocol("wsess_00000000000000000000000000000001", &request)
                .status(),
            StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn byte_ranges_are_bounded_and_invalid_ranges_are_typed_http() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sample.mp3");
        let mut file = File::create(&path).unwrap();
        file.write_all(b"ID3").unwrap();
        file.write_all(&vec![7_u8; 1024 * 1024]).unwrap();
        drop(file);

        let authority = MediaAuthority::new();
        let info = authority
            .admit(
                "wsess_00000000000000000000000000000001",
                path.to_str().unwrap(),
            )
            .unwrap();
        let request = Request::builder()
            .method(Method::GET)
            .uri(format!("{MEDIA_SCHEME}://localhost/{}/content", info.token))
            .header(header::RANGE, "bytes=100-199")
            .body(Vec::new())
            .unwrap();
        let response =
            authority.handle_protocol("wsess_00000000000000000000000000000001", &request);
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.body().len(), 100);
        assert_eq!(
            response.headers().get(header::CONTENT_RANGE).unwrap(),
            format!("bytes 100-199/{}", info.bytes).as_str()
        );

        let invalid = Request::builder()
            .method(Method::GET)
            .uri(format!("{MEDIA_SCHEME}://localhost/{}/content", info.token))
            .header(header::RANGE, format!("bytes={}-", info.bytes + 1))
            .body(Vec::new())
            .unwrap();
        assert_eq!(
            authority
                .handle_protocol("wsess_00000000000000000000000000000001", &invalid)
                .status(),
            StatusCode::RANGE_NOT_SATISFIABLE
        );
    }

    #[test]
    fn requested_ranges_are_capped_to_the_streaming_budget() {
        let start = 123_u64;
        let requested_end = start + MAX_MEDIA_RESPONSE_BYTES * 3;
        let capped = cap_range(start, requested_end);
        assert_eq!(capped.0, start);
        assert_eq!(capped.1 - capped.0 + 1, MAX_MEDIA_RESPONSE_BYTES);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_audio_presentation_has_a_dedicated_platform_boundary() {
        let html = audio_presentation_html();
        assert!(html.contains("<audio"));
        assert!(!html.contains(" controls"));
        assert!(html.contains("src=\"content\""));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_audio_presentation_uses_media_state_as_the_single_timeline_source() {
        let html = audio_presentation_html();
        assert!(html.contains("timeline.max=String(duration)"));
        assert!(html.contains("timeline.value=String(Math.min(position,duration||position))"));
        assert!(html.contains("media.currentTime=Math.min(Math.max(value,0),duration)"));
        assert!(!html.contains("setInterval("));
        assert!(!html.contains("setTimeout("));
        assert!(!html.contains("requestAnimationFrame("));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_audio_presentation_handles_media_lifecycle_states() {
        let html = audio_presentation_html();
        assert!(html.contains("var paused=media.paused||media.ended;icon.innerHTML=paused?"));
        assert!(html.contains("<svg id=\"portus-media-icon\" viewBox=\"0 0 24 24\""));
        assert!(!html.contains(">Play</button>"));
        assert!(!html.contains(">Pause</button>"));
        assert!(html.contains("toggle.setAttribute('aria-label',paused?'Play':'Pause')"));
        assert!(
            html.contains("duration=isFinite(media.duration)&&media.duration>=0?media.duration:0")
        );
        assert!(html.contains("position=isFinite(media.currentTime)&&media.currentTime>=0"));
        assert!(html
            .contains("['loadedmetadata','durationchange','timeupdate','play','pause','ended']"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_audio_presentation_contains_portus_owned_timeline() {
        let html = audio_presentation_html();
        for marker in [
            "id=\"portus-media-timeline\"",
            "id=\"portus-media-time\"",
            "loadedmetadata",
            "durationchange",
            "timeupdate",
            "play",
            "pause",
            "ended",
            "media.currentTime",
        ] {
            assert!(
                html.contains(marker),
                "missing Linux media UI marker: {marker}"
            );
        }
        assert!(!html.contains("file://"));
        assert!(!html.contains("__TAURI__"));
        assert!(!html.contains("invoke("));
    }

    #[test]
    fn video_presentation_uses_portus_owned_fullscreen_control() {
        let html = presentation_html(ContentKind::Video);
        for marker in [
            "id=\"portus-video-toggle\"",
            "aria-label=\"Play\"",
            "id=\"portus-video-timeline\"",
            "id=\"portus-video-volume\"",
            "id=\"portus-video-volume-range\"",
            "id=\"portus-video-fullscreen\"",
            "aria-label=\"Fullscreen\"",
            "portus-window-action://fullscreen",
            "timeupdate",
            "currentTime",
            "volumechange",
        ] {
            assert!(
                html.contains(marker),
                "missing video fullscreen marker: {marker}"
            );
        }
        assert!(
            !html.contains(" controls"),
            "video presentation must not expose native media controls"
        );
        assert!(
            !html.contains("controlsList="),
            "video presentation must not rely on native controls configuration"
        );
        assert!(
            html.contains("controls-hidden"),
            "video presentation must provide an auto-hide control state"
        );
        assert!(
            html.contains("setTimeout(function(){wrap.classList.add('controls-hidden');},2000)"),
            "video presentation must hide controls after inactivity while playing"
        );
    }
    #[test]
    fn presentation_surface_contains_no_filesystem_path_or_script_bridge() {
        for kind in [ContentKind::Image, ContentKind::Video, ContentKind::Audio] {
            let html = presentation_html(kind);
            assert!(html.contains("content"));
            assert!(!html.contains("file://"));
            assert!(!html.contains("__TAURI__"));
            assert!(!html.contains("invoke("));
        }
    }
}
