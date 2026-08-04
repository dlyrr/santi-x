//! The xcap layer: screen/window grabbing plus the pure image helpers that sit
//! on top of it. Deliberately free of Tauri types so it can be exercised on its
//! own.

use std::io::Cursor;

use image::codecs::png::{CompressionType, FilterType as PngFilterType, PngEncoder};
use image::{ExtendedColorType, ImageEncoder, RgbaImage};
use serde::Serialize;
use xcap::{Monitor, Window};

/// Longest edge of a generated thumbnail, in pixels.
const THUMB_MAX_EDGE: u32 = 480;

/// Guard rails so a driver reporting a nonsense size cannot make us attempt a
/// multi-gigabyte allocation. ~268 Mpx is ~1 GiB as RGBA8, comfortably above any
/// real multi-monitor desktop.
const MAX_EDGE: u32 = 65_535;
const MAX_PIXELS: u64 = 1 << 28;

/// A captured image plus where its top-left sits in xcap (physical pixel) space.
pub struct Shot {
    pub image: RgbaImage,
    pub origin_x: i32,
    pub origin_y: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorInfo {
    pub id: u32,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
    pub scale_factor: f32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    pub id: u32,
    pub title: String,
    pub app_name: String,
    pub width: u32,
    pub height: u32,
}

/// A window's on-screen geometry in xcap space, for the overlay's hover
/// auto-detect (M2.5 §2). Higher `z` is nearer the front.
///
/// These ship inside `ArmPayload`, enumerated in the same instant as the freeze
/// frame — a list fetched *after* arming would describe a desktop the frozen
/// pixels no longer match.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowRect {
    pub id: u32,
    pub title: String,
    pub app_name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub z: i32,
}

/// Smallest edge worth offering as a hover target. Below this it is a splitter,
/// a drop shadow host or a stray 1px message window, never something anyone
/// means to capture.
const MIN_WINDOW_EDGE: u32 = 16;

// ---------------------------------------------------------------------------
// enumeration
// ---------------------------------------------------------------------------

pub fn list_monitors() -> Result<Vec<MonitorInfo>, String> {
    let monitors = Monitor::all().map_err(|e| format!("enumerating monitors failed: {e}"))?;

    let mut out = Vec::with_capacity(monitors.len());
    let mut first_err: Option<String> = None;

    for (i, m) in monitors.iter().enumerate() {
        match monitor_info(m, i) {
            Ok(info) => out.push(info),
            Err(e) => {
                let _ = first_err.get_or_insert(e);
            }
        }
    }

    // A single flaky display should not blank the picker, but if nothing at all
    // came back the caller deserves the reason.
    if out.is_empty() {
        if let Some(e) = first_err {
            return Err(e);
        }
    }
    Ok(out)
}

pub fn list_windows() -> Result<Vec<WindowInfo>, String> {
    let windows = Window::all().map_err(|e| format!("enumerating windows failed: {e}"))?;

    let mut out = Vec::new();
    for w in &windows {
        let (Ok(id), Ok(title)) = (w.id(), w.title()) else {
            continue;
        };
        if title.trim().is_empty() || w.is_minimized().unwrap_or(true) {
            continue;
        }
        let (Ok(width), Ok(height)) = (w.width(), w.height()) else {
            continue;
        };
        if width == 0 || height == 0 {
            continue;
        }
        out.push(WindowInfo {
            id,
            title,
            app_name: w.app_name().unwrap_or_default(),
            width,
            height,
        });
    }
    Ok(out)
}

