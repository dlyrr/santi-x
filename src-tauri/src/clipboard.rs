//! The native Win32 clipboard path, used when the plugin's fails (M2.7 §1).
//!
//! `tauri-plugin-clipboard-manager` reaches `arboard::Clipboard::set_image`
//! through one `arboard::Clipboard` created at plugin init and reused, behind a
//! mutex, for the whole process lifetime. On Windows the clipboard is a shared
//! resource exactly one process may hold open at a time, so a stale handle or a
//! lost race with another app comes back as a hard error with no retry behind
//! it. This module opens the clipboard itself, retries when someone else has it,
//! and writes the two formats that matter.
//!
//! # Ownership
//!
//! `SetClipboardData` *takes* the `HGLOBAL` on success — the clipboard frees it
//! when it is next emptied. Freeing it ourselves afterwards is a use-after-free
//! that surfaces as a crash in whichever app pastes next, so the block is only
//! freed on the paths where the handoff never happened.

use std::sync::OnceLock;
use std::time::Duration;

use image::RgbaImage;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL};
use windows::Win32::Graphics::Gdi::{BITMAPV5HEADER, BI_BITFIELDS, LCS_GM_IMAGES};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

use crate::capture;

/// `CF_DIBV5`. The `windows` crate keeps the `CF_*` constants behind
/// `Win32_System_Ole`, which drags the entire COM surface in for one number, and
/// `SetClipboardData` takes a plain `u32` format anyway.
const CF_DIBV5: u32 = 17;

/// `LCS_sRGB` — the `'sRGB'` FOURCC, stored big-endian as the header wants it.
/// Tags the colour space so consumers do not apply a gamma we never encoded.
const LCS_SRGB: u32 = 0x7352_4742;

/// `"PNG"`, NUL-terminated, as UTF-16. A `const` rather than a built string so
/// there is no temporary whose lifetime has to outlive the `PCWSTR` pointing at
/// it.
const PNG_FORMAT_NAME: &[u16] = &[b'P' as u16, b'N' as u16, b'G' as u16, 0];

/// Losing the clipboard for a few milliseconds while another app writes to it is
/// normal, not a failure. Eight tries at 25ms covers ~200ms of contention, which
/// is far longer than any well-behaved app holds it.
const OPEN_ATTEMPTS: u32 = 8;
const OPEN_BACKOFF: Duration = Duration::from_millis(25);

// ---------------------------------------------------------------------------
// entry point
// ---------------------------------------------------------------------------

