//! Scrolling capture (M5 §4): capture a window, scroll it, capture again, and
//! stitch the frames into one tall image.
//!
//! # It fails honestly or not at all
//!
//! This is the least reliable feature in santi.sharex and pretending otherwise
//! would be the real bug. A scrolling capture that guesses wrong does not
//! produce *nothing* — it produces a twenty-thousand-pixel image with seams and
//! repeated paragraphs that the user only discovers later, by reading it. So
//! every uncertainty in here resolves the same way: **stop, keep what is
//! already stitched, and say why.** [`ScrollOutcome::message`] is always
//! populated and [`ScrollOutcome::incomplete`] is the flag the UI must surface.
//!
//! # Measuring the overlap, never assuming the delta
//!
//! Three wheel notches is not a number of pixels. Smooth scrolling, momentum,
//! line snapping and per-app multipliers mean the same input moves Notepad,
//! Chrome and a terminal by three different amounts, and none of them are
//! constant across a run. Anything built on an assumed delta guarantees seams
//! and duplicated rows.
//!
//! So the delta is *measured*. For every candidate shift `d`, a fixed-height
//! strip of frame *n+1* is compared against the same strip of frame *n* moved
//! down by `d`, scoring mean absolute luma difference over a subsampled grid.
//! The lowest score wins. A fixed strip height matters: scoring the whole
//! overlap would shrink the sample as `d` grows and quietly bias the search
//! towards large shifts.
//!
//! Two guards sit on top of the winner:
//!
//! * **Absolute** — the best score must actually be a match ([`MAX_MATCH_SCORE`]).
//!   Two unrelated frames have no good offset at all, and that is the case that
//!   must never be concatenated blindly.
//! * **Distinctive** — the winner must be either near-perfect or clearly better
//!   than the best rival more than [`RIVAL_GUARD`] pixels away. A strip that
//!   matches everywhere matched nothing.
//!
//! Ties resolve to the *smallest* shift. Under-scrolling repeats a few rows;
//! over-scrolling drops content that is then gone forever.
//!
//! # Driving the wheel
//!
//! `SendInput` with the pointer parked over the window, not `WM_MOUSEWHEEL`
//! posted at a handle. A posted wheel message is delivered to whichever HWND it
//! is addressed to, and in Chromium, Electron, WPF and every other app whose
//! scrollable surface is a child window that is the wrong one — the message is
//! silently ignored and the capture reads as "the content did not move".
//! Synthetic input goes through the same path a real wheel does, so whatever
//! the app does with a real wheel is what it does here.
//!
//! The cost is that it needs the pointer. Which means:
//!
//! * The target is raised to the foreground first, and is re-checked with
//!   `WindowFromPoint` before *every* step. If anything is in front of it the
//!   run stops rather than scrolling a window the user did not pick.
//! * **Moving the mouse ends the run.** That is deliberate and it is the escape
//!   hatch: the pointer is parked once and never yanked back, so reaching for
//!   Cancel stops the capture on the way.
//! * Our own windows are refused outright.
//!
//! # What is not drawn in
//!
//! The cursor, whatever `capture_cursor` says. It is parked in the middle of
//! every frame, so drawing it would stamp the same arrow down the whole length
//! of the stitched image.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use image::RgbaImage;
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_WHEEL, MOUSEINPUT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetAncestor, GetCursorPos, GetWindowThreadProcessId, SetCursorPos, SetForegroundWindow,
    WindowFromPoint, GA_ROOT,
};

use crate::capture;
use crate::store::{AppState, CaptureRecord};
use crate::{finalize, lock, run_blocking};

/// Frames captured so far. The UI subscribes to this for its progress readout.
const PROGRESS_EVENT: &str = "scroll://progress";

// ---------------------------------------------------------------------------
// tuning
// ---------------------------------------------------------------------------

/// Bounds applied to the three settings at the point of use.
///
/// Unlike `theme` and `loupe_zoom` these are not normalised in `store.rs`,
/// because they are read in exactly one place — this loop — so clamping here is
/// complete. A hand-edited `scrollDelayMs` of four billion would otherwise be a
/// forty-six day sleep inside a `spawn_blocking` worker.
const DELAY_MAX_MS: u32 = 5_000;
const STEP_MIN: i32 = 1;
const STEP_MAX: i32 = 15;
const FRAMES_MIN: u32 = 2;
const FRAMES_MAX: u32 = 400;

/// One wheel notch, in the units `MOUSEINPUT::mouseData` wants. `WHEEL_DELTA`
/// from the `windows` crate is a `u32`, and this is signed arithmetic — scrolling
/// down is a *negative* rotation — so it is restated here rather than cast at
/// every use.
const WHEEL_NOTCH: i32 = 120;

/// How long the target gets to come to the front before the first grab.
const FOCUS_SETTLE: Duration = Duration::from_millis(180);
/// How often the settle delay wakes to look at the cancel flag. A 2s delay must
/// not mean a 2s wait after the user presses Cancel.
const CANCEL_POLL: Duration = Duration::from_millis(25);

/// How far the pointer may drift from the park point before the run is treated
/// as abandoned. Some mice report a pixel of jitter sitting still.
const POINTER_TOLERANCE: i32 = 4;

