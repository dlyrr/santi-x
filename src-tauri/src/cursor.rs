//! Capturing the mouse cursor and compositing it onto a shot.
//!
//! `xcap` reads the framebuffer, and the framebuffer never contains the cursor —
//! Windows composites it separately, in hardware where it can. So a cursor in a
//! screenshot has to be drawn in by hand: ask the OS which cursor is showing and
//! where, rasterise its icon, and alpha-blend it at the right offset.
//!
//! # Two kinds of cursor
//!
//! `GetIconInfo` hands back either a colour bitmap plus a mask, or — for the
//! classic monochrome cursors, the I-beam being the one everybody meets — no
//! colour bitmap at all and a mask that is *double height*: the top half is the
//! AND mask and the bottom half the XOR mask. The two cases need different
//! decoding, and the monochrome one is easy to forget because the common arrow
//! cursor is a colour icon on any modern theme, so it works right up until you
//! screenshot a text field.
//!
//! | AND | XOR | result   |
//! |-----|-----|----------|
//! | 0   | 0   | black    |
//! | 0   | 1   | white    |
//! | 1   | 0   | transparent |
//! | 1   | 1   | invert destination — approximated as white |
//!
//! The invert case is what makes the I-beam visible over both light and dark
//! backgrounds. Truly honouring it would mean inverting the pixels underneath;
//! white is the honest approximation and is what it looks like over the dark
//! backgrounds it is usually seen on.
//!
//! # Colour cursors without an alpha channel
//!
//! A 32-bit colour cursor usually carries real alpha, but some carry all-zero
//! alpha and rely on the AND mask instead. Trusting the alpha channel blindly
//! renders those completely invisible, so an all-zero alpha channel falls back
//! to the mask.

use image::RgbaImage;

use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, DIB_RGB_COLORS, HBITMAP, HGDIOBJ,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorInfo, GetIconInfo, CURSORINFO, CURSOR_SHOWING, HICON, ICONINFO,
};

/// A rasterised cursor and where its top-left belongs, in xcap screen space
/// (hotspot already subtracted).
pub struct CursorShot {
    pub image: RgbaImage,
    pub x: i32,
    pub y: i32,
}

/// The cursor as it is right now, or `None` when it is hidden, unavailable, or
/// something in the Win32 dance failed.
///
/// Never an `Err`: a screenshot without a cursor is a fine screenshot, and
/// failing an entire capture because the cursor could not be read would be an
/// absurd trade.
pub fn capture_cursor() -> Option<CursorShot> {
    let mut info = CURSORINFO {
        cbSize: std::mem::size_of::<CURSORINFO>() as u32,
        ..Default::default()
    };

    unsafe { GetCursorInfo(&mut info) }.ok()?;

    if info.flags != CURSOR_SHOWING || info.hCursor.is_invalid() {
        return None;
    }

    // `HCURSOR` and `HICON` are the same handle type to Win32; the windows crate
    // keeps them distinct, so the cast is here rather than in the signature.
    let icon = HICON(info.hCursor.0);
    let mut icon_info = ICONINFO::default();
    unsafe { GetIconInfo(icon, &mut icon_info) }.ok()?;

    // Both bitmaps are ours to free the moment we are done with them; leaking
    // GDI objects from a path that runs on every capture would bleed handles
    // until the process died.
    let colour = (!icon_info.hbmColor.is_invalid()).then_some(icon_info.hbmColor);
    let mask = (!icon_info.hbmMask.is_invalid()).then_some(icon_info.hbmMask);

    let image = rasterise(colour, mask);

    if let Some(bmp) = colour {
        unsafe { let _ = DeleteObject(HGDIOBJ(bmp.0)); }
    }
    if let Some(bmp) = mask {
        unsafe { let _ = DeleteObject(HGDIOBJ(bmp.0)); }
    }

    let image = image?;

    Some(CursorShot {
        x: info.ptScreenPos.x - icon_info.xHotspot as i32,
        y: info.ptScreenPos.y - icon_info.yHotspot as i32,
        image,
    })
}

fn rasterise(colour: Option<HBITMAP>, mask: Option<HBITMAP>) -> Option<RgbaImage> {
    match colour {
        Some(colour) => rasterise_colour(colour, mask),
        None => rasterise_monochrome(mask?),
    }
}