/// Every capturable window with its rect, topmost first.
///
/// `Window::all()` already drops invisible, cloaked, tool-window and empty-rect
/// handles, and comes back in Z order with the frontmost window at index 0.
/// Everything below is the extra filtering the overlay needs.
pub fn list_window_rects() -> Result<Vec<WindowRect>, String> {
    let windows = Window::all().map_err(|e| format!("enumerating windows failed: {e}"))?;
    let own_pid = std::process::id();
    let count = windows.len();

    let mut out = Vec::with_capacity(count);
    for (index, w) in windows.iter().enumerate() {
        // Our own windows go first, and by pid rather than by title: the
        // overlay is always-on-top, so left in the list it would be the topmost
        // hit under every cursor position. A window whose pid will not read is
        // dropped too — better to lose one hover target than to offer ours.
        if w.pid().map(|pid| pid == own_pid).unwrap_or(true) {
            continue;
        }
        let (Ok(id), Ok(title)) = (w.id(), w.title()) else {
            continue;
        };
        if title.trim().is_empty() || w.is_minimized().unwrap_or(true) {
            continue;
        }
        let (Ok(x), Ok(y), Ok(width), Ok(height)) = (w.x(), w.y(), w.width(), w.height()) else {
            continue;
        };
        if width < MIN_WINDOW_EDGE
            || height < MIN_WINDOW_EDGE
            || width > MAX_EDGE
            || height > MAX_EDGE
        {
            continue;
        }

        out.push(WindowRect {
            id,
            title,
            // Last, and only for windows that survived every other filter:
            // `app_name` opens the owning process and reads its version
            // resource, which is by far the most expensive call in this loop and
            // this loop sits on the hotkey's critical path.
            app_name: w.app_name().unwrap_or_default(),
            x,
            y,
            width,
            height,
            // Derived from the enumeration index rather than `Window::z()`,
            // which runs a fresh EnumWindows per call — O(n^2) for the same
            // answer. Same formula xcap uses, so the sense matches: bigger is
            // nearer the front.
            z: (count - 1 - index) as i32,
        });
    }

    // Already in this order, but hit-testing takes the first match and that
    // contract should not rest on an implementation detail of xcap.
    out.sort_by_key(|w| std::cmp::Reverse(w.z));
    Ok(out)
}

// ---------------------------------------------------------------------------
// capture
// ---------------------------------------------------------------------------

/// Every monitor composited into one image spanning the whole virtual desktop.
pub fn capture_virtual_desktop() -> Result<Shot, String> {
    let monitors = Monitor::all().map_err(|e| format!("enumerating monitors failed: {e}"))?;
    if monitors.is_empty() {
        return Err("no monitors found".into());
    }

    let mut first_err: Option<String> = None;
    let mut rects: Vec<(usize, i32, i32, u32, u32)> = Vec::with_capacity(monitors.len());

    for (i, m) in monitors.iter().enumerate() {
        match monitor_rect(m, i) {
            Ok((x, y, w, h)) => rects.push((i, x, y, w, h)),
            Err(e) => {
                let _ = first_err.get_or_insert(e);
            }
        }
    }
    if rects.is_empty() {
        return Err(first_err.unwrap_or_else(|| "no monitor reported a usable geometry".into()));
    }

    // Reported dimensions exist only to size and place the canvas; the captured
    // images are authoritative for actual pixels, and `replace` clips anything
    // that disagrees.
    let min_x = rects.iter().map(|r| r.1).min().unwrap_or(0);
    let min_y = rects.iter().map(|r| r.2).min().unwrap_or(0);
    let max_x = rects
        .iter()
        .map(|r| r.1.saturating_add(r.3 as i32))
        .max()
        .unwrap_or(0);
    let max_y = rects
        .iter()
        .map(|r| r.2.saturating_add(r.4 as i32))
        .max()
        .unwrap_or(0);

    let width = (max_x - min_x).max(0) as u32;
    let height = (max_y - min_y).max(0) as u32;
    validate_dims(width, height, "virtual desktop")?;

    // Zero-filled, so gaps in a non-rectangular monitor arrangement stay transparent.
    let mut canvas = RgbaImage::new(width, height);
    let mut placed = 0usize;

    for &(i, x, y, _, _) in &rects {
        match monitors[i].capture_image() {
            Ok(img) => {
                image::imageops::replace(&mut canvas, &img, (x - min_x) as i64, (y - min_y) as i64);
                placed += 1;
            }
            Err(e) => {
                let _ = first_err.get_or_insert(format!("monitor {i}: capture failed: {e}"));
            }
        }
    }

    if placed == 0 {
        return Err(first_err.unwrap_or_else(|| "every monitor failed to capture".into()));
    }

    Ok(Shot {
        image: canvas,
        origin_x: min_x,
        origin_y: min_y,
    })
}