/// Shortest shift that counts as movement. One pixel per step is not a scroll,
/// it is rounding, and treating it as one would stitch sixty frames into a
/// sixty-pixel-taller image.
const MIN_MOVE: u32 = 2;

/// How many consecutive still frames end the run. Two rather than one: an app
/// mid-way through a smooth-scroll animation can hand back an unchanged frame
/// and then carry on, and truncating a document there would look like a bug in
/// the stitcher.
const STILL_CONFIRMATIONS: u32 = 2;

/// Mean absolute luma difference, per sampled pixel, above which the best
/// candidate is not a match at all. JPEG-ish noise and subpixel text rendering
/// sit well under this; two unrelated frames sit far above it.
const MAX_MATCH_SCORE: f32 = 12.0;
/// A score this low is pixel-identical in all but name, and needs no rival test
/// to be believed.
const NEAR_PERFECT: f32 = 2.5;
/// The winner must beat its nearest distant rival by this factor.
const DISTINCT_MARGIN: f32 = 1.5;
/// Candidates within this many pixels of the winner are the same match, not
/// rivals, so they are excluded from the distinctiveness test.
const RIVAL_GUARD: u32 = 6;

/// Comparison strip bounds. Small enough to fit a modest window, large enough
/// that the sample is not dominated by one heading.
const MIN_STRIP: u32 = 16;
const MAX_STRIP: u32 = 192;
/// Below this there is not enough horizontal signal to match on.
const MIN_STRIP_WIDTH: u32 = 8;

/// Grid resolution. ~1k samples per candidate offset: a full search over a
/// 1400px window is about a million comparisons per frame pair, which is
/// nothing next to a 250ms settle delay.
const SAMPLE_ROWS: u32 = 24;
const SAMPLE_COLS: u32 = 40;

/// Ceilings on the stitched result. A 1920-wide image at 32768px tall is
/// already 250MB of RGBA in memory; past that the PNG encode is the user's
/// problem long before the seams are.
const MAX_STITCH_HEIGHT: u32 = 32_768;
const MAX_STITCH_PIXELS: u64 = 1 << 26;

// ---------------------------------------------------------------------------
// types
// ---------------------------------------------------------------------------

/// Why the loop stopped. Serialised as [`ScrollOutcome::reason`] so the UI can
/// branch without parsing prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// The content stopped moving: the bottom of the page.
    End,
    /// `scroll_max_frames` was reached with content still to go.
    MaxFrames,
    /// `cancel_scroll_capture` was called.
    Cancelled,
    /// Two consecutive frames had no offset that matched. **This is the one
    /// that must never concatenate anyway.**
    NoOverlap,
    /// The window changed size mid-run, so the frames no longer line up.
    Resized,
    /// Something came in front of the target, so the wheel would have gone
    /// somewhere else.
    Obscured,
    /// The user moved the mouse — the documented way to abandon a run.
    PointerMoved,
    /// Windows stopped reporting the cursor position, so the run can no longer
    /// prove where its synthetic wheel is going. Distinct from
    /// [`Stop::PointerMoved`]: telling the user they moved the mouse when they
    /// did not is exactly the kind of confident-and-wrong message this module
    /// exists to avoid.
    PointerLost,
    /// `SendInput` refused the wheel event. Distinct from
    /// [`Stop::CaptureFailed`]: nothing was captured badly, the scroll simply
    /// never happened, and the two have completely different fixes.
    WheelBlocked,
    /// The stitched image hit [`MAX_STITCH_HEIGHT`] / [`MAX_STITCH_PIXELS`].
    HeightLimit,
    /// A grab failed part-way through. The frames before it are still kept.
    CaptureFailed,
}

impl Stop {
    fn code(self) -> &'static str {
        match self {
            Stop::End => "end",
            Stop::MaxFrames => "maxFrames",
            Stop::Cancelled => "cancelled",
            Stop::NoOverlap => "noOverlap",
            Stop::Resized => "resized",
            Stop::Obscured => "obscured",
            Stop::PointerMoved => "pointerMoved",
            Stop::PointerLost => "pointerLost",
            Stop::WheelBlocked => "wheelBlocked",
            Stop::HeightLimit => "heightLimit",
            Stop::CaptureFailed => "captureFailed",
        }
    }

    /// Whether the run ended somewhere other than the bottom of the content.
    fn incomplete(self) -> bool {
        self != Stop::End
    }
}

/// What a finished run produced, and how honest it is about it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrollOutcome {
    /// The same [`CaptureRecord`] every other capture produces — it went through
    /// `finalize`, so it is named, saved, copied, thumbnailed and in history.
    pub record: CaptureRecord,
    /// Frames grabbed, including ones that turned out to be duplicates.
    pub frames: u32,
    /// Frames actually merged into the image. 1 means nothing scrolled.
    pub stitched: u32,
    pub width: u32,
    pub height: u32,
    /// Machine-readable stop reason — see [`Stop::code`].
    pub reason: String,
    /// One sentence for the user. Always populated, including on success.
    pub message: String,
    /// `true` whenever the run ended before the bottom of the content, or
    /// nothing scrolled at all. The UI must not present these as complete.
    pub incomplete: bool,
}