/// A colour cursor: BGRA straight out of the bitmap, with the AND mask used for
/// transparency only when the alpha channel turns out to be unusable.
fn rasterise_colour(colour: HBITMAP, mask: Option<HBITMAP>) -> Option<RgbaImage> {
    let (w, h) = bitmap_size(colour)?;
    let bgra = read_bgra(colour, w, h)?;

    let mut img = RgbaImage::new(w, h);
    let opaque_alpha = bgra.chunks_exact(4).any(|px| px[3] != 0);

    // Only decoded when the alpha channel is useless, which is the uncommon case.
    let and_bits = if opaque_alpha {
        None
    } else {
        mask.and_then(|m| read_mono_rows(m, w, h, 0))
    };

    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let (b, g, r, a) = (bgra[i], bgra[i + 1], bgra[i + 2], bgra[i + 3]);
            let alpha = if opaque_alpha {
                a
            } else {
                // AND bit set means "leave the destination alone".
                match &and_bits {
                    Some(bits) if bits[(y * w + x) as usize] => 0,
                    _ => 255,
                }
            };
            img.put_pixel(x, y, image::Rgba([r, g, b, alpha]));
        }
    }

    Some(img)
}

/// A monochrome cursor: one double-height mask, AND over XOR.
fn rasterise_monochrome(mask: HBITMAP) -> Option<RgbaImage> {
    let (w, full_h) = bitmap_size(mask)?;
    if full_h < 2 {
        return None;
    }
    let h = full_h / 2;

    let and_bits = read_mono_rows(mask, w, h, 0)?;
    let xor_bits = read_mono_rows(mask, w, h, h)?;

    let mut img = RgbaImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            let px = match (and_bits[i], xor_bits[i]) {
                (false, false) => image::Rgba([0, 0, 0, 255]),
                (false, true) => image::Rgba([255, 255, 255, 255]),
                (true, false) => image::Rgba([0, 0, 0, 0]),
                // "invert the destination" — see the module docs.
                (true, true) => image::Rgba([255, 255, 255, 255]),
            };
            img.put_pixel(x, y, px);
        }
    }

    Some(img)
}

fn bitmap_size(bmp: HBITMAP) -> Option<(u32, u32)> {
    let mut info = BITMAP::default();
    let written = unsafe {
        GetObjectW(
            HGDIOBJ(bmp.0),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut info as *mut BITMAP as *mut _),
        )
    };
    if written == 0 || info.bmWidth <= 0 || info.bmHeight <= 0 {
        return None;
    }
    Some((info.bmWidth as u32, info.bmHeight as u32))
}

/// Read a bitmap as top-down 32-bit BGRA.
fn read_bgra(bmp: HBITMAP, w: u32, h: u32) -> Option<Vec<u8>> {
    let mut header = BITMAPINFO::default();
    header.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    header.bmiHeader.biWidth = w as i32;
    // Negative height = top-down rows, matching `RgbaImage`'s order.
    header.bmiHeader.biHeight = -(h as i32);
    header.bmiHeader.biPlanes = 1;
    header.bmiHeader.biBitCount = 32;
    header.bmiHeader.biCompression = BI_RGB.0;

    let mut buf = vec![0u8; (w as usize) * (h as usize) * 4];

    let dc = unsafe { GetDC(None) };
    if dc.is_invalid() {
        return None;
    }
    let rows = unsafe {
        GetDIBits(
            dc,
            bmp,
            0,
            h,
            Some(buf.as_mut_ptr() as *mut _),
            &mut header,
            DIB_RGB_COLORS,
        )
    };
    unsafe { ReleaseDC(None, dc) };

    (rows != 0).then_some(buf)
}

/// Read `h` rows of a 1-bit bitmap starting at `row_offset`, as one bool per
/// pixel. Rows in a DIB are padded to 4-byte boundaries, which is the detail
/// that silently shears the image if you assume `w / 8` bytes per row.
fn read_mono_rows(bmp: HBITMAP, w: u32, h: u32, row_offset: u32) -> Option<Vec<bool>> {
    let total_h = row_offset + h;

    let mut header = BITMAPINFO::default();
    header.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    header.bmiHeader.biWidth = w as i32;
    header.bmiHeader.biHeight = -(total_h as i32);
    header.bmiHeader.biPlanes = 1;
    header.bmiHeader.biBitCount = 1;
    header.bmiHeader.biCompression = BI_RGB.0;

    let stride = (((w + 31) / 32) * 4) as usize;
    // `BITMAPINFO` carries one palette entry inline and a 1bpp DIB needs two, so
    // the struct is over-allocated as bytes rather than used directly.
    let mut palette_backed = vec![0u8; std::mem::size_of::<BITMAPINFO>() + 8];
    unsafe {
        std::ptr::copy_nonoverlapping(
            &header as *const BITMAPINFO as *const u8,
            palette_backed.as_mut_ptr(),
            std::mem::size_of::<BITMAPINFO>(),
        );
    }

    let mut buf = vec![0u8; stride * total_h as usize];

    let dc = unsafe { GetDC(None) };
    if dc.is_invalid() {
        return None;
    }
    let rows = unsafe {
        GetDIBits(
            dc,
            bmp,
            0,
            total_h,
            Some(buf.as_mut_ptr() as *mut _),
            palette_backed.as_mut_ptr() as *mut BITMAPINFO,
            DIB_RGB_COLORS,
        )
    };
    unsafe { ReleaseDC(None, dc) };

    if rows == 0 {
        return None;
    }

    let mut bits = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        let row = (row_offset + y) as usize * stride;
        for x in 0..w {
            let byte = buf[row + (x / 8) as usize];
            bits.push((byte >> (7 - (x % 8))) & 1 == 1);
        }
    }
    Some(bits)
}

