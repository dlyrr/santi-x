//! Offline text recognition through `Windows.Media.Ocr` (M5 §3).
//!
//! Windows ships a text recogniser as part of the OS, so "extract the text from
//! this capture" needs no service, no API key and no network — which is the only
//! reason it is in santi.sharex at all.
//!
//! The whole path is: `RgbaImage` → BGRA bytes → WinRT `IBuffer` →
//! `SoftwareBitmap` → `OcrEngine::RecognizeAsync`. Two things about it are easy
//! to get quietly wrong:
//!
//! * **The channel swap.** WinRT wants `Bgra8`; `RgbaImage` is RGBA. A
//!   red/blue-swapped image still OCRs — it just reads *worse* — so this is a
//!   silent failure that survives casual testing. It reuses the same
//!   `rgba_to_bgra` the clipboard path uses (M2.7 §1) rather than growing a
//!   second copy, and [`tests::bgra_round_trips_through_a_software_bitmap`]
//!   pins the byte order end to end.
//! * **`MaxImageDimension`.** `RecognizeAsync` rejects any bitmap over
//!   10000px on either axis with a bare `E_INVALIDARG`, and a fullscreen grab
//!   across three 4K monitors is 11520px wide — so the ceiling is not
//!   theoretical, and the image is scaled to fit before it reaches the engine.
//!   There is measurably *no* lower bound to match it: a 1x1 bitmap is accepted,
//!   so nothing here pads or upscales small crops.

use std::borrow::Cow;
use std::path::Path;
use std::sync::OnceLock;

use image::imageops::FilterType;
use image::RgbaImage;
use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use windows::core::HSTRING;
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;
use windows::Win32::System::Com::CoIncrementMTAUsage;

use crate::clipboard::rgba_to_bgra;
use crate::run_blocking;
use crate::store;

/// Used only if `MaxImageDimension` itself fails, which would mean the engine is
/// already unusable. Matches the value Windows actually reports.
const MAX_DIMENSION_FALLBACK: u32 = 10_000;

/// The one error that is not a bug and not a bad image: the OCR engine is a
/// per-language optional feature, and a Windows install can legitimately have
/// none of them. Saying "OCR failed" there sends the user hunting for a fault
/// that does not exist, so this names the exact place to fix it.
const NO_LANGUAGE_PACK: &str = "Windows has no OCR language pack installed, so there is nothing here to read text with. \
Add one in Settings → Time & language → Language & region: pick a language, open its three-dot menu → Language options, \
and install the \"Optical character recognition\" optional feature. santi.sharex will use it the next time you extract text.";

// ---------------------------------------------------------------------------
// wire types (M5 §3)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrLine {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrResult {
    /// Every line joined with `\n`, in reading order.
    pub text: String,
    pub lines: Vec<OcrLine>,
    /// The BCP-47 tag of the language the engine actually recognised with —
    /// `"en-US"`, `"de-DE"`. Empty only if the engine refused to name it.
    pub language: String,
}

// ---------------------------------------------------------------------------
// command
// ---------------------------------------------------------------------------

/// Reads the capture back off disk and recognises it.
///
/// `spawn_blocking` because OCR on a 4K screenshot is comfortably a second of
/// work, and it must not be a second of frozen UI.
#[tauri::command]
pub async fn ocr_capture(app: AppHandle, id: String) -> Result<OcrResult, String> {
    run_blocking(move || {
        let record = store::capture_by_id(&app, &id)?;

        // Same shape as `editor::get_capture_image`: a capture taken with "save
        // to disk" off only ever existed in the clipboard and as a 480px
        // thumbnail, and reading text out of a thumbnail is worse than useless.
        if record.path.is_empty() {
            return Err(format!(
                "\"{}\" was never written to disk, so there is no image to read text from",
                record.name
            ));
        }
        if !Path::new(&record.path).exists() {
            return Err(format!("{} is no longer on disk", record.name));
        }

        let img = image::open(&record.path)
            .map_err(|e| format!("could not reopen {}: {e}", record.name))?
            .to_rgba8();
        recognise(&img)
    })
    .await
}

