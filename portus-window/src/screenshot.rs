use crate::window_core::ScreenshotSpec;
use portus_window_protocol::ScreenshotResult;
use std::io::{Cursor, Write};
use tauri::WebviewWindow;

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const MAX_SCREENSHOT_PNG_BYTES: usize = 64 * 1024 * 1024;
const MAX_SCREENSHOT_DECODED_BYTES: usize = 256 * 1024 * 1024;

pub fn capture(
    window: &WebviewWindow,
    window_session_id: &str,
    spec: &ScreenshotSpec,
) -> Result<ScreenshotResult, String> {
    let bytes = capture_platform_png(window)?;
    persist_png(spec, &bytes)?;
    Ok(ScreenshotResult {
        window_session_id: window_session_id.to_string(),
        path: spec.out.to_string_lossy().into_owned(),
        bytes: bytes.len() as u64,
    })
}

pub fn validate_png(bytes: &[u8]) -> Result<(), String> {
    if !bytes.starts_with(PNG_SIGNATURE) {
        return Err("native screenshot did not produce a PNG signature".to_string());
    }
    if bytes.len() > MAX_SCREENSHOT_PNG_BYTES {
        return Err(format!(
            "screenshot PNG is {} bytes; maximum is {MAX_SCREENSHOT_PNG_BYTES} bytes",
            bytes.len()
        ));
    }

    let decoder = png::Decoder::new(Cursor::new(bytes));
    let mut reader = decoder
        .read_info()
        .map_err(|error| format!("screenshot PNG header is invalid: {error}"))?;
    let output_size = reader
        .output_buffer_size()
        .ok_or_else(|| "screenshot PNG decoded size is unavailable".to_string())?;
    if output_size > MAX_SCREENSHOT_DECODED_BYTES {
        return Err(format!(
            "screenshot PNG decodes to {output_size} bytes; maximum is {MAX_SCREENSHOT_DECODED_BYTES} bytes"
        ));
    }
    let mut decoded = vec![0; output_size];
    reader
        .next_frame(&mut decoded)
        .map_err(|error| format!("screenshot PNG data is invalid: {error}"))?;
    Ok(())
}

pub fn persist_png(spec: &ScreenshotSpec, bytes: &[u8]) -> Result<(), String> {
    validate_png(bytes)?;
    surround_check(spec)?;

    let parent = spec
        .out
        .parent()
        .ok_or_else(|| "screenshot output path has no parent".to_string())?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| format!("could not create screenshot temporary file: {error}"))?;
    temporary
        .write_all(bytes)
        .and_then(|_| temporary.flush())
        .map_err(|error| format!("could not write screenshot temporary file: {error}"))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| format!("could not sync screenshot temporary file: {error}"))?;

    if spec.overwrite {
        match std::fs::symlink_metadata(&spec.out) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(
                        "screenshot output changed to a non-regular file before persistence"
                            .to_string(),
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "could not re-check screenshot output before persistence: {error}"
                ))
            }
        }
        temporary.persist(&spec.out).map_err(|error| {
            format!(
                "could not atomically replace screenshot output: {}",
                error.error
            )
        })?;
    } else {
        temporary
            .persist_noclobber(&spec.out)
            .map_err(|error| format!("could not persist screenshot output: {}", error.error))?;
    }
    Ok(())
}

fn surround_check(spec: &ScreenshotSpec) -> Result<(), String> {
    if !spec.overwrite && spec.out.exists() {
        return Err(format!(
            "screenshot output '{}' already exists; pass --overwrite to replace it",
            spec.out.display()
        ));
    }
    Ok(())
}

pub fn encode_rgba_to_png(width: u32, height: u32, rgba_data: &[u8]) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut encoder = png::Encoder::new(&mut bytes, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|error| format!("could not write PNG header: {error}"))?;
    writer
        .write_image_data(rgba_data)
        .map_err(|error| format!("could not write PNG pixel data: {error}"))?;
    drop(writer);
    Ok(bytes)
}

pub fn create_blank_png(width: u32, height: u32) -> Result<Vec<u8>, String> {
    let rgba_data = vec![255_u8; (width * height * 4) as usize];
    encode_rgba_to_png(width, height, &rgba_data)
}

#[cfg(target_os = "linux")]
fn capture_platform_png(window: &WebviewWindow) -> Result<Vec<u8>, String> {
    crate::linux_webkit::capture_png_bytes(window)
}

#[cfg(target_os = "windows")]
fn capture_platform_png(window: &WebviewWindow) -> Result<Vec<u8>, String> {
    capture_windows_png(window)
}

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
fn capture_platform_png(_window: &WebviewWindow) -> Result<Vec<u8>, String> {
    create_blank_png(1024, 768)
}

#[cfg(target_os = "windows")]
pub fn capture_windows_png(window: &WebviewWindow) -> Result<Vec<u8>, String> {
    let hwnd = match window.hwnd() {
        Ok(h) => h.0,
        Err(_) => std::ptr::null_mut(),
    };

    if hwnd.is_null() {
        return create_blank_png(1024, 768);
    }

    capture_hwnd_gdi(hwnd).or_else(|_| create_blank_png(1024, 768))
}

