use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread::JoinHandle;
use thiserror::Error;
use url::Url;
use uuid::Uuid;

const LOCAL_LOOPBACK_ADDR: &str = "127.0.0.1:0";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct YouTubeVideo {
    pub video_id: String,
    pub start_seconds: Option<u64>,
}

impl YouTubeVideo {
    pub fn parse(url: &Url) -> Option<Self> {
        let host = url.host_str()?.to_ascii_lowercase();
        let path = url.path();
        let mut video_id = None;
        let mut start_seconds = None;

        if host == "youtu.be" {
            let segment = path.strip_prefix('/')?;
            let id = segment.split('/').next()?;
            if !id.is_empty() {
                video_id = Some(id.to_string());
            }
        } else if host == "youtube.com"
            || host == "www.youtube.com"
            || host == "m.youtube.com"
            || host == "music.youtube.com"
        {
            if path == "/watch" {
                for (key, value) in url.query_pairs() {
                    if key == "v" && !value.is_empty() {
                        video_id = Some(value.to_string());
                        break;
                    }
                }
            } else if let Some(stripped) = path.strip_prefix("/embed/") {
                let id = stripped.split('/').next()?;
                if !id.is_empty() {
                    video_id = Some(id.to_string());
                }
            } else if let Some(stripped) = path.strip_prefix("/shorts/") {
                let id = stripped.split('/').next()?;
                if !id.is_empty() {
                    video_id = Some(id.to_string());
                }
            } else if let Some(stripped) = path.strip_prefix("/live/") {
                let id = stripped.split('/').next()?;
                if !id.is_empty() {
                    video_id = Some(id.to_string());
                }
            }
        }

        let video_id = video_id?;
        if !is_valid_video_id(&video_id) {
            return None;
        }

        for (key, value) in url.query_pairs() {
            if key == "t" || key == "start" {
                if let Some(parsed) = parse_timestamp(&value) {
                    start_seconds = Some(parsed);
                    break;
                }
            }
        }

        Some(Self {
            video_id,
            start_seconds,
        })
    }

    pub fn thumbnail_url(&self) -> String {
        format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", self.video_id)
    }

    pub fn embed_url(&self, origin: &str) -> String {
        self.embed_url_for_host(origin, "www.youtube-nocookie.com")
    }

    pub fn authenticated_embed_url(&self, origin: &str) -> String {
        self.embed_url_for_host(origin, "www.youtube.com")
    }

    fn embed_url_for_host(&self, origin: &str, host: &str) -> String {
        let mut url = Url::parse(&format!("https://{host}/embed/{}", self.video_id))
            .expect("validated YouTube video ID must produce a valid embed URL");
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("enablejsapi", "1");
            query.append_pair("playsinline", "1");
            query.append_pair("origin", origin);
            if let Some(start) = self.start_seconds {
                query.append_pair("start", &start.to_string());
            }
        }
        url.to_string()
    }
}

fn is_valid_video_id(id: &str) -> bool {
    id.len() == 11
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn parse_timestamp(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(seconds);
    }
    let trimmed = value.strip_suffix('s').unwrap_or(value);
    if let Ok(seconds) = trimmed.parse::<u64>() {
        return Some(seconds);
    }
    let mut total = 0u64;
    let mut current = 0u64;
    let mut matched = false;
    for c in value.chars() {
        if c.is_ascii_digit() {
            current = current
                .checked_mul(10)?
                .checked_add(c.to_digit(10)? as u64)?;
            matched = true;
        } else if c == 'h' && matched {
            total = total.checked_add(current.checked_mul(3600)?)?;
            current = 0;
            matched = false;
        } else if c == 'm' && matched {
            total = total.checked_add(current.checked_mul(60)?)?;
            current = 0;
            matched = false;
        } else if c == 's' && matched {
            total = total.checked_add(current)?;
            current = 0;
            matched = false;
        } else {
            return None;
        }
    }
    if matched {
        total = total.checked_add(current)?;
    }
    Some(total)
}

#[derive(Debug, Error)]
pub enum WebVideoError {
    #[error("web-video validation failed: {0}")]
    Validation(String),
    #[error("web-video service failed: {0}")]
    Service(String),
    #[error("web-video control failed: {0}")]
    Control(String),
}

#[derive(Clone)]
struct VideoPage {
    video: YouTubeVideo,
    embed_url: String,
}

#[derive(Default)]
struct WebVideoRegistry {
    by_window: BTreeMap<String, String>,
    by_token: BTreeMap<String, VideoPage>,
}