// ---------------------------------------------------------------------------
// recognition
// ---------------------------------------------------------------------------

pub fn recognise(img: &RgbaImage) -> Result<OcrResult, String> {
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err(format!("there is nothing to read in a {w}x{h} image"));
    }

    ensure_mta();
    let engine = engine()?;

    // Static on the class, not a property of the engine: the ceiling is the
    // recogniser's, not the language's.
    let max = OcrEngine::MaxImageDimension().unwrap_or(MAX_DIMENSION_FALLBACK);
    let prepared = fit_for_ocr(img, max);
    let bitmap = software_bitmap(prepared.as_ref())?;

    let recognised = engine
        .RecognizeAsync(&bitmap)
        .and_then(|op| op.get())
        .map_err(|e| {
            eprintln!("santi.sharex: RecognizeAsync failed: {e}");
            format!("Windows could not read this image: {e}")
        })?;

    let mut lines: Vec<OcrLine> = Vec::new();
    let vector = recognised
        .Lines()
        .map_err(|e| format!("Windows returned no lines for this image: {e}"))?;
    for line in vector {
        // A line whose text cannot be read is dropped rather than failing the
        // whole capture — one bad line is not worth losing the other fifty.
        if let Ok(text) = line.Text() {
            lines.push(OcrLine {
                text: text.to_string(),
            });
        }
    }

    // Built from the lines rather than read from `OcrResult::Text` so the two
    // halves of the payload can never disagree: "copy all" and the per-line list
    // are the same text, laid out the same way.
    let text = lines
        .iter()
        .map(|l| l.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    let language = engine
        .RecognizerLanguage()
        .and_then(|lang| lang.LanguageTag())
        .map(|tag| tag.to_string())
        .unwrap_or_default();

    Ok(OcrResult {
        text,
        lines,
        language,
    })
}

/// The user's own languages first, `en-US` only as a floor.
///
/// `TryCreateFromUserProfileLanguages` returns null — an `Err` here — when none
/// of the user's preferred languages has a recogniser installed, which is common
/// enough on a non-English install that the fallback is worth having. When
/// *neither* works there is no OCR on this machine at all, which is a
/// configuration problem with a specific fix rather than a failure to report.
fn engine() -> Result<OcrEngine, String> {
    match OcrEngine::TryCreateFromUserProfileLanguages() {
        Ok(engine) => return Ok(engine),
        Err(e) => eprintln!("santi.sharex: no OCR engine for the user's languages ({e}); trying en-US"),
    }

    Language::CreateLanguage(&HSTRING::from("en-US"))
        .and_then(|lang| OcrEngine::TryCreateFromLanguage(&lang))
        .map_err(|e| {
            eprintln!("santi.sharex: no en-US OCR engine either ({e})");
            NO_LANGUAGE_PACK.to_string()
        })
}

/// Packs `img` into a WinRT `SoftwareBitmap` in `Bgra8`.
///
/// `DataWriter` is the shortest route from a `&[u8]` to an `IBuffer` that does
/// not need `IBufferByteAccess` and the COM plumbing behind it.
fn software_bitmap(img: &RgbaImage) -> Result<SoftwareBitmap, String> {
    let (w, h) = img.dimensions();
    let width = i32::try_from(w).map_err(|_| format!("width {w} is too large for a bitmap"))?;
    let height = i32::try_from(h).map_err(|_| format!("height {h} is too large for a bitmap"))?;

    // The swap this whole module hangs on. See the module docs.
    let bgra = rgba_to_bgra(img);

    let writer = DataWriter::new().map_err(|e| format!("could not create a WinRT writer: {e}"))?;
    writer
        .WriteBytes(&bgra)
        .map_err(|e| format!("could not write {} bytes for OCR: {e}", bgra.len()))?;
    let buffer = writer
        .DetachBuffer()
        .map_err(|e| format!("could not detach the OCR buffer: {e}"))?;

    SoftwareBitmap::CreateCopyFromBuffer(&buffer, BitmapPixelFormat::Bgra8, width, height)
        .map_err(|e| format!("could not build a {w}x{h} bitmap for OCR: {e}"))
}