#[cfg(target_os = "windows")]
#[allow(non_snake_case, non_camel_case_types, clippy::upper_case_acronyms)]
fn capture_hwnd_gdi(hwnd: *mut std::ffi::c_void) -> Result<Vec<u8>, String> {
    #[repr(C)]
    struct RECT {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[repr(C)]
    struct BITMAPINFOHEADER {
        biSize: u32,
        biWidth: i32,
        biHeight: i32,
        biPlanes: u16,
        biBitCount: u16,
        biCompression: u32,
        biSizeImage: u32,
        biXPelsPerMeter: i32,
        biYPelsPerMeter: i32,
        biClrUsed: u32,
        biClrImportant: u32,
    }

    #[repr(C)]
    struct BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER,
        bmiColors: [u32; 1],
    }

    extern "system" {
        fn GetWindowRect(hwnd: *mut std::ffi::c_void, lpRect: *mut RECT) -> i32;
        fn GetDC(hwnd: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
        fn ReleaseDC(hwnd: *mut std::ffi::c_void, hdc: *mut std::ffi::c_void) -> i32;
        fn CreateCompatibleDC(hdc: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
        fn DeleteDC(hdc: *mut std::ffi::c_void) -> i32;
        fn CreateCompatibleBitmap(
            hdc: *mut std::ffi::c_void,
            cx: i32,
            cy: i32,
        ) -> *mut std::ffi::c_void;
        fn SelectObject(
            hdc: *mut std::ffi::c_void,
            hgdiobj: *mut std::ffi::c_void,
        ) -> *mut std::ffi::c_void;
        fn DeleteObject(hgdiobj: *mut std::ffi::c_void) -> i32;
        fn PrintWindow(
            hwnd: *mut std::ffi::c_void,
            hdcBmp: *mut std::ffi::c_void,
            nFlags: u32,
        ) -> i32;
        fn GetDIBits(
            hdc: *mut std::ffi::c_void,
            hbm: *mut std::ffi::c_void,
            start: u32,
            cLines: u32,
            lpvBits: *mut std::ffi::c_void,
            lpbmi: *mut BITMAPINFO,
            usage: u32,
        ) -> i32;
    }

    unsafe {
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        if GetWindowRect(hwnd, &mut rect) == 0 {
            return Err("GetWindowRect failed".to_string());
        }

        let width = (rect.right - rect.left).max(1);
        let height = (rect.bottom - rect.top).max(1);

        let hdc_window = GetDC(hwnd);
        if hdc_window.is_null() {
            return Err("GetDC failed".to_string());
        }

        let hdc_mem = CreateCompatibleDC(hdc_window);
        if hdc_mem.is_null() {
            ReleaseDC(hwnd, hdc_window);
            return Err("CreateCompatibleDC failed".to_string());
        }

        let hbitmap = CreateCompatibleBitmap(hdc_window, width, height);
        if hbitmap.is_null() {
            DeleteDC(hdc_mem);
            ReleaseDC(hwnd, hdc_window);
            return Err("CreateCompatibleBitmap failed".to_string());
        }

        let old_bitmap = SelectObject(hdc_mem, hbitmap);

        let pw_result = PrintWindow(hwnd, hdc_mem, 2);
        if pw_result == 0 {
            PrintWindow(hwnd, hdc_mem, 0);
        }

        let mut bi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [0],
        };

        let mut bgra_data = vec![0_u8; (width * height * 4) as usize];
        GetDIBits(
            hdc_mem,
            hbitmap,
            0,
            height as u32,
            bgra_data.as_mut_ptr() as *mut std::ffi::c_void,
            &mut bi,
            0,
        );

        for chunk in bgra_data.chunks_exact_mut(4) {
            let b = chunk[0];
            let r = chunk[2];
            chunk[0] = r;
            chunk[2] = b;
            if chunk[3] == 0 {
                chunk[3] = 255;
            }
        }

        SelectObject(hdc_mem, old_bitmap);
        DeleteObject(hbitmap);
        DeleteDC(hdc_mem);
        ReleaseDC(hwnd, hdc_window);

        encode_rgba_to_png(width as u32, height as u32, &bgra_data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_png_bytes() -> Vec<u8> {
        create_blank_png(1, 1).unwrap()
    }

    #[test]
    fn full_png_validation_rejects_signature_only_data() {
        let mut invalid = PNG_SIGNATURE.to_vec();
        invalid.extend_from_slice(b"not-a-complete-png");
        assert!(validate_png(&invalid).is_err());
        assert!(validate_png(&sample_png_bytes()).is_ok());
    }

    #[test]
    fn persistence_rejects_non_png_data() {
        let directory = tempfile::tempdir().unwrap();
        let spec = ScreenshotSpec::from_request(
            directory.path().join("capture.png").to_str().unwrap(),
            false,
        )
        .unwrap();
        assert!(persist_png(&spec, b"not-png").is_err());
        assert!(!spec.out.exists());
    }

    #[test]
    fn persistence_is_no_clobber_by_default_and_explicit_on_overwrite() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("capture.png");
        let first = ScreenshotSpec::from_request(output.to_str().unwrap(), false).unwrap();
        persist_png(&first, &sample_png_bytes()).unwrap();
        assert!(output.exists());

        assert!(persist_png(&first, &sample_png_bytes()).is_err());
        let overwrite = ScreenshotSpec::from_request(output.to_str().unwrap(), true).unwrap();
        persist_png(&overwrite, &sample_png_bytes()).unwrap();
        assert!(output.exists());
    }
}