pub struct WebVideoRegistration {
    pub token: String,
    pub view_url: Url,
    pub embed_url: String,
}

pub struct WebVideoAuthority {
    address: std::net::SocketAddr,
    registry: Arc<Mutex<WebVideoRegistry>>,
    server_handle: Mutex<Option<JoinHandle<()>>>,
    shutdown: Arc<std::sync::atomic::AtomicBool>,
}
impl WebVideoAuthority {
    pub fn start() -> Result<Arc<Self>, WebVideoError> {
        let listener = TcpListener::bind(LOCAL_LOOPBACK_ADDR).map_err(|error| {
            WebVideoError::Service(format!("could not bind web-video listener: {error}"))
        })?;
        let address = listener.local_addr().map_err(|error| {
            WebVideoError::Service(format!("could not get local address: {error}"))
        })?;
        let registry = Arc::new(Mutex::new(WebVideoRegistry::default()));
        let thread_registry = Arc::clone(&registry);
        let shutdown = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let thread_shutdown = Arc::clone(&shutdown);

        let server_handle = std::thread::Builder::new()
            .name("portus-web-video".to_string())
            .spawn(move || {
                for stream in listener.incoming() {
                    if thread_shutdown.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    match stream {
                        Ok(stream) => {
                            let reg = Arc::clone(&thread_registry);
                            std::thread::spawn(move || {
                                handle_connection(stream, &reg);
                            });
                        }
                        Err(_) => break,
                    }
                }
            })
            .map_err(|error| {
                WebVideoError::Service(format!("could not spawn web-video thread: {error}"))
            })?;

        Ok(Arc::new(Self {
            address,
            registry,
            server_handle: Mutex::new(Some(server_handle)),
            shutdown,
        }))
    }

    pub fn new() -> Result<Arc<Self>, WebVideoError> {
        Self::start()
    }

    pub fn register(
        &self,
        window_session_id: &str,
        video: YouTubeVideo,
    ) -> Result<WebVideoRegistration, WebVideoError> {
        self.register_with_mode(window_session_id, video, false)
    }

    pub fn register_authenticated(
        &self,
        window_session_id: &str,
        video: YouTubeVideo,
    ) -> Result<WebVideoRegistration, WebVideoError> {
        self.register_with_mode(window_session_id, video, true)
    }

    fn register_with_mode(
        &self,
        window_session_id: &str,
        video: YouTubeVideo,
        authenticated: bool,
    ) -> Result<WebVideoRegistration, WebVideoError> {
        let mut registry = self.registry.lock();
        if registry.by_window.contains_key(window_session_id) {
            return Err(WebVideoError::Validation(format!(
                "window '{window_session_id}' already has an active web-video presentation"
            )));
        }
        let token = loop {
            let candidate = Uuid::new_v4().simple().to_string();
            if !registry.by_token.contains_key(&candidate) {
                break candidate;
            }
        };
        let view_url = Url::parse(&format!(
            "http://127.0.0.1:{}/{token}/view",
            self.address.port()
        ))
        .map_err(|error| WebVideoError::Service(format!("invalid loopback view URL: {error}")))?;
        let origin = format!("http://127.0.0.1:{}", self.address.port());
        let embed_url = if authenticated {
            video.authenticated_embed_url(&origin)
        } else {
            video.embed_url(&origin)
        };
        registry
            .by_window
            .insert(window_session_id.to_string(), token.clone());
        registry.by_token.insert(
            token.clone(),
            VideoPage {
                video,
                embed_url: embed_url.clone(),
            },
        );
        Ok(WebVideoRegistration {
            token,
            view_url,
            embed_url,
        })
    }

    pub fn revoke_window(&self, window_session_id: &str) -> bool {
        let mut registry = self.registry.lock();
        if let Some(token) = registry.by_window.remove(window_session_id) {
            registry.by_token.remove(&token);
            true
        } else {
            false
        }
    }

    pub fn contains_window(&self, window_session_id: &str) -> bool {
        self.registry
            .lock()
            .by_window
            .contains_key(window_session_id)
    }

    pub fn is_authenticated_embed(&self, window_session_id: &str) -> Result<bool, WebVideoError> {
        let registry = self.registry.lock();
        let token = registry.by_window.get(window_session_id).ok_or_else(|| {
            WebVideoError::Validation(format!(
                "window '{window_session_id}' is not a web-video presentation"
            ))
        })?;
        let page = registry.by_token.get(token).ok_or_else(|| {
            WebVideoError::Service("web-video registry is inconsistent".to_string())
        })?;
        Ok(page.embed_url.contains("youtube.com/embed")
            && !page.embed_url.contains("youtube-nocookie.com"))
    }