pub fn capture_monitor_by_id(id: u32) -> Result<Shot, String> {
    let monitors = Monitor::all().map_err(|e| format!("enumerating monitors failed: {e}"))?;

    let (index, monitor) = monitors
        .iter()
        .enumerate()
        .find(|(_, m)| m.id().map(|got| got == id).unwrap_or(false))
        .ok_or_else(|| format!("no monitor with id {id}"))?;

    let (x, y, w, h) = monitor_rect(monitor, index)?;
    validate_dims(w, h, &format!("monitor {id}"))?;

    let image = monitor
        .capture_image()
        .map_err(|e| format!("monitor {id}: capture failed: {e}"))?;

    Ok(Shot {
        image,
        origin_x: x,
        origin_y: y,
    })
}

pub fn capture_focused_window() -> Result<Shot, String> {
    let windows = Window::all().map_err(|e| format!("enumerating windows failed: {e}"))?;

    let focused = windows.iter().find(|w| {
        w.is_focused().unwrap_or(false)
            && !w.is_minimized().unwrap_or(true)
            && w.title().map(|t| !t.trim().is_empty()).unwrap_or(false)
    });

    match focused {
        Some(w) => shoot_window(w, "focused window"),
        None => Err("no focused window to capture (it may be minimized or untitled)".into()),
    }
}

pub fn capture_window_by_id(id: u32) -> Result<Shot, String> {
    let windows = Window::all().map_err(|e| format!("enumerating windows failed: {e}"))?;

    let window = windows
        .iter()
        .find(|w| w.id().map(|got| got == id).unwrap_or(false))
        .ok_or_else(|| format!("no window with id {id}"))?;

    shoot_window(window, &format!("window {id}"))
}

// ---------------------------------------------------------------------------
// image helpers
// ---------------------------------------------------------------------------

/// Downscale so the longest edge is at most 480px. Smaller images pass through
/// untouched — a thumbnail is never an upscale.
pub fn thumbnail(img: &RgbaImage) -> RgbaImage {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 || (w <= THUMB_MAX_EDGE && h <= THUMB_MAX_EDGE) {
        return img.clone();
    }

    let scale = f64::from(THUMB_MAX_EDGE) / f64::from(w.max(h));
    let tw = ((f64::from(w) * scale).round() as u32).max(1);
    let th = ((f64::from(h) * scale).round() as u32).max(1);
    image::imageops::thumbnail(img, tw, th)
}

/// Minimal-compression PNG for the freeze frame. A 4K desktop has to get through
/// this in well under a second, so no filtering and the fastest deflate level.
pub fn encode_png_fast(img: &RgbaImage) -> Result<Vec<u8>, String> {
    encode(img, CompressionType::Fast, PngFilterType::NoFilter)
}

/// Default-compression PNG, for images that get written to `save_dir` and kept.
pub fn encode_png(img: &RgbaImage) -> Result<Vec<u8>, String> {
    encode(img, CompressionType::Default, PngFilterType::Adaptive)
}

/// Crop, clamped to the source. The rect must start inside the image and enclose
/// at least one pixel.
pub fn crop(img: &RgbaImage, x: u32, y: u32, w: u32, h: u32) -> Result<RgbaImage, String> {
    let (iw, ih) = img.dimensions();
    if x >= iw || y >= ih {
        return Err(format!(
            "crop origin {x},{y} lies outside the {iw}x{ih} image"
        ));
    }

    let cw = w.min(iw - x);
    let ch = h.min(ih - y);
    if cw == 0 || ch == 0 {
        return Err(format!(
            "crop {w}x{h} at {x},{y} has zero area within the {iw}x{ih} image"
        ));
    }

    Ok(image::imageops::crop_imm(img, x, y, cw, ch).to_image())
}

// ---------------------------------------------------------------------------
// internals
// ---------------------------------------------------------------------------

fn encode(
    img: &RgbaImage,
    compression: CompressionType,
    filter: PngFilterType,
) -> Result<Vec<u8>, String> {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err(format!("cannot encode a {w}x{h} image"));
    }
    validate_dims(w, h, "image")?;

    // Roughly a byte per pixel: enough that the hot path is not spent growing
    // the buffer, without reserving the full RGBA size.
    let guess = (w as usize).saturating_mul(h as usize).saturating_add(1024);
    let mut cursor = Cursor::new(Vec::with_capacity(guess));

    PngEncoder::new_with_quality(&mut cursor, compression, filter)
        .write_image(img.as_raw(), w, h, ExtendedColorType::Rgba8)
        .map_err(|e| format!("PNG encode of {w}x{h} failed: {e}"))?;

    Ok(cursor.into_inner())
}