/// Resamples only when the engine would otherwise reject the image outright.
///
/// Borrowed in every ordinary case — a single-monitor screenshot pays nothing
/// for this — and the copy it makes when it does fire is the price of getting an
/// answer at all.
fn fit_for_ocr(img: &RgbaImage, max: u32) -> Cow<'_, RgbaImage> {
    let (w, h) = img.dimensions();
    match fit_within(w, h, max) {
        None => Cow::Borrowed(img),
        // Lanczos3 rather than Nearest or Triangle: this is a downscale of text,
        // where the filter is the difference between legible glyphs and mush,
        // and the recogniser only gets the one look at it.
        Some((tw, th)) => Cow::Owned(image::imageops::resize(img, tw, th, FilterType::Lanczos3)),
    }
}

/// The size `w`x`h` has to shrink to for the engine to accept it, aspect ratio
/// intact, or `None` when it already fits.
fn fit_within(w: u32, h: u32, max: u32) -> Option<(u32, u32)> {
    if w == 0 || h == 0 || max == 0 || (w <= max && h <= max) {
        return None;
    }

    let shrink = (f64::from(max) / f64::from(w)).min(f64::from(max) / f64::from(h));
    // Floor, then clamp: rounding up is what puts a 10000 limit at 10001.
    let tw = ((f64::from(w) * shrink).floor() as u32).clamp(1, max);
    let th = ((f64::from(h) * shrink).floor() as u32).clamp(1, max);
    Some((tw, th))
}