/// Alpha-blend the cursor onto a shot whose top-left sits at `(origin_x, origin_y)`
/// in the same screen space as [`CursorShot::x`]/`y`.
///
/// Clips on every edge, so a cursor half off the captured area draws its visible
/// half rather than being dropped or panicking.
pub fn composite(
    target: &mut RgbaImage,
    cursor: &CursorShot,
    origin_x: i32,
    origin_y: i32,
) {
    let (tw, th) = target.dimensions();
    let (cw, ch) = cursor.image.dimensions();

    let dx = cursor.x - origin_x;
    let dy = cursor.y - origin_y;

    for cy in 0..ch {
        let ty = dy + cy as i32;
        if ty < 0 || ty >= th as i32 {
            continue;
        }
        for cx in 0..cw {
            let tx = dx + cx as i32;
            if tx < 0 || tx >= tw as i32 {
                continue;
            }

            let src = cursor.image.get_pixel(cx, cy).0;
            let a = src[3] as u32;
            if a == 0 {
                continue;
            }
            if a == 255 {
                target.put_pixel(tx as u32, ty as u32, image::Rgba(src));
                continue;
            }

            let dst = target.get_pixel(tx as u32, ty as u32).0;
            let blend = |s: u8, d: u8| ((s as u32 * a + d as u32 * (255 - a)) / 255) as u8;
            target.put_pixel(
                tx as u32,
                ty as u32,
                image::Rgba([
                    blend(src[0], dst[0]),
                    blend(src[1], dst[1]),
                    blend(src[2], dst[2]),
                    // The desktop is opaque, so the result is too.
                    dst[3].max(src[3]),
                ]),
            );
        }
    }
}

/// Capture the cursor and draw it onto `shot`, if one is showing. A no-op when
/// there is nothing to draw.
pub fn overlay_onto(shot: &mut crate::capture::Shot) {
    if let Some(cursor) = capture_cursor() {
        composite(&mut shot.image, &cursor, shot.origin_x, shot.origin_y);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, px: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(w, h, image::Rgba(px))
    }

    #[test]
    fn composites_at_the_right_offset() {
        let mut target = solid(10, 10, [0, 0, 0, 255]);
        let cursor = CursorShot {
            image: solid(2, 2, [255, 0, 0, 255]),
            x: 103,
            y: 205,
        };
        // Shot origin (100, 200) puts the cursor at (3, 5) within the image.
        composite(&mut target, &cursor, 100, 200);

        assert_eq!(target.get_pixel(3, 5).0, [255, 0, 0, 255]);
        assert_eq!(target.get_pixel(4, 6).0, [255, 0, 0, 255]);
        assert_eq!(target.get_pixel(2, 5).0, [0, 0, 0, 255], "left of the cursor");
        assert_eq!(target.get_pixel(5, 5).0, [0, 0, 0, 255], "right of the cursor");
    }

    #[test]
    fn clips_instead_of_panicking_when_partly_offscreen() {
        let mut target = solid(4, 4, [0, 0, 0, 255]);
        let cursor = CursorShot {
            image: solid(4, 4, [255, 255, 255, 255]),
            x: -2,
            y: -2,
        };
        composite(&mut target, &cursor, 0, 0);

        // Only the bottom-right quadrant of the cursor lands on the image.
        assert_eq!(target.get_pixel(0, 0).0, [255, 255, 255, 255]);
        assert_eq!(target.get_pixel(1, 1).0, [255, 255, 255, 255]);
        assert_eq!(target.get_pixel(2, 2).0, [0, 0, 0, 255]);
    }

    #[test]
    fn fully_transparent_pixels_leave_the_target_alone() {
        let mut target = solid(2, 2, [10, 20, 30, 255]);
        let cursor = CursorShot {
            image: solid(2, 2, [255, 0, 0, 0]),
            x: 0,
            y: 0,
        };
        composite(&mut target, &cursor, 0, 0);
        assert_eq!(target.get_pixel(0, 0).0, [10, 20, 30, 255]);
    }

    #[test]
    fn half_alpha_blends_evenly() {
        let mut target = solid(1, 1, [0, 0, 0, 255]);
        let cursor = CursorShot {
            image: solid(1, 1, [255, 255, 255, 128]),
            x: 0,
            y: 0,
        };
        composite(&mut target, &cursor, 0, 0);
        let got = target.get_pixel(0, 0).0;
        assert!(
            (got[0] as i32 - 128).abs() <= 1,
            "expected ~128, got {}",
            got[0]
        );
        assert_eq!(got[3], 255, "the desktop stays opaque");
    }
}