fn shoot_window(window: &Window, label: &str) -> Result<Shot, String> {
    let w = window
        .width()
        .map_err(|e| format!("{label}: width() failed: {e}"))?;
    let h = window
        .height()
        .map_err(|e| format!("{label}: height() failed: {e}"))?;
    validate_dims(w, h, label)?;

    let x = window
        .x()
        .map_err(|e| format!("{label}: x() failed: {e}"))?;
    let y = window
        .y()
        .map_err(|e| format!("{label}: y() failed: {e}"))?;

    let image = window
        .capture_image()
        .map_err(|e| format!("{label}: capture failed: {e}"))?;

    let (iw, ih) = image.dimensions();
    if iw == 0 || ih == 0 {
        return Err(format!(
            "{label}: capture returned an empty {iw}x{ih} image"
        ));
    }

    Ok(Shot {
        image,
        origin_x: x,
        origin_y: y,
    })
}

fn monitor_rect(m: &Monitor, index: usize) -> Result<(i32, i32, u32, u32), String> {
    let x = m
        .x()
        .map_err(|e| format!("monitor {index}: x() failed: {e}"))?;
    let y = m
        .y()
        .map_err(|e| format!("monitor {index}: y() failed: {e}"))?;
    let w = m
        .width()
        .map_err(|e| format!("monitor {index}: width() failed: {e}"))?;
    let h = m
        .height()
        .map_err(|e| format!("monitor {index}: height() failed: {e}"))?;

    validate_dims(w, h, &format!("monitor {index}"))?;
    Ok((x, y, w, h))
}

fn monitor_info(m: &Monitor, index: usize) -> Result<MonitorInfo, String> {
    let (x, y, width, height) = monitor_rect(m, index)?;
    Ok(MonitorInfo {
        id: m
            .id()
            .map_err(|e| format!("monitor {index}: id() failed: {e}"))?,
        name: monitor_label(m, index),
        x,
        y,
        width,
        height,
        is_primary: m.is_primary().unwrap_or(false),
        scale_factor: m.scale_factor().unwrap_or(1.0),
    })
}

fn monitor_label(m: &Monitor, index: usize) -> String {
    for candidate in [m.friendly_name(), m.name()].into_iter().flatten() {
        if !candidate.trim().is_empty() {
            return candidate;
        }
    }
    format!("Display {}", index + 1)
}

fn validate_dims(w: u32, h: u32, what: &str) -> Result<(), String> {
    if w == 0 || h == 0 {
        return Err(format!("{what} reported an empty {w}x{h} area"));
    }
    if w > MAX_EDGE || h > MAX_EDGE || u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err(format!("{what} reported an implausible {w}x{h} area"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    fn solid(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba([10, 20, 30, 255]))
    }

    #[test]
    fn thumbnail_fits_the_long_edge_and_keeps_aspect() {
        let t = thumbnail(&solid(3840, 2160));
        assert_eq!(t.width(), THUMB_MAX_EDGE);
        assert_eq!(t.height(), 270);
    }

    #[test]
    fn thumbnail_never_upscales() {
        let t = thumbnail(&solid(64, 32));
        assert_eq!(t.dimensions(), (64, 32));
    }

    #[test]
    fn crop_clamps_to_the_source() {
        let c = crop(&solid(100, 80), 90, 70, 50, 50).expect("clamped crop");
        assert_eq!(c.dimensions(), (10, 10));
    }

    #[test]
    fn crop_rejects_zero_area_and_out_of_bounds() {
        assert!(crop(&solid(100, 80), 0, 0, 0, 10).is_err());
        assert!(crop(&solid(100, 80), 100, 0, 10, 10).is_err());
    }

    #[test]
    fn both_encoders_emit_a_png_signature() {
        let img = solid(32, 16);
        for bytes in [
            encode_png_fast(&img).expect("fast"),
            encode_png(&img).expect("default"),
        ] {
            assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        }
    }
}