/// Puts the process in an apartment WinRT activation can work from.
///
/// `spawn_blocking` hands out uninitialised pool threads, and activating a
/// runtime class from one is `CO_E_NOTINITIALIZED`. `CoIncrementMTAUsage` keeps
/// an implicit MTA alive process-wide instead of initialising each thread, so
/// there is nothing to undo on a thread we do not own and nothing that can
/// disturb the main thread's own apartment. Once per process; the cookie exists
/// only to *end* the implicit MTA, which santi.sharex never wants to do.
fn ensure_mta() {
    static MTA: OnceLock<()> = OnceLock::new();
    MTA.get_or_init(|| {
        if let Err(e) = unsafe { CoIncrementMTAUsage() } {
            eprintln!("santi.sharex: CoIncrementMTAUsage failed: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;
    use windows::Storage::Streams::{Buffer, DataReader};

    #[test]
    fn anything_already_inside_the_ceiling_is_left_alone() {
        // Both the ordinary case and a small crop: there is no lower bound to
        // rescue, so a 1x1 is passed through exactly as a 1920x1080 is.
        assert_eq!(fit_within(1920, 1080, 10_000), None);
        assert_eq!(fit_within(10_000, 10_000, 10_000), None);
        assert_eq!(fit_within(316, 20, 10_000), None);
        assert_eq!(fit_within(1, 1, 10_000), None);
    }

    #[test]
    fn an_oversized_capture_is_scaled_inside_the_ceiling() {
        // Three 4K monitors side by side — the real reason this exists.
        let (w, h) = fit_within(11_520, 2_160, 10_000).expect("11520 is over the ceiling");
        assert!(w <= 10_000 && h <= 10_000, "{w}x{h} is still over");
        assert_eq!(w, 10_000);
        // Aspect ratio survives, or the glyphs arrive stretched.
        let ratio = f64::from(w) / f64::from(h);
        assert!((ratio - 11_520.0 / 2_160.0).abs() < 0.01, "aspect drifted to {ratio}");
    }

    #[test]
    fn one_pixel_over_is_still_over() {
        // The off-by-one the floor-then-clamp exists for: 10001 must not come
        // back as 10001, and must not round back up to it either.
        assert_eq!(fit_within(10_001, 4, 10_000), Some((10_000, 3)));
        assert_eq!(fit_within(4, 10_001, 10_000), Some((3, 10_000)));
    }

    #[test]
    fn degenerate_sizes_do_not_divide_by_zero() {
        assert_eq!(fit_within(0, 10, 10_000), None);
        assert_eq!(fit_within(10, 0, 10_000), None);
        assert_eq!(fit_within(10, 10, 0), None);
    }

    /// Pins the constant against the engine rather than against a docs page:
    /// `MAX_DIMENSION_FALLBACK` is only ever used when the call below fails, and
    /// a fallback that disagrees with reality is worse than no fallback.
    #[test]
    fn the_fallback_ceiling_matches_what_windows_reports() {
        ensure_mta();
        assert_eq!(
            OcrEngine::MaxImageDimension().unwrap(),
            MAX_DIMENSION_FALLBACK
        );
    }

    /// The ceiling is a real, enforced limit — one pixel over is `E_INVALIDARG`
    /// — and [`fit_for_ocr`] is what keeps a wide multi-monitor grab under it.
    /// Cheap to prove: 4px tall, so the oversized bitmap is only 160KB.
    #[test]
    fn the_engine_rejects_oversized_bitmaps_and_accepts_what_we_hand_it() {
        ensure_mta();
        let Ok(engine) = engine() else {
            // No language pack on this machine; nothing to assert against.
            return;
        };

        let too_wide = RgbaImage::from_pixel(10_001, 4, Rgba([255, 255, 255, 255]));
        let raw = software_bitmap(&too_wide).unwrap();
        assert!(
            engine.RecognizeAsync(&raw).and_then(|op| op.get()).is_err(),
            "10001px wide was accepted — the ceiling moved, revisit fit_for_ocr"
        );

        let fitted = fit_for_ocr(&too_wide, MAX_DIMENSION_FALLBACK);
        let bitmap = software_bitmap(fitted.as_ref()).unwrap();
        assert!(
            engine.RecognizeAsync(&bitmap).and_then(|op| op.get()).is_ok(),
            "the scaled-down bitmap was still rejected"
        );
    }

    /// The silent failure this module is most exposed to: a red/blue swap that
    /// still produces text, only worse. Round-trips the pixels back out of the
    /// `SoftwareBitmap` and checks the byte order that actually reached WinRT.
    #[test]
    fn bgra_round_trips_through_a_software_bitmap() {
        ensure_mta();

        let mut img = RgbaImage::new(4, 2);
        // Deliberately asymmetric: R, G and B all differ, so a swap cannot hide.
        img.put_pixel(0, 0, Rgba([250, 120, 10, 255]));
        img.put_pixel(3, 1, Rgba([1, 2, 3, 255]));

        let bitmap = software_bitmap(&img).expect("a 4x2 bitmap should build");
        assert_eq!(bitmap.PixelWidth().unwrap(), 4);
        assert_eq!(bitmap.PixelHeight().unwrap(), 2);
        assert_eq!(
            bitmap.BitmapPixelFormat().unwrap(),
            BitmapPixelFormat::Bgra8
        );

        let len = 4 * 2 * 4;
        let buffer = Buffer::Create(len as u32).expect("buffer");
        bitmap.CopyToBuffer(&buffer).expect("copy back out");
        let reader = DataReader::FromBuffer(&buffer).expect("reader");
        let mut out = vec![0u8; len];
        reader.ReadBytes(&mut out).expect("read");

        // B, G, R, A — not R, G, B, A.
        assert_eq!(&out[0..4], &[10, 120, 250, 255]);
        assert_eq!(&out[len - 4..], &[3, 2, 1, 255]);
    }
}