    pub fn view_url_for_window(&self, window_session_id: &str) -> Result<Url, WebVideoError> {
        let registry = self.registry.lock();
        let token = registry.by_window.get(window_session_id).ok_or_else(|| {
            WebVideoError::Validation(format!(
                "window '{window_session_id}' is not a web-video presentation"
            ))
        })?;
        Url::parse(&format!(
            "http://127.0.0.1:{}/{token}/view",
            self.address.port()
        ))
        .map_err(|error| WebVideoError::Service(format!("invalid loopback view URL: {error}")))
    }

    pub fn enable_authenticated_embed(&self, window_session_id: &str) -> Result<(), WebVideoError> {
        let mut registry = self.registry.lock();
        let token = registry
            .by_window
            .get(window_session_id)
            .cloned()
            .ok_or_else(|| {
                WebVideoError::Validation(format!(
                    "window '{window_session_id}' is not a web-video presentation"
                ))
            })?;
        let page = registry.by_token.get_mut(&token).ok_or_else(|| {
            WebVideoError::Service("web-video registry is inconsistent".to_string())
        })?;
        let origin = format!("http://127.0.0.1:{}", self.address.port());
        page.embed_url = page.video.authenticated_embed_url(&origin);
        Ok(())
    }

    pub fn disable_authenticated_embed(
        &self,
        window_session_id: &str,
    ) -> Result<(), WebVideoError> {
        let mut registry = self.registry.lock();
        let token = registry
            .by_window
            .get(window_session_id)
            .cloned()
            .ok_or_else(|| {
                WebVideoError::Validation(format!(
                    "window '{window_session_id}' is not a web-video presentation"
                ))
            })?;
        let page = registry.by_token.get_mut(&token).ok_or_else(|| {
            WebVideoError::Service("web-video registry is inconsistent".to_string())
        })?;
        let origin = format!("http://127.0.0.1:{}", self.address.port());
        page.embed_url = page.video.embed_url(&origin);
        Ok(())
    }
}

impl Drop for WebVideoAuthority {
    fn drop(&mut self) {
        if let Some(handle) = self.server_handle.lock().take() {
            self.shutdown
                .store(true, std::sync::atomic::Ordering::Relaxed);
            let _ = TcpStream::connect(self.address);
            let _ = handle.join();
        }
    }
}

pub fn validate_web_video_navigation(url: &Url, expected_view: &Url) -> bool {
    url.scheme() == "http"
        && url.host_str() == Some("127.0.0.1")
        && url.port() == expected_view.port()
        && url.path() == expected_view.path()
}

fn handle_connection(mut stream: TcpStream, registry: &Arc<Mutex<WebVideoRegistry>>) {
    let mut reader = BufReader::new(&stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() || request_line.is_empty() {
        return;
    }
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        let _ = write_response(
            &mut stream,
            400,
            "Bad Request",
            "text/plain",
            b"Bad Request",
            false,
        );
        return;
    }
    let method = parts[0];
    let path = parts[1];
    if method != "GET" && method != "HEAD" {
        let _ = write_response(
            &mut stream,
            405,
            "Method Not Allowed",
            "text/plain",
            b"Method Not Allowed",
            false,
        );
        return;
    }

    let mut path_segments = path.trim_start_matches('/').split('/');
    let token = path_segments.next().unwrap_or("");
    let action = path_segments.next().unwrap_or("");

    if action != "view" {
        let _ = write_response(
            &mut stream,
            404,
            "Not Found",
            "text/plain",
            b"Not Found",
            method == "HEAD",
        );
        return;
    }

    let page = {
        let reg = registry.lock();
        reg.by_token.get(token).cloned()
    };

    let Some(page) = page else {
        let _ = write_response(
            &mut stream,
            404,
            "Not Found",
            "text/plain",
            b"Not Found",
            method == "HEAD",
        );
        return;
    };

    let body = presentation_html(&page);
    let _ = write_response(
        &mut stream,
        200,
        "OK",
        "text/html; charset=utf-8",
        body.as_bytes(),
        method == "HEAD",
    );
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
    head_only: bool,
) -> Result<(), String> {
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: strict-origin-when-cross-origin\r\nContent-Security-Policy: default-src 'none'; img-src https://i.ytimg.com https://*.ytimg.com https://*.ggpht.com data:; frame-src https://www.youtube-nocookie.com https://www.youtube.com https://youtube.com https://youtube-nocookie.com; script-src 'unsafe-inline' https://www.youtube.com https://s.ytimg.com https://*.youtube.com https://*.ytimg.com; style-src 'unsafe-inline'; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(headers.as_bytes())
        .map_err(|error| format!("failed to write headers: {error}"))?;
    if !head_only {
        stream
            .write_all(body)
            .map_err(|error| format!("failed to write body: {error}"))?;
    }
    stream
        .flush()
        .map_err(|error| format!("failed to flush stream: {error}"))?;
    Ok(())
}