/// Frames captured so far, emitted on [`PROGRESS_EVENT`] after every grab.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScrollProgress {
    pub frames: u32,
    pub stitched: u32,
    pub max_frames: u32,
    /// Height of the stitched image so far, in pixels.
    pub height: u32,
}

/// One run at a time, and a cancel flag it polls. Managed state, registered in
/// `lib.rs`.
#[derive(Default)]
pub struct ScrollState {
    running: AtomicBool,
    cancel: AtomicBool,
}

/// Clears `running` on every exit path, including a panic unwinding out of the
/// loop. A stuck flag would mean scrolling capture never works again until the
/// app is restarted.
struct RunGuard<'a>(&'a ScrollState);

impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// commands
// ---------------------------------------------------------------------------

/// Scroll-capture the window with this id (an id from `list_windows`).
///
/// Long-running by construction — `scroll_max_frames` × `scroll_delay_ms` is
/// tens of seconds — so it runs on a blocking worker and reports progress on
/// [`PROGRESS_EVENT`] while it goes.
#[tauri::command]
pub async fn start_scroll_capture(app: AppHandle, id: u32) -> Result<ScrollOutcome, String> {
    run_blocking(move || run(&app, id)).await
}

/// Stop the run in flight. Prompt: the settle delay polls this every
/// [`CANCEL_POLL`], so the worst case is one poll plus one grab.
///
/// Whatever was stitched before the cancel is still finalized — a cancel is
/// "stop here", not "throw away the last thirty seconds". The outcome comes back
/// with `reason: "cancelled"` and `incomplete: true`.
#[tauri::command]
pub fn cancel_scroll_capture(app: AppHandle) {
    app.state::<ScrollState>().cancel.store(true, Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// the loop
// ---------------------------------------------------------------------------

fn run(app: &AppHandle, id: u32) -> Result<ScrollOutcome, String> {
    let state: &ScrollState = app.state::<ScrollState>().inner();
    if state.running.swap(true, Ordering::SeqCst) {
        return Err("a scrolling capture is already running".to_string());
    }
    let _guard = RunGuard(state);
    state.cancel.store(false, Ordering::SeqCst);

    // Always-on-top and therefore capable of sitting over the target, exactly as
    // it would sit in a region freeze frame (M2.9 §3).
    crate::hide_preview_for_capture(app);

    let (delay, step, max_frames) = {
        let app_state = app.state::<AppState>();
        let settings = lock(&app_state.settings);
        (
            Duration::from_millis(settings.scroll_delay_ms.min(DELAY_MAX_MS) as u64),
            // `abs` rather than reject: a negative step is a hand-edited file
            // asking to scroll up, which the stitcher cannot express, and
            // scrolling down is the only sensible reading of "3 notches".
            settings.scroll_step.saturating_abs().clamp(STEP_MIN, STEP_MAX),
            settings.scroll_max_frames.clamp(FRAMES_MIN, FRAMES_MAX),
        )
    };

    let hwnd = hwnd_from_id(id);
    if owned_by_us(hwnd) {
        return Err(
            "that is one of santi.sharex's own windows — pick the window you want to capture"
                .to_string(),
        );
    }

    // Raised *before* the first grab, not after: a window that comes to the
    // front repaints, and a first frame captured behind something else would
    // not match the second one.
    // Best effort, and its failure is not an error: it only works because *we*
    // are the foreground process at the moment the user starts the capture, and
    // `target_under_point` below is the check that actually matters.
    let _ = unsafe { SetForegroundWindow(hwnd) };
    std::thread::sleep(FOCUS_SETTLE);

    let shot = capture::capture_window_by_id(id)?;
    let (width, height) = shot.image.dimensions();
    let geometry = Strips::for_frame(width, height).ok_or_else(|| {
        format!(
            "that window is {width}x{height} — too small to measure a scroll against. Scrolling capture needs a window at least a few hundred pixels tall."
        )
    })?;

    // Middle of the window: the best generic guess at "over the scrollable
    // thing", and far from the title bar, the tab strip and any status bar.
    let point = POINT {
        x: shot.origin_x + (width / 2) as i32,
        y: shot.origin_y + (height / 2) as i32,
    };
    let _pointer = park_pointer(point)?;
    if !target_under_point(hwnd, point) {
        return Err(
            "another window is in front of the one you picked, so the scroll would have gone to the wrong app. Bring it to the front and try again."
                .to_string(),
        );
    }

    let mut canvas: Vec<u8> = shot.image.as_raw().clone();
    let mut stitched_height = height;
    let mut previous = Luma::of(&shot.image);
    let mut frames: u32 = 1;
    let mut stitched: u32 = 1;
    let mut still_streak: u32 = 0;
    // Kept out of `Stop` so the enum stays a plain code: the details belong in
    // the sentence, not in the tag.
    let mut detail = String::new();

    emit_progress(app, frames, stitched, max_frames, stitched_height);

    let stop = loop {
        if state.cancel.load(Ordering::SeqCst) {
            break Stop::Cancelled;
        }
        if frames >= max_frames {
            break Stop::MaxFrames;
        }
        match pointer_at(point) {
            PointerCheck::Moved => break Stop::PointerMoved,
            PointerCheck::Unknown => break Stop::PointerLost,
            PointerCheck::Parked => {}
        }
        if !target_under_point(hwnd, point) {
            break Stop::Obscured;
        }

        if let Err(e) = wheel_down(step) {
            detail = e;
            break Stop::WheelBlocked;
        }
        if !settle(state, delay) {
            break Stop::Cancelled;
        }

        let next = match capture::capture_window_by_id(id) {
            Ok(shot) => shot.image,
            Err(e) => {
                detail = e;
                break Stop::CaptureFailed;
            }
        };
        frames += 1;

        if next.dimensions() != (width, height) {
            break Stop::Resized;
        }

        let current = Luma::of(&next);
        match measure_overlap(&previous, &current, &geometry) {
            Overlap::Still => {
                still_streak += 1;
                previous = current;
                emit_progress(app, frames, stitched, max_frames, stitched_height);
                if still_streak >= STILL_CONFIRMATIONS {
                    break Stop::End;
                }
            }
            Overlap::Unmatched { score } => {
                detail = format!("best score {score:.1}, needed {MAX_MATCH_SCORE:.0} or better");
                break Stop::NoOverlap;
            }
            Overlap::Moved { shift } => {
                still_streak = 0;
                let grown = stitched_height as u64 + shift as u64;
                if grown > MAX_STITCH_HEIGHT as u64
                    || grown.saturating_mul(width as u64) > MAX_STITCH_PIXELS
                {
                    break Stop::HeightLimit;
                }

                append_new_rows(&mut canvas, &next, shift);
                stitched_height += shift;
                stitched += 1;
                previous = current;
                emit_progress(app, frames, stitched, max_frames, stitched_height);
            }
        }
    };

    let image = RgbaImage::from_raw(width, stitched_height, canvas).ok_or_else(|| {
        format!("the stitched {width}x{stitched_height} image did not add up — nothing was saved")
    })?;

    // The same one pipeline as every other capture: filename pattern, clipboard
    // copy, thumbnail, history record, `capture://new`, preview, editor.
    let record = finalize(app, image, "scroll")?;

    Ok(ScrollOutcome {
        record,
        frames,
        stitched,
        width,
        height: stitched_height,
        reason: stop.code().to_string(),
        message: describe(stop, stitched, &detail),
        incomplete: stop.incomplete() || stitched < 2,
    })
}

/// The sentence the UI shows. Every stop reason has one, and every early stop
/// says both *that* it stopped early and *why*.
fn describe(stop: Stop, stitched: u32, detail: &str) -> String {
    let n = frame_count(stitched);
    // Bases are written as finished sentences, so a detail replaces the full
    // stop rather than trailing after it.
    let with_detail = |base: String| {
        if detail.is_empty() {
            base
        } else {
            format!("{} ({detail}).", base.trim_end_matches('.'))
        }
    };

    match stop {
        Stop::End if stitched < 2 => {
            "The content never moved, so this is a single frame — the window may not scroll with the wheel, or it was already at the bottom.".to_string()
        }
        Stop::End => format!("Reached the bottom of the content after {n}."),
        // `n` is frames *stitched*, which is not the budget — still frames and
        // retries spend the budget without stitching — so the sentence does not
        // pretend it is.
        Stop::MaxFrames => format!(
            "Stopped after {n} at the frame budget, with content still below — raise Max frames in Settings to go further."
        ),
        Stop::Cancelled => format!("Cancelled — kept the {n} captured so far."),
        Stop::NoOverlap => with_detail(format!(
            "Stopped after {n}: the last two frames had no overlap to stitch on, so the rest was left out rather than joined blindly. Virtualised lists, parallax and animated content do this."
        )),
        Stop::Resized => format!(
            "Stopped after {n}: the window changed size mid-capture, so the frames no longer line up."
        ),
        Stop::Obscured => format!(
            "Stopped after {n}: something came in front of the window, so the scroll would have gone to the wrong app."
        ),
        Stop::PointerMoved => format!(
            "Stopped after {n}: the mouse moved off the window. Scrolling capture drives the real pointer, so moving it ends the run."
        ),
        Stop::PointerLost => format!(
            "Stopped after {n}: Windows stopped reporting the pointer position, so there was no way to be sure the scroll was still going to the right window."
        ),
        Stop::WheelBlocked => with_detail(format!(
            "Stopped after {n}: the scroll itself was refused, so nothing after this point moved."
        )),
        Stop::HeightLimit => {
            format!("Stopped after {n} at the {MAX_STITCH_HEIGHT}px height limit.")
        }
        Stop::CaptureFailed => with_detail(format!(
            "Stopped after {n}: a grab failed, so the frames before it were kept."
        )),
    }
}

fn frame_count(n: u32) -> String {
    if n == 1 {
        "1 frame".to_string()
    } else {
        format!("{n} frames")
    }
}

fn emit_progress(app: &AppHandle, frames: u32, stitched: u32, max_frames: u32, height: u32) {
    let _ = app.emit(
        PROGRESS_EVENT,
        ScrollProgress {
            frames,
            stitched,
            max_frames,
            height,
        },
    );
}

/// Sleeps `total`, waking every [`CANCEL_POLL`] to look at the cancel flag.
/// Returns `false` if the run was cancelled.
fn settle(state: &ScrollState, total: Duration) -> bool {
    let mut left = total;
    while !left.is_zero() {
        if state.cancel.load(Ordering::SeqCst) {
            return false;
        }
        let slice = left.min(CANCEL_POLL);
        std::thread::sleep(slice);
        left -= slice;
    }
    !state.cancel.load(Ordering::SeqCst)
}

// ---------------------------------------------------------------------------
// stitching
// ---------------------------------------------------------------------------

/// Appends the bottom `shift` rows of `next` — the only rows it holds that the
/// canvas does not already have.
///
/// The measurement says prev row `shift + k` is next row `k`, so next rows
/// `[0, h - shift)` are a repeat of what is already stitched and rows
/// `[h - shift, h)` are new.
fn append_new_rows(canvas: &mut Vec<u8>, next: &RgbaImage, shift: u32) {
    let (w, h) = next.dimensions();
    let shift = shift.min(h);
    let stride = w as usize * 4;
    let start = (h - shift) as usize * stride;
    canvas.extend_from_slice(&next.as_raw()[start..]);
}

// ---------------------------------------------------------------------------
// overlap measurement
// ---------------------------------------------------------------------------

/// One frame reduced to a single-channel plane. Built once per frame rather
/// than per candidate offset — the scorer touches the same rows a thousand
/// times, and doing the RGB→luma maths inside that loop would dominate it.
struct Luma {
    width: u32,
    px: Vec<u8>,
}

impl Luma {
    fn of(img: &RgbaImage) -> Self {
        let (width, height) = img.dimensions();
        let mut px = Vec::with_capacity((width as usize) * (height as usize));
        for chunk in img.as_raw().chunks_exact(4) {
            // Rec.601 in fixed point. Exactness does not matter here; what
            // matters is that two identical pixels map to the same byte.
            let v = (77 * chunk[0] as u32 + 150 * chunk[1] as u32 + 29 * chunk[2] as u32) >> 8;
            px.push(v as u8);
        }
        Self { width, px }
    }

    #[inline]
    fn at(&self, x: u32, y: u32) -> u8 {
        self.px[(y as usize) * (self.width as usize) + (x as usize)]
    }
}

/// Where the comparison strips sit inside a frame, and how far the search runs.
///
/// The margins are not decoration. The top one skips a fixed header or tab
/// strip, which repeats identically in every frame and would otherwise pull the
/// match towards zero; the bottom one skips a status bar; the side ones skip the
/// scrollbar, whose thumb moves by a *different* amount than the content and is
/// the single most misleading thing in the frame.
#[derive(Debug, Clone, Copy)]
struct Strips {
    top: u32,
    height: u32,
    left: u32,
    width: u32,
    max_shift: u32,
}

impl Strips {
    fn for_frame(w: u32, h: u32) -> Option<Self> {
        let v_margin = (h / 10).min(48);
        let usable = h.checked_sub(v_margin.saturating_mul(2))?;
        if usable < MIN_STRIP {
            return None;
        }
        let height = (usable / 3).clamp(MIN_STRIP, MAX_STRIP).min(usable);
        let max_shift = usable - height;
        if max_shift < MIN_MOVE {
            return None;
        }

        let h_margin = (w / 16).min(32);
        let width = w.checked_sub(h_margin.saturating_mul(2))?;
        if width < MIN_STRIP_WIDTH {
            return None;
        }

        Some(Self {
            top: v_margin,
            height,
            left: h_margin,
            width,
            max_shift,
        })
    }
}

/// What one frame pair says about how far the content moved.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Overlap {
    /// Content moved down by `shift` pixels.
    Moved { shift: u32 },
    /// The best match is at (near) zero: nothing moved.
    Still,
    /// No candidate was a confident match. Stitching stops here — `score` is
    /// what the message tells the user.
    Unmatched { score: f32 },
}

/// Scores every candidate shift and applies the two confidence guards.
///
/// `prev` and `next` must be the same size, and `geometry` must have come from
/// [`Strips::for_frame`] with that size.
fn measure_overlap(prev: &Luma, next: &Luma, geometry: &Strips) -> Overlap {
    let mut scores: Vec<f32> = Vec::with_capacity(geometry.max_shift as usize + 1);
    for shift in 0..=geometry.max_shift {
        scores.push(score_at(prev, next, geometry, shift));
    }

    // Strictly less-than, scanning upwards, so a tie resolves to the smallest
    // shift. Repeating a few rows is recoverable; skipping them is not.
    let mut best = 0u32;
    for (shift, &score) in scores.iter().enumerate() {
        if score < scores[best as usize] {
            best = shift as u32;
        }
    }
    let best_score = scores[best as usize];

    // The best score more than RIVAL_GUARD away from the winner. If the winner
    // is not clearly better than that, the strip matched everywhere and the
    // "match" carries no information.
    let rival = scores
        .iter()
        .enumerate()
        .filter(|(shift, _)| (*shift as i64 - best as i64).unsigned_abs() > RIVAL_GUARD as u64)
        .map(|(_, &score)| score)
        .fold(f32::INFINITY, f32::min);

    let distinctive = best_score <= NEAR_PERFECT || best_score * DISTINCT_MARGIN <= rival;
    if best_score > MAX_MATCH_SCORE || !distinctive {
        return Overlap::Unmatched { score: best_score };
    }
    if best < MIN_MOVE {
        return Overlap::Still;
    }
    Overlap::Moved { shift: best }
}

/// Mean absolute difference between `next`'s strip and `prev`'s strip moved
/// down by `shift`, over a fixed subsampled grid.
///
/// The grid is the same size for every `shift`, which is the whole point of a
/// fixed-height strip: scores from different offsets are only comparable when
/// they average the same number of samples.
fn score_at(prev: &Luma, next: &Luma, g: &Strips, shift: u32) -> f32 {
    let mut total: u64 = 0;
    let mut count: u32 = 0;

    for r in 0..SAMPLE_ROWS {
        let dy = (r * g.height) / SAMPLE_ROWS;
        let ny = g.top + dy;
        let py = ny + shift;
        for c in 0..SAMPLE_COLS {
            let x = g.left + (c * g.width) / SAMPLE_COLS;
            let a = next.at(x, ny) as i32;
            let b = prev.at(x, py) as i32;
            total += (a - b).unsigned_abs() as u64;
            count += 1;
        }
    }

    if count == 0 {
        return f32::INFINITY;
    }
    total as f32 / count as f32
}

// ---------------------------------------------------------------------------
// Win32: the wheel and the pointer
// ---------------------------------------------------------------------------

/// xcap's window id *is* the `HWND`, truncated to 32 bits (`impl_window.rs`
/// does `hwnd.0 as u32`). Window handles are 32-bit-significant on Win64 by
/// design — that is what makes them interop-safe — so this round-trips.
fn hwnd_from_id(id: u32) -> HWND {
    HWND(id as usize as *mut std::ffi::c_void)
}

fn owned_by_us(hwnd: HWND) -> bool {
    let mut pid: u32 = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid != 0 && pid == std::process::id()
}

/// Whether `hwnd` — or one of its children — is what the user would hit by
/// clicking at `point`.
fn target_under_point(hwnd: HWND, point: POINT) -> bool {
    let hit = unsafe { WindowFromPoint(point) };
    if hit.is_invalid() {
        return false;
    }
    hit == hwnd || unsafe { GetAncestor(hit, GA_ROOT) } == hwnd
}

fn cursor_pos() -> Option<POINT> {
    let mut point = POINT::default();
    unsafe { GetCursorPos(&mut point) }.ok()?;
    Some(point)
}

enum PointerCheck {
    Parked,
    Moved,
    Unknown,
}

fn pointer_at(point: POINT) -> PointerCheck {
    match cursor_pos() {
        None => PointerCheck::Unknown,
        Some(now) => {
            if (now.x - point.x).abs() <= POINTER_TOLERANCE
                && (now.y - point.y).abs() <= POINTER_TOLERANCE
            {
                PointerCheck::Parked
            } else {
                PointerCheck::Moved
            }
        }
    }
}

/// The pointer moved to the park point, and where it came from.
struct ParkedPointer {
    parked: POINT,
    restore: POINT,
}

impl Drop for ParkedPointer {
    fn drop(&mut self) {
        // Only if it is still where we left it. Moving the mouse is how a run is
        // abandoned, so a user who has already grabbed the mouse must not have it
        // yanked out from under them on the way out.
        if let PointerCheck::Parked = pointer_at(self.parked) {
            let _ = unsafe { SetCursorPos(self.restore.x, self.restore.y) };
        }
    }
}

/// Parks the pointer over the target and proves it actually landed there.
///
/// The read-back is not paranoia. `SetCursorPos` takes physical screen pixels
/// only for a per-monitor-DPI-aware process; under any other awareness mode
/// Windows silently rescales the coordinates, and a pointer parked on the wrong
/// monitor scrolls whatever happens to be there. Catching that here is the
/// difference between a clear error and a capture of somebody else's window.
fn park_pointer(point: POINT) -> Result<ParkedPointer, String> {
    let restore = cursor_pos()
        .ok_or_else(|| "Windows would not report the pointer position".to_string())?;

    unsafe { SetCursorPos(point.x, point.y) }
        .map_err(|e| format!("could not move the pointer over that window: {e}"))?;

    match cursor_pos() {
        Some(now)
            if (now.x - point.x).abs() <= POINTER_TOLERANCE
                && (now.y - point.y).abs() <= POINTER_TOLERANCE => {}
        Some(now) => {
            let _ = unsafe { SetCursorPos(restore.x, restore.y) };
            return Err(format!(
                "the pointer was asked for {},{} and landed at {},{} — scrolling capture drives the real pointer, so it stopped rather than scroll the wrong thing",
                point.x, point.y, now.x, now.y
            ));
        }
        None => return Err("Windows would not report the pointer position".to_string()),
    }

    Ok(ParkedPointer {
        parked: point,
        restore,
    })
}

/// One synthetic wheel-down of `notches`.
///
/// Negative `mouseData` is rotation toward the user, i.e. scrolling down, which
/// is the only direction the stitcher can express.
fn wheel_down(notches: i32) -> Result<(), String> {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: (-notches.saturating_mul(WHEEL_NOTCH)) as u32,
                dwFlags: MOUSEEVENTF_WHEEL,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };

    let sent = unsafe { SendInput(&[input], std::mem::size_of::<INPUT>() as i32) };
    if sent == 1 {
        Ok(())
    } else {
        Err("Windows blocked the synthetic wheel event — something with a higher integrity level may have the input desktop".to_string())
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgba;

    /// Deterministic per-pixel noise. Real enough that a wrong offset scores
    /// badly, reproducible enough that a failure is debuggable.
    fn noise(seed: u64, x: u32, y: u32) -> u8 {
        let mut h = seed
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add((x as u64) << 32)
            .wrapping_add(y as u64 * 0x0000_1000_0000_0001);
        h ^= h >> 33;
        h = h.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
        h ^= h >> 29;
        (h & 0xFF) as u8
    }

    fn textured(seed: u64, w: u32, h: u32, y_offset: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, y| {
            let v = noise(seed, x, y + y_offset);
            Rgba([v, v.wrapping_add(40), v.wrapping_sub(20), 255])
        })
    }

    #[test]
    fn recovers_a_known_shift_exactly() {
        let (w, h) = (200u32, 300u32);
        let geometry = Strips::for_frame(w, h).expect("geometry");

        for shift in [2u32, 3, 17, 40, 96, 137] {
            assert!(shift <= geometry.max_shift, "{shift} outside the search");
            let prev = Luma::of(&textured(7, w, h, 0));
            let next = Luma::of(&textured(7, w, h, shift));
            match measure_overlap(&prev, &next, &geometry) {
                Overlap::Moved { shift: got } => assert_eq!(got, shift),
                other => panic!("shift {shift} came back as {other:?}"),
            }
        }
    }

    #[test]
    fn identical_frames_read_as_still() {
        let (w, h) = (200u32, 300u32);
        let geometry = Strips::for_frame(w, h).expect("geometry");
        let frame = textured(11, w, h, 0);
        let prev = Luma::of(&frame);
        let next = Luma::of(&frame);
        assert!(matches!(
            measure_overlap(&prev, &next, &geometry),
            Overlap::Still
        ));
    }

    #[test]
    fn unrelated_frames_have_no_confident_overlap() {
        let (w, h) = (200u32, 300u32);
        let geometry = Strips::for_frame(w, h).expect("geometry");
        let prev = Luma::of(&textured(1, w, h, 0));
        let next = Luma::of(&textured(999_983, w, h, 0));
        match measure_overlap(&prev, &next, &geometry) {
            Overlap::Unmatched { .. } => {}
            other => panic!("unrelated frames matched as {other:?}"),
        }
    }

    /// The case the honesty requirement is really about: a frame pair that is
    /// half plausible. A flat fill matches at *every* offset, so there is no
    /// information in the winner and it must not be trusted.
    #[test]
    fn a_featureless_frame_pair_is_not_a_match() {
        let (w, h) = (200u32, 300u32);
        let geometry = Strips::for_frame(w, h).expect("geometry");
        let flat = RgbaImage::from_pixel(w, h, Rgba([200, 200, 200, 255]));
        let mut moved = flat.clone();
        // One stripe, well outside the sampled strip, so the strips themselves
        // stay uniform.
        for x in 0..w {
            moved.put_pixel(x, 1, Rgba([0, 0, 0, 255]));
        }
        // The premise, asserted rather than assumed: the stripe really is
        // outside the sampled band. Without this the test could pass for the
        // wrong reason if the margins ever changed.
        assert!(geometry.top > 1, "the stripe is inside the sampled strip");

        let prev = Luma::of(&flat);
        let next = Luma::of(&moved);
        // Uniform strips match perfectly everywhere, so the winner is shift 0
        // and the honest reading is "it did not move" — never a confident
        // non-zero shift invented out of noise.
        assert!(matches!(
            measure_overlap(&prev, &next, &geometry),
            Overlap::Still
        ));
    }

    /// The DISTINCTIVENESS guard on its own.
    ///
    /// Every other test here is decided by something else: the absolute
    /// [`MAX_MATCH_SCORE`] cut-off rejects unrelated frames, and a featureless
    /// pair ties at shift 0 and reads as `Still`. Neither exercises
    /// [`DISTINCT_MARGIN`], so without this the rival test could be deleted
    /// wholesale and the suite would stay green.
    ///
    /// Two frames of independent low-amplitude noise over the same flat
    /// background score *plausibly* — comfortably inside the absolute cut-off —
    /// at every single offset. There is a numerical winner, and it means
    /// nothing. Stitching on it would splice unrelated content together, which
    /// is precisely the blind concatenation M5 §4 forbids.
    #[test]
    fn a_plausible_but_undistinguished_winner_is_refused() {
        let (w, h) = (200u32, 300u32);
        let geometry = Strips::for_frame(w, h).expect("geometry");

        // Amplitude chosen so the mean absolute difference lands around 6:
        // well under MAX_MATCH_SCORE (12), well over NEAR_PERFECT (2.5).
        let grain = |seed: u64| {
            RgbaImage::from_fn(w, h, |x, y| {
                let v = 128 + (noise(seed, x, y) % 17);
                Rgba([v, v, v, 255])
            })
        };
        let prev = Luma::of(&grain(21));
        let next = Luma::of(&grain(22));

        match measure_overlap(&prev, &next, &geometry) {
            Overlap::Unmatched { score } => {
                // The absolute guard must NOT be what rejected this, or the
                // test is just `unrelated_frames_have_no_confident_overlap`
                // again under another name.
                assert!(
                    score <= MAX_MATCH_SCORE,
                    "score {score} tripped the absolute cut-off, so the rival test went untested"
                );
                assert!(
                    score > NEAR_PERFECT,
                    "score {score} is near-perfect, which bypasses the rival test"
                );
            }
            other => panic!("an undistinguished winner was accepted as {other:?}"),
        }
    }

    /// The mirror of the above: a real match must survive the rival test even
    /// though the frames are not pixel-identical. Otherwise the guard could be
    /// tightened until it rejected everything and both tests would still pass.
    #[test]
    fn a_real_shift_survives_the_rival_test_despite_noise() {
        let (w, h) = (200u32, 300u32);
        let geometry = Strips::for_frame(w, h).expect("geometry");
        let shift = 23u32;

        // The same content, moved, plus a little per-frame grain — a repaint is
        // never bit-identical once subpixel text rendering is involved.
        let frame = |y_offset: u32, seed: u64| {
            RgbaImage::from_fn(w, h, |x, y| {
                let base = noise(7, x, y + y_offset) as u32;
                let jitter = (noise(seed, x, y) % 5) as u32;
                let v = ((base + jitter) % 256) as u8;
                Rgba([v, v.wrapping_add(40), v.wrapping_sub(20), 255])
            })
        };
        let prev = Luma::of(&frame(0, 101));
        let next = Luma::of(&frame(shift, 102));

        match measure_overlap(&prev, &next, &geometry) {
            Overlap::Moved { shift: got } => assert_eq!(got, shift),
            other => panic!("a real {shift}px shift under noise was refused as {other:?}"),
        }
    }

    /// Content that repeats vertically is the one thing the scorer genuinely
    /// cannot resolve — a scroll of exactly one period is indistinguishable
    /// from no scroll at all. Pinned so the behaviour is a known quantity
    /// rather than a surprise: it must read as `Still` (which ends the run and
    /// keeps what was stitched), and must NEVER invent a confident shift.
    #[test]
    fn periodic_content_never_invents_a_shift() {
        let (w, h) = (200u32, 300u32);
        let geometry = Strips::for_frame(w, h).expect("geometry");
        let period = 20u32;

        let banded = |y_offset: u32| {
            RgbaImage::from_fn(w, h, |x, y| {
                let v = noise(5, x, (y + y_offset) % period);
                Rgba([v, v, v, 255])
            })
        };
        let prev = Luma::of(&banded(0));
        let next = Luma::of(&banded(period));

        match measure_overlap(&prev, &next, &geometry) {
            Overlap::Still => {}
            Overlap::Unmatched { .. } => {}
            Overlap::Moved { shift } => {
                panic!("perfectly periodic content produced a confident shift of {shift}")
            }
        }
    }

    #[test]
    fn stitching_a_known_shift_reproduces_the_source() {
        let (w, h, shift) = (64u32, 96u32, 9u32);
        let first = textured(3, w, h, 0);
        let second = textured(3, w, h, shift);

        let mut canvas = first.as_raw().clone();
        append_new_rows(&mut canvas, &second, shift);
        let stitched = RgbaImage::from_raw(w, h + shift, canvas).expect("stitched");

        let source = textured(3, w, h + shift, 0);
        assert_eq!(stitched.dimensions(), source.dimensions());
        assert_eq!(stitched.as_raw(), source.as_raw());
    }

    #[test]
    fn geometry_refuses_a_window_too_small_to_measure() {
        assert!(Strips::for_frame(400, 20).is_none());
        assert!(Strips::for_frame(4, 400).is_none());
        assert!(Strips::for_frame(800, 600).is_some());
    }

    #[test]
    fn the_search_never_looks_outside_the_frame() {
        for (w, h) in [(200u32, 300u32), (1920, 1080), (640, 480), (300, 200)] {
            let Some(g) = Strips::for_frame(w, h) else {
                continue;
            };
            let last_row_read = g.top + ((SAMPLE_ROWS - 1) * g.height) / SAMPLE_ROWS + g.max_shift;
            let last_col_read = g.left + ((SAMPLE_COLS - 1) * g.width) / SAMPLE_COLS;
            assert!(last_row_read < h, "{w}x{h} reads row {last_row_read}");
            assert!(last_col_read < w, "{w}x{h} reads column {last_col_read}");
        }
    }

    #[test]
    fn stop_reasons_are_honest_about_being_incomplete() {
        assert!(!Stop::End.incomplete());
        for stop in [
            Stop::MaxFrames,
            Stop::Cancelled,
            Stop::NoOverlap,
            Stop::Resized,
            Stop::Obscured,
            Stop::PointerMoved,
            Stop::PointerLost,
            Stop::WheelBlocked,
            Stop::HeightLimit,
            Stop::CaptureFailed,
        ] {
            assert!(stop.incomplete(), "{:?} claimed to be complete", stop);
            assert!(describe(stop, 4, "").ends_with('.'));
            assert!(describe(stop, 4, "the reason").ends_with('.'));
        }
        // A single-frame run never reads as a finished capture, whatever the
        // stop reason says.
        assert!(describe(Stop::End, 1, "").contains("never moved"));
        assert!(describe(Stop::Cancelled, 1, "").contains("1 frame "));
    }
}
