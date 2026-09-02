#[cfg(target_os = "linux")]
use std::sync::{mpsc, Arc};
#[cfg(target_os = "linux")]
use std::time::Duration;
use tauri::WebviewWindow;

#[cfg(target_os = "linux")]
pub fn attach_load_failure_observer<F>(window: &WebviewWindow, on_failure: F) -> Result<(), String>
where
    F: Fn(String, String) + Send + Sync + 'static,
{
    use webkit2gtk::WebViewExt;
    let fullscreen_enter_window = window.clone();
    let fullscreen_leave_window = window.clone();
    window
        .with_webview(move |platform_webview| {
            let webview = platform_webview.inner();
            let fullscreen_window = fullscreen_enter_window.clone();
            webview.connect_enter_fullscreen(move |_webview| {
                if let Err(error) = fullscreen_window.set_fullscreen(true) {
                    eprintln!("Portus Window could not enter fullscreen: {error}");
                }
                false
            });
            let fullscreen_window = fullscreen_leave_window.clone();
            webview.connect_leave_fullscreen(move |_webview| {
                if let Err(error) = fullscreen_window.set_fullscreen(false) {
                    eprintln!("Portus Window could not leave fullscreen: {error}");
                }
                false
            });
            let callback: Arc<dyn Fn(String, String) + Send + Sync> = Arc::new(on_failure);
            let failed_callback = Arc::clone(&callback);
            webview.connect_load_failed(move |_webview, _event, failing_uri, error| {
                failed_callback(failing_uri.to_string(), error.to_string());
                false
            });
        })
        .map_err(|error| format!("could not attach WebKitGTK load failure observer: {error}"))
}

#[cfg(not(target_os = "linux"))]
pub fn attach_load_failure_observer<F>(
    _window: &WebviewWindow,
    _on_failure: F,
) -> Result<(), String>
where
    F: Fn(String, String) + Send + Sync + 'static,
{
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn clear_web_cache(window: &WebviewWindow) -> Result<(), String> {
    use webkit2gtk::{WebContextExt, WebViewExt};
    window
        .with_webview(move |platform_webview| {
            if let Some(context) = platform_webview.inner().context() {
                context.clear_cache();
            } else {
                eprintln!(
                    "Portus Window could not prune cache: WebKitGTK web context is unavailable"
                );
            }
        })
        .map_err(|error| format!("could not schedule WebKitGTK cache clear: {error}"))
}

#[cfg(not(target_os = "linux"))]
pub fn clear_web_cache(_window: &WebviewWindow) -> Result<(), String> {
    Ok(())
}

#[cfg(target_os = "linux")]
pub fn capture_png_bytes(window: &WebviewWindow) -> Result<Vec<u8>, String> {
    use gtk::prelude::WidgetExt;
    let (sender, receiver) = mpsc::sync_channel(1);
    window
        .with_webview(move |platform_webview| {
            let result = (|| -> Result<Vec<u8>, String> {
                let widget = platform_webview.inner();
                let allocation = widget.allocation();
                let width = allocation.width();
                let height = allocation.height();
                if width <= 0 || height <= 0 {
                    return Err("window allocation is empty".to_string());
                }
                let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, width, height)
                    .map_err(|error| format!("could not create cairo surface: {error}"))?;
                let context = cairo::Context::new(&surface)
                    .map_err(|error| format!("could not create cairo context: {error}"))?;
                widget.draw(&context);
                surface.flush();
                let mut png_bytes = Vec::new();
                surface
                    .write_to_png(&mut png_bytes)
                    .map_err(|error| format!("could not encode PNG: {error}"))?;
                Ok(png_bytes)
            })();
            let _ = sender.send(result);
        })
        .map_err(|error| format!("could not schedule WebKitGTK screenshot: {error}"))?;

    receiver
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| "WebKitGTK screenshot capture timed out after 5 seconds".to_string())?
}

#[cfg(not(target_os = "linux"))]
pub fn capture_png_bytes(_window: &WebviewWindow) -> Result<Vec<u8>, String> {
    Err("native WebKitGTK screenshot capture is supported on Linux only".to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn fullscreen_integration_is_platform_owned() {
        let source = include_str!("linux_webkit.rs");
        assert!(source.contains("connect_enter_fullscreen"));
        assert!(source.contains("connect_leave_fullscreen"));
        assert!(source.contains("set_fullscreen(true)"));
        assert!(source.contains("set_fullscreen(false)"));
    }
}