/// Puts `img` on the clipboard as `CF_DIBV5` and, when it can, as `"PNG"`.
///
/// Fails only if the DIB never landed; a missing PNG format leaves a clipboard
/// every paste target still understands.
pub(crate) fn write_image(img: &RgbaImage) -> Result<(), String> {
    // Both payloads are built *before* the clipboard is opened. It is a
    // process-wide lock and PNG-encoding a 4K desktop is not something to hold
    // it through. A PNG that will not encode just means the DIB goes alone.
    let dib = dibv5(img)?;
    let png = capture::encode_png(img).ok();

    let _session = ClipboardSession::open()?;
    unsafe { EmptyClipboard() }.map_err(|e| format!("EmptyClipboard failed: {e}"))?;

    set_format(CF_DIBV5, &dib).map_err(|e| format!("CF_DIBV5: {e}"))?;

    if let (Some(png), Some(format)) = (png, png_format()) {
        // Non-fatal by design: the DIB is already on the clipboard, so a failed
        // PNG write costs alpha fidelity in the apps that prefer it, not the
        // copy.
        if let Err(e) = set_format(format, &png) {
            eprintln!("santi.sharex: clipboard PNG format skipped: {e}");
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// the clipboard handle
// ---------------------------------------------------------------------------

/// An open clipboard, closed on drop.
///
/// A `Drop` impl rather than a `CloseClipboard` at each exit: leaving the
/// clipboard open blocks *every* process's clipboard access until santi.sharex quits,
/// so it has to survive an early return and a panic alike.
struct ClipboardSession;

impl ClipboardSession {
    fn open() -> Result<Self, String> {
        let mut last = String::new();
        for attempt in 0..OPEN_ATTEMPTS {
            match unsafe { OpenClipboard(None) } {
                Ok(()) => return Ok(Self),
                Err(e) => {
                    last = e.to_string();
                    if attempt + 1 < OPEN_ATTEMPTS {
                        std::thread::sleep(OPEN_BACKOFF);
                    }
                }
            }
        }
        Err(format!(
            "OpenClipboard failed {OPEN_ATTEMPTS} times over {}ms — another app is holding it ({last})",
            OPEN_BACKOFF.as_millis() * u128::from(OPEN_ATTEMPTS - 1)
        ))
    }
}

impl Drop for ClipboardSession {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

// ---------------------------------------------------------------------------
// formats
// ---------------------------------------------------------------------------

/// The registered id for the `"PNG"` clipboard format, or `None` if the OS
/// refused to register it.
///
/// Most modern apps prefer PNG over a DIB, and it is the only one of the two
/// that round-trips alpha losslessly. Registration is per-session and
/// idempotent — the same name always maps to the same id — so it is cached.
fn png_format() -> Option<u32> {
    static FORMAT: OnceLock<Option<u32>> = OnceLock::new();
    *FORMAT.get_or_init(|| match unsafe {
        RegisterClipboardFormatW(PCWSTR(PNG_FORMAT_NAME.as_ptr()))
    } {
        0 => None,
        id => Some(id),
    })
}

/// Copies `bytes` into a moveable global block and hands that block to the
/// clipboard. See the module docs on ownership: the block is freed here only
/// when `SetClipboardData` never took it.
fn set_format(format: u32, bytes: &[u8]) -> Result<(), String> {
    unsafe {
        let handle: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, bytes.len())
            .map_err(|e| format!("GlobalAlloc of {} bytes failed: {e}", bytes.len()))?;

        let ptr = GlobalLock(handle);
        if ptr.is_null() {
            let _ = GlobalFree(Some(handle));
            return Err("GlobalLock returned null".into());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.cast::<u8>(), bytes.len());
        // Returns FALSE with no error once the lock count reaches zero, which is
        // exactly what happens here, so the result carries no information.
        let _ = GlobalUnlock(handle);

        match SetClipboardData(format, Some(HANDLE(handle.0))) {
            Ok(_) => Ok(()),
            Err(e) => {
                // Ownership never transferred, so the block is still ours to free.
                let _ = GlobalFree(Some(handle));
                Err(format!("SetClipboardData failed: {e}"))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// CF_DIBV5
// ---------------------------------------------------------------------------

/// A `BITMAPV5HEADER` followed by top-down 32-bit BGRA rows — the exact byte
/// layout `CF_DIBV5` expects.
///
/// No row padding is computed because there is none to compute: at 32bpp a row
/// is `width * 4` bytes, already a multiple of the 4-byte DWORD alignment a DIB
/// requires.
fn dibv5(img: &RgbaImage) -> Result<Vec<u8>, String> {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err(format!("cannot put a {w}x{h} image on the clipboard"));
    }

    let pixels = rgba_to_bgra(img);
    let size_image = u32::try_from(pixels.len())
        .map_err(|_| format!("a {w}x{h} image is too large for a DIB"))?;
    let width = i32::try_from(w).map_err(|_| format!("width {w} does not fit a DIB"))?;
    let height = i32::try_from(h).map_err(|_| format!("height {h} does not fit a DIB"))?;

    let header = BITMAPV5HEADER {
        bV5Size: std::mem::size_of::<BITMAPV5HEADER>() as u32,
        bV5Width: width,
        // Negative height is what makes the rows top-down. A DIB is bottom-up by
        // default, and our rows are not, so a positive height here pastes the
        // capture upside down.
        bV5Height: -height,
        bV5Planes: 1,
        bV5BitCount: 32,
        // The masks below are only honoured under BI_BITFIELDS. Under BI_RGB the
        // fourth channel is padding, which is how a capture with transparency
        // ends up pasting as a black rectangle.
        bV5Compression: BI_BITFIELDS,
        bV5SizeImage: size_image,
        // The pixel bytes are B,G,R,A in that order, which a little-endian
        // machine reads back as one 0xAARRGGBB DWORD — hence these masks.
        bV5RedMask: 0x00FF_0000,
        bV5GreenMask: 0x0000_FF00,
        bV5BlueMask: 0x0000_00FF,
        bV5AlphaMask: 0xFF00_0000,
        bV5CSType: LCS_SRGB,
        bV5Intent: LCS_GM_IMAGES as u32,
        ..Default::default()
    };

    // `BITMAPV5HEADER` is `repr(C)` and, with its 16-bit fields adjacent, has no
    // padding to read as uninitialised bytes.
    let head = unsafe {
        std::slice::from_raw_parts(
            std::ptr::from_ref(&header).cast::<u8>(),
            std::mem::size_of::<BITMAPV5HEADER>(),
        )
    };

    let mut blob = Vec::with_capacity(head.len() + pixels.len());
    blob.extend_from_slice(head);
    blob.extend_from_slice(&pixels);
    Ok(blob)
}

/// Windows bitmaps are BGRA in memory; `RgbaImage` is RGBA. The swap is not
/// optional, and getting it backwards produces a picture with red and blue
/// exchanged — a wrong answer plausible enough to ship, which is why it is
/// pinned by a test below.
fn rgba_to_bgra(img: &RgbaImage) -> Vec<u8> {
    let src = img.as_raw();
    let mut out = Vec::with_capacity(src.len());
    for px in src.chunks_exact(4) {
        out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    #[test]
    fn rgba_lands_as_bgra_and_not_the_other_way_round() {
        // Deliberately asymmetric: R and B differ, and alpha is neither.
        let img = RgbaImage::from_pixel(1, 1, Rgba([250, 120, 10, 200]));
        assert_eq!(rgba_to_bgra(&img), vec![10, 120, 250, 200]);
    }

    #[test]
    fn rgba_to_bgra_keeps_every_pixel_in_order() {
        let mut img = RgbaImage::new(2, 1);
        img.put_pixel(0, 0, Rgba([1, 2, 3, 4]));
        img.put_pixel(1, 0, Rgba([5, 6, 7, 8]));
        assert_eq!(rgba_to_bgra(&img), vec![3, 2, 1, 4, 7, 6, 5, 8]);
    }

    #[test]
    fn dibv5_is_top_down_32bpp_with_a_live_alpha_mask() {
        let mut img = RgbaImage::new(2, 3);
        img.put_pixel(0, 0, Rgba([250, 120, 10, 200]));
        let blob = dibv5(&img).expect("a 2x3 image should encode");

        let head_len = std::mem::size_of::<BITMAPV5HEADER>();
        assert_eq!(blob.len(), head_len + 2 * 3 * 4);

        let head: BITMAPV5HEADER = unsafe { std::ptr::read_unaligned(blob.as_ptr().cast()) };
        assert_eq!(head.bV5Size, head_len as u32);
        assert_eq!(head.bV5Width, 2);
        assert_eq!(head.bV5Height, -3, "negative height means top-down rows");
        assert_eq!(head.bV5Planes, 1);
        assert_eq!(head.bV5BitCount, 32);
        assert_eq!(head.bV5Compression, BI_BITFIELDS);
        assert_eq!(head.bV5AlphaMask, 0xFF00_0000);
        assert_eq!(head.bV5SizeImage as usize, blob.len() - head_len);

        // The first pixel sits immediately after the header, already swapped.
        assert_eq!(&blob[head_len..head_len + 4], &[10, 120, 250, 200]);
    }

    #[test]
    fn dibv5_rejects_an_empty_image() {
        assert!(dibv5(&RgbaImage::new(0, 0)).is_err());
    }
}