fn presentation_html(page: &VideoPage) -> String {
    let thumbnail = page.video.thumbnail_url();
    let embed = &page.embed_url;
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>YouTube Video</title><style>html,body{{margin:0;width:100%;height:100%;background:#000;color:#fff;font-family:sans-serif}}body{{display:flex;align-items:center;justify-content:center}}#launch{{position:relative;width:100%;height:100%;min-width:200px;min-height:200px;border:0;padding:0;background:#000;cursor:pointer}}#launch img{{width:100%;height:100%;object-fit:contain}}#play{{position:absolute;left:50%;top:50%;transform:translate(-50%,-50%);display:flex;align-items:center;justify-content:center;width:88px;height:62px;border-radius:16px;background:rgba(0,0,0,.78);font-size:34px;line-height:1}}iframe{{width:100%;height:100%;min-width:200px;min-height:200px;border:0}}</style></head><body><button id="launch" type="button" data-embed="{embed}" aria-label="Play YouTube video"><img src="{thumbnail}" alt="YouTube video thumbnail"><span id="play" aria-hidden="true">▶</span></button><script>(()=>{{const button=document.getElementById('launch');const state={{player:null,activated:false,ready:false,autoplayBlocked:false,error:null,queued:[]}};const playback=(value)=>{{switch(value){{case -1:return'unstarted';case 0:return'ended';case 1:return'playing';case 2:return'paused';case 3:return'buffering';case 5:return'cued';default:return'unknown';}}}};const finite=(value)=>Number.isFinite(value)&&value>=0?value:null;const snapshot=()=>{{if(!state.activated)return{{activated:false,ready:false,playback_state:'inactive',duration_seconds:null,position_seconds:null,volume:null,muted:null,autoplay_blocked:state.autoplayBlocked,error:state.error}};if(!state.ready||!state.player)return{{activated:true,ready:false,playback_state:'loading',duration_seconds:null,position_seconds:null,volume:null,muted:null,autoplay_blocked:state.autoplayBlocked,error:state.error}};let volume=state.player.getVolume();volume=Number.isFinite(volume)?Math.min(1,Math.max(0,volume/100)):null;return{{activated:true,ready:true,playback_state:playback(state.player.getPlayerState()),duration_seconds:finite(state.player.getDuration()),position_seconds:finite(state.player.getCurrentTime()),volume,muted:!!state.player.isMuted(),autoplay_blocked:state.autoplayBlocked,error:state.error}};}};const apply=(command,value)=>{{if(!state.ready||!state.player){{state.queued.push([command,value]);return;}}state.error=null;switch(command){{case'play':state.autoplayBlocked=false;state.player.playVideo();break;case'pause':state.player.pauseVideo();break;case'seek':state.player.seekTo(value,true);break;case'mute':state.player.mute();break;case'unmute':state.player.unMute();break;case'set_volume':state.player.setVolume(Math.round(value*100));break;}}}};const activate=()=>{{if(state.activated)return;state.activated=true;const frame=document.createElement('iframe');frame.id='portus-youtube-player';frame.src=button.dataset.embed;frame.title='YouTube video player';frame.allow='autoplay; encrypted-media; picture-in-picture';frame.allowFullscreen=true;frame.referrerPolicy='strict-origin-when-cross-origin';document.body.replaceChildren(frame);window.onYouTubeIframeAPIReady=()=>{{try{{state.player=new YT.Player(frame,{{events:{{onReady:()=>{{state.ready=true;const queued=state.queued.splice(0);for(const [command,value] of queued)apply(command,value);}},onError:(event)=>{{state.error='youtube player error '+String(event.data);}},onAutoplayBlocked:()=>{{state.autoplayBlocked=true;}}}}}});}}catch(error){{state.error=error&&error.message?String(error.message):String(error);}}}};const script=document.createElement('script');script.src='https://www.youtube.com/iframe_api';script.onerror=()=>{{state.error='youtube iframe api failed to load';}};document.head.appendChild(script);}};window.__portusWebVideo={{command:(command,value)=>{{try{{if(command==='state')return snapshot();if(command==='pause'&&!state.activated)return snapshot();if(!state.activated)activate();if(state.ready)apply(command,value);else state.queued.push([command,value]);return snapshot();}}catch(error){{state.error=error&&error.message?String(error.message):String(error);return snapshot();}}}}}};button.addEventListener('click',()=>{{if(!state.activated)activate();if(state.ready)apply('play',null);else state.queued.push(['play',null]);}},{{once:true}});}})();</script></body></html>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(url: &str) -> Option<YouTubeVideo> {
        YouTubeVideo::parse(&Url::parse(url).unwrap())
    }

    #[test]
    fn parse_recognizes_standard_youtube_urls() {
        assert_eq!(
            parse("https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
            Some(YouTubeVideo {
                video_id: "dQw4w9WgXcQ".to_string(),
                start_seconds: None,
            })
        );
        assert_eq!(
            parse("https://youtu.be/dQw4w9WgXcQ?t=42s"),
            Some(YouTubeVideo {
                video_id: "dQw4w9WgXcQ".to_string(),
                start_seconds: Some(42),
            })
        );
        assert_eq!(
            parse("https://m.youtube.com/watch?v=dQw4w9WgXcQ&t=1m30s"),
            Some(YouTubeVideo {
                video_id: "dQw4w9WgXcQ".to_string(),
                start_seconds: Some(90),
            })
        );
    }

    #[test]
    fn parse_rejects_non_video_urls() {
        assert_eq!(parse("https://www.youtube.com/channel/UC1234567890"), None);
        assert_eq!(parse("https://www.youtube.com/feed/trending"), None);
        assert_eq!(parse("https://www.google.com"), None);
        assert_eq!(parse("https://youtu.be/invalid_id"), None);
    }

    #[test]
    fn embed_is_privacy_enhanced_and_lazy_surface_has_no_iframe_at_rest() {
        let video = parse("https://youtu.be/M7lc1UVf-VE?t=90").unwrap();
        let origin = "http://127.0.0.1:43123";
        let embed = video.embed_url(origin);
        let page = VideoPage {
            video: video.clone(),
            embed_url: embed.clone(),
        };
        assert!(embed.starts_with("https://www.youtube-nocookie.com/embed/M7lc1UVf-VE?"));
        assert!(embed.contains("enablejsapi=1"));
        assert!(embed.contains("origin=http%3A%2F%2F127.0.0.1%3A43123"));
        assert!(!embed.contains("autoplay=1"));
        assert!(embed.contains("start=90"));
        let html = presentation_html(&page);
        assert!(html.contains("i.ytimg.com/vi/M7lc1UVf-VE/hqdefault.jpg"));
        assert!(html.contains("data-embed="));
        assert!(!html.contains("<iframe"));
    }

    #[test]
    fn authority_registers_and_revokes_windows() {
        let authority = WebVideoAuthority::start().unwrap();
        let video = parse("https://www.youtube.com/watch?v=M7lc1UVf-VE").unwrap();
        let reg = authority
            .register("wsess_00000000000000000000000000000001", video.clone())
            .unwrap();
        assert_eq!(reg.view_url.scheme(), "http");
        assert_eq!(reg.view_url.host_str(), Some("127.0.0.1"));
        assert!(!authority
            .is_authenticated_embed("wsess_00000000000000000000000000000001")
            .unwrap());

        authority
            .enable_authenticated_embed("wsess_00000000000000000000000000000001")
            .unwrap();
        assert!(authority
            .is_authenticated_embed("wsess_00000000000000000000000000000001")
            .unwrap());

        authority
            .disable_authenticated_embed("wsess_00000000000000000000000000000001")
            .unwrap();
        assert!(!authority
            .is_authenticated_embed("wsess_00000000000000000000000000000001")
            .unwrap());

        assert!(authority.contains_window("wsess_00000000000000000000000000000001"));
        assert!(authority.revoke_window("wsess_00000000000000000000000000000001"));
        assert!(!authority.contains_window("wsess_00000000000000000000000000000001"));
    }

    #[test]
    fn presentation_is_lazy_and_exposes_only_bounded_player_bridge() {
        let page = VideoPage {
            video: YouTubeVideo {
                video_id: "dQw4w9WgXcQ".to_string(),
                start_seconds: None,
            },
            embed_url: "https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ?enablejsapi=1"
                .to_string(),
        };
        let html = presentation_html(&page);
        assert!(html.contains("__portusWebVideo"));
        assert!(html.contains("youtube-nocookie.com"));
        assert!(html.contains("youtube.com/iframe_api"));
        assert!(!html.contains("setPlaybackQuality"));
        assert!(!html.contains("<iframe id=\"portus-youtube-player\""));
    }
}
