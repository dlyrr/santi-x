//! Frames in over a channel, one ffmpeg process out (M4 §3).
//!
//! # Shape
//!
//! ```text
//!   xcap DXGI thread ──sync_channel(0)──▶ pacer loop ──FrameQueue──▶ writer ──▶ ffmpeg stdin
//!                                        (this thread)  (bounded)    (thread)
//! ```
//!
//! The pacer is the only consumer of xcap's channel, and it is the only thing
//! that decides when a frame exists: the desktop duplication API only produces a
//! frame when something on screen *changed*, so a still desktop yields nothing
//! at all. Encoding what arrives would make a recording of a static window three
//! seconds long and one frame big. So the pacer ticks on a clock at the target
//! rate and emits whatever the newest frame is, repeating it when nothing moved.
//! That is also what makes the raw stream constant-rate, which is what
//! `-f rawvideo -r FPS` promises ffmpeg.
//!
//! # Dropping is the design, not the failure
//!
//! Frames go to the encoder through a [`FrameQueue`] with a hard capacity
//! derived from the frame size, not a number pulled out of the air. When the
//! encoder falls behind, the queue drops its oldest frame and counts it. It can
//! never grow: an app that eats RAM until it dies is worse than one that says it
//! dropped 4% of frames, and stale frames are the least useful thing in a live
//! recording, which is why the *oldest* is the one that goes.
//!
//! Note what is **not** counted as a drop: a 144 Hz desktop recorded at 30 fps
//! supersedes four frames out of five in the pacer, and that is downsampling,
//! not loss. Only frames the encoder could not be handed are reported.
//!
//! # Teardown
//!
//! Every wait here has a deadline and every deadline ends in `kill()`. A wedged
//! ffmpeg must not be able to hold the capture loop, the HUD or the app open —
//! see [`run`]'s teardown block, which takes the capture down *first* and only
//! then starts negotiating with the child process.
//!
//! Asking a second time shortens those deadlines rather than removing them, and
//! only once [`ESCALATE_GRACE`] has passed. Both halves of that matter and both
//! are about the same user: someone who taps a stop hotkey twice because they
//! are not sure the first press registered. Treating that reflex as "kill it
//! now" would kill ffmpeg before it wrote the MP4's `moov` atom, which is a file
//! that will not open — the exact failure the user was worried about, caused by
//! worrying about it.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use image::RgbaImage;
use xcap::{Frame, Monitor, VideoRecorder};

use super::ffmpeg::{self, Ffmpeg};
use crate::lock;

/// M4 §3: `fps` is 5..=60. Below 5 the clip is a slideshow whose timeline no
/// longer matches what the user saw; above 60 the raw pipe outruns any encoder
/// on this machine and every extra frame is a dropped one.
pub const FPS_MIN: u32 = 5;
pub const FPS_MAX: u32 = 60;

/// Bounds on the GIF's max width. The lower one keeps a hand-edited `0` from
/// asking ffmpeg to scale to nothing.
pub const GIF_WIDTH_MIN: u32 = 64;
pub const GIF_WIDTH_MAX: u32 = 3840;

/// How much RGBA the frame queue may hold before it starts dropping. Sixty-four
/// megabytes is two frames of 4K or eighty frames of a 640×360 region — the
/// point is that the ceiling is in *bytes*, so recording a whole 4K monitor
/// cannot quietly reserve a gigabyte the way a fixed frame count would.
const QUEUE_BUDGET_BYTES: usize = 64 * 1024 * 1024;
const QUEUE_MIN_FRAMES: usize = 2;
const QUEUE_MAX_FRAMES: usize = 90;

/// The pacer never sleeps longer than this in one go, so a stop is noticed
/// promptly even at 5 fps.
const TICK_SLICE: Duration = Duration::from_millis(80);

/// How far behind the clock the pacer may fall before it gives up on catching up
/// and re-bases its timeline. Without this, a machine that stalls for ten
/// seconds spends the next ten emitting frames as fast as it can.
const MAX_LAG: Duration = Duration::from_millis(750);

/// After the queue is closed, how long the writer gets to hand the last frames
/// over before the child is killed out from under it.
const WRITER_DEADLINE: Duration = Duration::from_secs(5);

/// After stdin closes, how long ffmpeg gets to flush and write its trailer. An
/// MP4 that never gets its `moov` atom is unplayable, so this is generous — but
/// it is finite, which is the whole point.
const CHILD_DEADLINE: Duration = Duration::from_secs(25);

/// The same two deadlines once the user has asked a second time. Short, because
/// they are insisting — but **not zero**, because zero means the MP4 is killed
/// before its trailer is written and the file will not open in anything. A
/// second press buys "seconds, not half a minute"; it does not buy a corpse.
const HURRIED_WRITER_DEADLINE: Duration = Duration::from_millis(750);
const HURRIED_CHILD_DEADLINE: Duration = Duration::from_secs(4);

/// How long after the first stop a second one counts as "I mean it" rather than
/// as one press the user was not sure landed. Below this the second request is
/// simply idempotent, which is what a stop hotkey tapped twice deserves.
const ESCALATE_GRACE: Duration = Duration::from_millis(1200);

/// How long the pacer will repeat the same frame before it stops trusting the
/// duplication and grabs the screen itself. See the reseed block in [`run`].
const RESEED_AFTER: Duration = Duration::from_millis(1800);

/// A GIF palette pass on a long clip is genuinely slow, so it gets its own,
/// much longer deadline. Still finite.
const PASS_DEADLINE: Duration = Duration::from_secs(600);

/// Deadline for pulling the thumbnail frame back out of the finished file.
const EXTRACT_DEADLINE: Duration = Duration::from_secs(30);

/// How many stderr lines are kept for the error message. ffmpeg's real
/// complaint is always in the last few.
const STDERR_LINES: usize = 40;

// ---------------------------------------------------------------------------
// job description
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Mp4,
    Gif,
}

impl Format {
    pub fn parse(raw: &str) -> Format {
        match raw.trim().to_ascii_lowercase().as_str() {
            "gif" => Format::Gif,
            _ => Format::Mp4,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Format::Mp4 => "mp4",
            Format::Gif => "gif",
        }
    }

    pub fn ext(self) -> &'static str {
        self.id()
    }

    /// The encoders a job in this format cannot run without, probed before the
    /// recording starts rather than discovered in a failed run (M4 §1).
    ///
    /// GIF needs libx264 too: the two palette passes both read a *file*, which a
    /// pipe cannot be asked to do twice, so the live half of a GIF recording
    /// writes a visually-lossless intermediate first. See [`run`].
    pub fn encoders(self) -> &'static [&'static str] {
        match self {
            Format::Mp4 => &[ffmpeg::ENCODER_X264],
            Format::Gif => &[ffmpeg::ENCODER_X264, ffmpeg::ENCODER_GIF],
        }
    }
}

/// A rect in xcap physical screen space — the same space `WindowRect` and the
/// freeze frame's origin live in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    /// Rounds the size down to even. `yuv420p` subsamples chroma 2×2, so libx264
    /// rejects an odd dimension outright — and `-pix_fmt yuv420p` is not
    /// optional (M4 §3: without it the file will not play in half the world's
    /// players). Losing one pixel row is the cheap side of that trade.
    pub fn to_even(self) -> Rect {
        Rect {
            x: self.x,
            y: self.y,
            width: self.width & !1,
            height: self.height & !1,
        }
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        let ax2 = self.x.saturating_add(self.width as i32);
        let ay2 = self.y.saturating_add(self.height as i32);
        let bx2 = other.x.saturating_add(other.width as i32);
        let by2 = other.y.saturating_add(other.height as i32);
        self.x < bx2 && other.x < ax2 && self.y < by2 && other.y < ay2
    }
}

/// Everything a run needs, resolved before the first frame. The rect in
/// particular is fixed here and never re-read: a window that moves mid-recording
/// keeps its original rect (M4 §2), and pretending otherwise would mean
/// re-negotiating the encoder's frame size mid-stream, which rawvideo cannot do.
#[derive(Debug, Clone)]
pub struct Job {
    pub ffmpeg: Ffmpeg,
    /// The monitor whose duplication feeds this recording.
    pub monitor_id: u32,
    /// Top-left of that monitor, so the rect can be turned into a crop offset.
    pub monitor_origin: (i32, i32),
    pub rect: Rect,
    pub fps: u32,
    pub format: Format,
    pub capture_cursor: bool,
    pub hw_encode: bool,
    /// Only consulted for [`Format::Gif`].
    pub gif_max_width: u32,
    /// Where the finished file goes.
    pub output: PathBuf,
    /// Scratch space for the GIF intermediate and palette. Cleaned up on every
    /// exit path, including a cancel.
    pub work_dir: PathBuf,
}

/// What a finished run produced.
#[derive(Debug, Clone, Default)]
pub struct Outcome {
    pub frames: u64,
    pub dropped: u64,
    /// The *file's* duration — frames divided by the frame rate — not wall
    /// clock. Dropped frames make a recording shorter than the time it took, and
    /// the number attached to the record has to describe the file the user can
    /// actually play.
    pub duration_ms: u32,
    pub width: u32,
    pub height: u32,
    /// ffmpeg had to be killed rather than exiting on its own, so the file may
    /// be short or (for MP4) missing its trailer. Reported, never hidden.
    pub truncated: bool,
    pub cancelled: bool,
    /// The thing being recorded stopped existing — the display was unplugged,
    /// its mode changed, or the session was switched. The recording ends itself
    /// and keeps what it had; this is why, so the ending can be explained rather
    /// than looking like the user's own stop.
    pub source_lost: Option<String>,
}

// ---------------------------------------------------------------------------
// control: how a recording stops
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Running,
    /// Finish and keep the file.
    Stopping,
    /// Finish and delete it.
    Cancelling,
}

/// What the three stop paths have collectively said, and when the first of them
/// said it.
#[derive(Debug, Clone, Copy)]
struct Wanted {
    mode: Mode,
    /// When the recording stopped being [`Mode::Running`]. `None` while it still
    /// is. Only [`Control::request_at`] reads it, to tell a deliberate second
    /// press from a reflex double-tap.
    since: Option<Instant>,
}

/// The one thing all three stop paths touch (M4 §4).
///
/// It is a mutex, a condvar and a flag — no window, no event loop, no child
/// process — which is exactly why the HUD button, the global hotkey and the tray
/// item each keep working when the other two do not. Whoever gets here first
/// wins; every later request is either a no-op or an escalation.
#[derive(Debug)]
pub struct Control {
    wanted: Mutex<Wanted>,
    changed: Condvar,
    /// Set when a stop arrives at something that has already been stopping for
    /// longer than [`ESCALATE_GRACE`] — "I asked twice, and I meant it".
    /// Shortens every remaining deadline, so a second press gets past a wedged
    /// ffmpeg instead of waiting out its full timer.
    escalated: AtomicBool,
}

impl Default for Control {
    fn default() -> Self {
        Self {
            wanted: Mutex::new(Wanted {
                mode: Mode::Running,
                since: None,
            }),
            changed: Condvar::new(),
            escalated: AtomicBool::new(false),
        }
    }
}

impl Control {
    pub fn mode(&self) -> Mode {
        lock(&self.wanted).mode
    }

    pub fn is_running(&self) -> bool {
        self.mode() == Mode::Running
    }

    pub fn escalated(&self) -> bool {
        self.escalated.load(Ordering::SeqCst)
    }

    /// Whether a run in progress should give up on whatever it is waiting for:
    /// the user has either asked twice or asked to throw the recording away.
    /// Consulted by the GIF passes, which are the one part of a recording that
    /// happens *after* the stop and can still take minutes.
    pub fn abandoning(&self) -> bool {
        self.escalated() || self.mode() == Mode::Cancelling
    }

    /// Idempotent by construction. A cancel always wins over a stop — "throw it
    /// away" is a stronger statement than "keep it" and the user only gets one
    /// chance to make it.
    pub fn request(&self, wanted: Mode) {
        self.request_at(wanted, Instant::now());
    }

    /// [`request`](Self::request) with the clock passed in, so the grace window
    /// can be tested without a test that sleeps through it.
    fn request_at(&self, wanted: Mode, now: Instant) {
        {
            let mut state = lock(&self.wanted);
            match (state.mode, wanted) {
                (Mode::Running, _) => {
                    state.mode = wanted;
                    state.since = Some(now);
                }
                // A cancel is never a double-tap: nothing is being kept, so
                // there is nothing a grace window could protect.
                (_, Mode::Cancelling) => {
                    self.escalated.store(true, Ordering::SeqCst);
                    state.mode = Mode::Cancelling;
                }
                // A second stop. Only an escalation once the first has had time
                // to visibly not work — see ESCALATE_GRACE.
                _ => {
                    let insistent = state
                        .since
                        .map(|since| now.saturating_duration_since(since) >= ESCALATE_GRACE)
                        .unwrap_or(true);
                    if insistent {
                        self.escalated.store(true, Ordering::SeqCst);
                    }
                }
            }
        }
        self.changed.notify_all();
    }

    /// Sleeps until `deadline`, waking early on any request. Returns the mode as
    /// of waking.
    fn sleep_until(&self, deadline: Instant) -> Mode {
        let mut state = lock(&self.wanted);
        while state.mode == Mode::Running {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let (next, _) = self
                .changed
                .wait_timeout(state, remaining.min(TICK_SLICE))
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
            if Instant::now() >= deadline {
                break;
            }
        }
        state.mode
    }
}

// ---------------------------------------------------------------------------
// progress
// ---------------------------------------------------------------------------

pub const PHASE_STARTING: u8 = 0;
pub const PHASE_RECORDING: u8 = 1;
pub const PHASE_FINISHING: u8 = 2;
pub const PHASE_ENCODING: u8 = 3;
pub const PHASE_DONE: u8 = 4;

pub fn phase_name(phase: u8) -> &'static str {
    match phase {
        PHASE_STARTING => "starting",
        PHASE_RECORDING => "recording",
        PHASE_FINISHING => "finishing",
        PHASE_ENCODING => "encoding",
        _ => "done",
    }
}

/// Counters the HUD reads on its own timer. Atomics rather than a channel: the
/// HUD asking "how far are we" must never be able to block the pacer.
#[derive(Debug, Default)]
pub struct Progress {
    pub frames: AtomicU64,
    pub dropped: AtomicU64,
    pub phase: AtomicU8,
}

impl Progress {
    pub fn set_phase(&self, phase: u8) {
        self.phase.store(phase, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------
// the bounded frame queue
// ---------------------------------------------------------------------------

/// A queue that cannot grow. See the module header for why dropping is the
/// design; `capacity` comes from [`queue_capacity`].
#[derive(Debug)]
pub struct FrameQueue {
    capacity: usize,
    inner: Mutex<QueueInner>,
    ready: Condvar,
}

#[derive(Debug, Default)]
struct QueueInner {
    frames: VecDeque<Vec<u8>>,
    closed: bool,
    dropped: u64,
}

impl FrameQueue {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            inner: Mutex::new(QueueInner::default()),
            ready: Condvar::new(),
        }
    }

    /// Never blocks and never grows past capacity. Returns `true` when the frame
    /// cost an older one its place.
    pub fn push(&self, frame: Vec<u8>) -> bool {
        let mut dropped_one = false;
        {
            let mut inner = lock(&self.inner);
            if inner.closed {
                // Nothing accepts frames after a close, but a frame that arrives
                // in that window is still a frame that did not get encoded.
                inner.dropped += 1;
                return true;
            }
            while inner.frames.len() >= self.capacity {
                inner.frames.pop_front();
                inner.dropped += 1;
                dropped_one = true;
            }
            inner.frames.push_back(frame);
        }
        self.ready.notify_one();
        dropped_one
    }

    /// Blocks until there is a frame, or until the queue is closed *and* drained
    /// — a close never throws away what is already queued, so a stop keeps the
    /// last frames the user saw.
    pub fn pop(&self) -> Option<Vec<u8>> {
        let mut inner = lock(&self.inner);
        loop {
            if let Some(frame) = inner.frames.pop_front() {
                return Some(frame);
            }
            if inner.closed {
                return None;
            }
            inner = self
                .ready
                .wait(inner)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    pub fn close(&self) {
        lock(&self.inner).closed = true;
        self.ready.notify_all();
    }

    /// Throws away what is queued as well as closing. Only the cancel path wants
    /// this: there is no point spending seconds writing frames into a file that
    /// is about to be deleted.
    pub fn abandon(&self) {
        let mut inner = lock(&self.inner);
        inner.closed = true;
        inner.frames.clear();
        self.ready.notify_all();
    }

    pub fn dropped(&self) -> u64 {
        lock(&self.inner).dropped
    }

    /// Queued frames. Only the drop-policy tests ask — the pacer deliberately
    /// never looks, because "is there room" is not a question it is allowed to
    /// act on: it pushes and the queue decides.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        lock(&self.inner).frames.len()
    }
}

/// Frames the queue may hold, from a byte budget rather than a fixed count.
pub fn queue_capacity(frame_bytes: usize) -> usize {
    if frame_bytes == 0 {
        return QUEUE_MIN_FRAMES;
    }
    (QUEUE_BUDGET_BYTES / frame_bytes).clamp(QUEUE_MIN_FRAMES, QUEUE_MAX_FRAMES)
}

// ---------------------------------------------------------------------------
// cropping
// ---------------------------------------------------------------------------

/// Copies a `dst_w × dst_h` window of `src`, starting at `(src_x, src_y)`, into
/// `dst`. Pixels outside the source are left as they were — `dst` is allocated
/// zeroed, so a rect that hangs off the edge of the monitor comes out black
/// rather than short.
///
/// "Rather than short" is the point: the output size is fixed for the whole
/// recording because rawvideo has no way to say a frame changed shape, so this
/// must always fill exactly `dst_w * dst_h * 4` bytes.
pub fn crop_into(
    dst: &mut [u8],
    dst_w: u32,
    dst_h: u32,
    src: &[u8],
    src_w: u32,
    src_h: u32,
    src_x: i32,
    src_y: i32,
) {
    if dst_w == 0 || dst_h == 0 || src_w == 0 || src_h == 0 {
        return;
    }
    for row in 0..dst_h {
        let sy = src_y + row as i32;
        if sy < 0 || sy >= src_h as i32 {
            continue;
        }
        // Horizontal overlap, in source columns.
        let start = src_x.max(0);
        let end = (src_x + dst_w as i32).min(src_w as i32);
        if end <= start {
            continue;
        }
        let count = (end - start) as usize;
        let src_off = ((sy as usize) * src_w as usize + start as usize) * 4;
        let dst_off = ((row as usize) * dst_w as usize + (start - src_x) as usize) * 4;
        dst[dst_off..dst_off + count * 4]
            .copy_from_slice(&src[src_off..src_off + count * 4]);
    }
}

// ---------------------------------------------------------------------------
// ffmpeg arguments
// ---------------------------------------------------------------------------

/// The input half, shared by every live encode: raw RGBA of a known size at a
/// known rate, on stdin.
fn raw_input_args(width: u32, height: u32, fps: u32) -> Vec<String> {
    vec![
        "-f".into(),
        "rawvideo".into(),
        "-pix_fmt".into(),
        "rgba".into(),
        "-s".into(),
        format!("{width}x{height}"),
        "-r".into(),
        fps.to_string(),
        "-i".into(),
        "-".into(),
    ]
}

/// M4 §3's MP4: libx264 by default, `-pix_fmt yuv420p` always, `+faststart`
/// always. Hardware encoding is opt-in and never the default — NVENC quality at
/// low bitrates is worse and the failure mode (a file that looks soft for no
/// visible reason) is confusing.
pub fn mp4_args(width: u32, height: u32, fps: u32, hw: bool, output: &Path) -> Vec<String> {
    let mut args = vec!["-loglevel".into(), "warning".into()];
    args.extend(raw_input_args(width, height, fps));
    // No audio track at all, rather than an empty one: M4 records the screen.
    args.push("-an".into());

    if hw {
        args.extend([
            "-c:v".into(),
            ffmpeg::ENCODER_NVENC.to_string(),
            "-preset".into(),
            "p5".into(),
            "-rc".into(),
            "vbr".into(),
            "-cq".into(),
            "24".into(),
            "-b:v".into(),
            "0".into(),
        ]);
    } else {
        args.extend([
            "-c:v".into(),
            ffmpeg::ENCODER_X264.to_string(),
            "-preset".into(),
            "veryfast".into(),
            "-crf".into(),
            "23".into(),
        ]);
    }

    args.extend([
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-movflags".into(),
        "+faststart".into(),
        "-y".into(),
        output.to_string_lossy().into_owned(),
    ]);
    args
}

/// The live half of a GIF recording: a visually-lossless MP4 the two palette
/// passes can then read *twice*.
///
/// A GIF cannot be made in one pass without looking like a dithered mess
/// (M4 §3), and both passes have to read the same footage — which a pipe cannot
/// be asked to do. `yuv444p` because the next thing that happens to these pixels
/// is a 256-colour palette being built from them, and chroma subsampling before
/// that step shows up as coloured fringing in the result.
pub fn gif_intermediate_args(width: u32, height: u32, fps: u32, output: &Path) -> Vec<String> {
    let mut args = vec!["-loglevel".into(), "warning".into()];
    args.extend(raw_input_args(width, height, fps));
    args.extend([
        "-an".into(),
        "-c:v".into(),
        ffmpeg::ENCODER_X264.to_string(),
        "-preset".into(),
        "ultrafast".into(),
        "-crf".into(),
        "18".into(),
        "-pix_fmt".into(),
        "yuv444p".into(),
        "-y".into(),
        output.to_string_lossy().into_owned(),
    ]);
    args
}

/// `Some(width)` when the recording is wider than the GIF ceiling. A 4K 30 fps
/// GIF is a several-hundred-megabyte file nobody wants (M4 §3).
pub fn plan_gif_width(source_width: u32, max_width: u32) -> Option<u32> {
    let max = max_width.clamp(GIF_WIDTH_MIN, GIF_WIDTH_MAX);
    (source_width > max).then_some(max)
}

/// The scale step both passes share. `null` rather than an empty string so the
/// filter graph is valid either way.
fn gif_scale_filter(target_width: Option<u32>) -> String {
    match target_width {
        Some(w) => format!("scale={w}:-1:flags=lanczos"),
        None => "null".to_string(),
    }
}

/// Pass one: build a palette from the whole clip. `stats_mode=diff` weights the
/// palette towards the parts of the frame that actually move, which is what a
/// screen recording is mostly made of.
pub fn palettegen_args(source: &Path, target_width: Option<u32>, palette: &Path) -> Vec<String> {
    vec![
        "-loglevel".into(),
        "warning".into(),
        "-nostdin".into(),
        "-i".into(),
        source.to_string_lossy().into_owned(),
        "-vf".into(),
        format!("{},palettegen=stats_mode=diff", gif_scale_filter(target_width)),
        "-y".into(),
        palette.to_string_lossy().into_owned(),
    ]
}

/// Pass two: encode against that palette. `diff_mode=rectangle` restricts
/// dithering to the changed rectangle of each frame, which stops a static
/// background from shimmering between frames.
pub fn paletteuse_args(
    source: &Path,
    palette: &Path,
    target_width: Option<u32>,
    output: &Path,
) -> Vec<String> {
    vec![
        "-loglevel".into(),
        "warning".into(),
        "-nostdin".into(),
        "-i".into(),
        source.to_string_lossy().into_owned(),
        "-i".into(),
        palette.to_string_lossy().into_owned(),
        "-lavfi".into(),
        format!(
            "[0:v]{}[x];[x][1:v]paletteuse=dither=bayer:bayer_scale=5:diff_mode=rectangle",
            gif_scale_filter(target_width)
        ),
        "-loop".into(),
        "0".into(),
        "-y".into(),
        output.to_string_lossy().into_owned(),
    ]
}

// ---------------------------------------------------------------------------
// child process plumbing
// ---------------------------------------------------------------------------

/// Last few lines of a child's stderr, drained on its own thread.
///
/// Draining is not optional: a pipe nobody reads fills, and an ffmpeg blocked
/// writing a warning into a full pipe is an ffmpeg that has stopped encoding.
/// That is a deadlock with no symptom other than a recording that never
/// finishes.
#[derive(Debug, Default, Clone)]
struct Stderr(Arc<Mutex<VecDeque<String>>>);

impl Stderr {
    fn drain(&self, child: &mut Child) {
        let Some(mut pipe) = child.stderr.take() else {
            return;
        };
        let sink = self.0.clone();
        std::thread::spawn(move || {
            let mut buf = String::new();
            let mut chunk = [0u8; 4096];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.push_str(&String::from_utf8_lossy(&chunk[..n])),
                }
                while let Some(at) = buf.find('\n') {
                    let line: String = buf.drain(..=at).collect();
                    let line = line.trim().to_string();
                    if line.is_empty() {
                        continue;
                    }
                    let mut sink = lock(&sink);
                    if sink.len() == STDERR_LINES {
                        sink.pop_front();
                    }
                    sink.push_back(line);
                }
            }
            if !buf.trim().is_empty() {
                let mut sink = lock(&sink);
                if sink.len() == STDERR_LINES {
                    sink.pop_front();
                }
                sink.push_back(buf.trim().to_string());
            }
        });
    }

    fn tail(&self) -> String {
        let sink = lock(&self.0);
        sink.iter().cloned().collect::<Vec<_>>().join("; ")
    }
}

fn spawn_ffmpeg(ffmpeg: &Ffmpeg, args: &[String], piped_stdin: bool) -> Result<Child, String> {
    let mut cmd = ffmpeg.command();
    cmd.args(args)
        .stdin(if piped_stdin {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    cmd.spawn()
        .map_err(|e| format!("could not start ffmpeg: {e}"))
}

/// Waits for a child, killing it once `deadline` passes or `abandon` says so.
/// `true` means it had to be killed.
///
/// `abandon` is what makes a wait interruptible: the GIF passes can legitimately
/// run for minutes, and a user who has asked twice must not have to sit through
/// the rest of one.
fn wait_or_kill_unless(
    child: &mut Child,
    deadline: Duration,
    abandon: &dyn Fn() -> bool,
) -> bool {
    let until = Instant::now() + deadline;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return false,
            Ok(None) => {}
            // We cannot ask about it any more, so there is nothing left to wait
            // for. Not a kill.
            Err(_) => return false,
        }
        if Instant::now() >= until || abandon() {
            let _ = child.kill();
            let _ = child.wait();
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Waits for a child, killing it once `deadline` passes. `true` means it had to
/// be killed.
fn wait_or_kill(child: &mut Child, deadline: Duration) -> bool {
    wait_or_kill_unless(child, deadline, &|| false)
}

/// Runs a short ffmpeg pass to completion, with a deadline. Used for the two GIF
/// passes and the thumbnail extraction — never for the live encode, which is fed
/// by [`run`] itself.
///
/// `abandon` is the second stop press arriving while a palette pass is grinding
/// through a five-minute clip. Without it the last leg of a GIF recording is the
/// one part of the whole feature the user cannot interrupt.
fn run_pass(
    ffmpeg: &Ffmpeg,
    args: &[String],
    deadline: Duration,
    what: &str,
    abandon: &dyn Fn() -> bool,
) -> Result<(), String> {
    let mut child = spawn_ffmpeg(ffmpeg, args, false)?;
    let stderr = Stderr::default();
    stderr.drain(&mut child);

    if wait_or_kill_unless(&mut child, deadline, abandon) {
        return Err(if abandon() {
            format!("the {what} pass was stopped before it finished")
        } else {
            format!(
                "the {what} pass did not finish within {}s and was stopped",
                deadline.as_secs()
            )
        });
    }
    match child.try_wait() {
        Ok(Some(status)) if !status.success() => {
            let tail = stderr.tail();
            Err(format!("the {what} pass failed ({status}): {tail}"))
        }
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// the xcap recorder, kept for the session
// ---------------------------------------------------------------------------

/// One duplication per monitor, reused for every recording of that monitor.
///
/// Not an optimisation. xcap 0.9.7's recorder thread parks on a condvar it only
/// checks between frames and sends on a rendezvous channel, so a recorder that
/// is dropped leaves a thread holding an `IDXGIOutputDuplication` alive for the
/// life of the process. Building a fresh one per recording would stack those up
/// and eventually be refused by DXGI. Building one per *monitor* and calling
/// `start`/`stop` on it keeps that bounded, which is what the API is shaped for
/// anyway.
///
/// The guard is held for the whole recording, which also enforces the
/// one-recording-at-a-time rule at the lowest possible level.
type Recorders = Mutex<HashMap<u32, (VideoRecorder, Receiver<Frame>)>>;

fn recorders() -> &'static Recorders {
    static RECORDERS: OnceLock<Recorders> = OnceLock::new();
    RECORDERS.get_or_init(|| Mutex::new(HashMap::new()))
}

// ---------------------------------------------------------------------------
// the run
// ---------------------------------------------------------------------------

pub fn run(job: &Job, control: &Control, progress: &Progress) -> Result<Outcome, String> {
    let width = job.rect.width;
    let height = job.rect.height;
    if width == 0 || height == 0 {
        return Err("that area is too small to record".into());
    }

    let monitor = monitor_by_id(job.monitor_id)?;
    let frame_bytes = (width as usize) * (height as usize) * 4;

    // The intermediate for GIF, the real thing for MP4.
    let encode_target = match job.format {
        Format::Mp4 => job.output.clone(),
        Format::Gif => job.work_dir.join(format!(
            "{}.intermediate.mp4",
            job.output
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "recording".into())
        )),
    };
    if let Some(parent) = encode_target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let args = match job.format {
        Format::Mp4 => mp4_args(width, height, job.fps, job.hw_encode, &encode_target),
        Format::Gif => gif_intermediate_args(width, height, job.fps, &encode_target),
    };

    // Everything that can fail *before* ffmpeg exists, does. An error after the
    // spawn would otherwise leave an orphaned encoder waiting on a pipe nobody
    // will ever write to — and, once the writer thread exists, a thread blocked
    // on a queue nobody will ever close.

    // A still desktop produces no duplication frames at all, so the timeline is
    // seeded with one real grab. Without it a recording that starts on a
    // motionless screen would simply be missing its first seconds.
    let mut source: (u32, u32, Vec<u8>) = match monitor.capture_image() {
        Ok(img) => (img.width(), img.height(), img.into_raw()),
        Err(e) => return Err(format!("could not read the screen: {e}")),
    };

    let mut recorders = lock(recorders());
    if !recorders.contains_key(&job.monitor_id) {
        let started = monitor
            .video_recorder()
            .map_err(|e| format!("could not start capturing this monitor: {e}"))?;
        recorders.insert(job.monitor_id, started);
    }
    let (recorder, rx) = recorders.get(&job.monitor_id).expect("just inserted");

    // xcap's channel is a rendezvous, so a frame from the *previous* recording
    // can still be parked in a blocked send. Take it and throw it away rather
    // than opening with a stale desktop.
    while rx.try_recv().is_ok() {}

    let mut child = spawn_ffmpeg(&job.ffmpeg, &args, true)?;
    let stderr = Stderr::default();
    stderr.drain(&mut child);
    let stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err("ffmpeg gave us no stdin to write frames to".into());
        }
    };

    let queue = Arc::new(FrameQueue::new(queue_capacity(frame_bytes)));
    let (writer_done, writer_result) = std::sync::mpsc::channel::<Option<String>>();
    {
        // Detached, never joined. If ffmpeg wedges mid-write this thread is
        // stuck in `write_all`, and the teardown below must not be stuck with
        // it — killing the child is what frees it.
        let queue = queue.clone();
        std::thread::spawn(move || {
            let mut stdin = stdin;
            let mut error = None;
            while let Some(frame) = queue.pop() {
                if let Err(e) = stdin.write_all(&frame) {
                    error = Some(e.to_string());
                    break;
                }
            }
            // Dropping stdin is the EOF ffmpeg waits for, and it has to happen
            // before the result is announced or the wait below is meaningless.
            drop(stdin);
            let _ = writer_done.send(error);
        });
    }

    if let Err(e) = recorder.start() {
        // The one failure left that can happen with a live child and a live
        // writer behind it, so it cleans both up itself.
        queue.abandon();
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_file(&encode_target);
        return Err(format!("could not start capturing this monitor: {e}"));
    }

    progress.set_phase(PHASE_RECORDING);

    let origin_x = job.rect.x - job.monitor_origin.0;
    let origin_y = job.rect.y - job.monitor_origin.1;

    let started = Instant::now();
    let period = Duration::from_nanos(1_000_000_000 / job.fps.clamp(FPS_MIN, FPS_MAX) as u64);
    let mut index: u64 = 0;
    let mut written: u64 = 0;
    // When the pixels in `source` last came from somewhere. See the reseed
    // block below.
    let mut sourced_at = Instant::now();
    let mut source_lost: Option<String> = None;

    while control.is_running() {
        let deadline = started + period.saturating_mul(index as u32);

        // Sleep to the tick, taking every frame that arrives on the way. This is
        // the only reader of xcap's channel: falling behind here stalls the
        // duplication rather than queueing, which is the correct backpressure.
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() || !control.is_running() {
                break;
            }
            match rx.recv_timeout(remaining.min(TICK_SLICE)) {
                Ok(frame) => {
                    source = (frame.width, frame.height, frame.raw);
                    sourced_at = Instant::now();
                }
                Err(RecvTimeoutError::Timeout) => {}
                // The capture thread died. Keep encoding the last frame rather
                // than ending the recording — the user still gets the clip, and
                // the reseed below is what stops "the last frame" from being
                // the rest of the recording.
                Err(RecvTimeoutError::Disconnected) => {
                    let _ = control.sleep_until(deadline);
                    break;
                }
            }
        }
        if !control.is_running() {
            break;
        }

        // Duplication only produces a frame when something *changed*, so a long
        // gap is normally just a still desktop. It is also exactly what a dead
        // duplication looks like: xcap's recorder thread ends on any
        // `AcquireNextFrame` error that is not a timeout, and DXGI hands one out
        // for a UAC prompt, a resolution change, a session switch or a monitor
        // being unplugged — and its `SyncSender` lives on in the recorder we
        // keep, so the channel never even reports as disconnected.
        //
        // Rather than trying to tell those two apart, stop needing to: re-grab
        // the screen directly. On a still desktop that is one BitBlt every
        // RESEED_AFTER, at the one moment the encoder has nothing else to do,
        // and it changes nothing about the output. On a dead duplication it is
        // the only thing standing between the user and a recording that is a
        // frozen frame for its entire length.
        if sourced_at.elapsed() >= RESEED_AFTER {
            match monitor.capture_image() {
                Ok(img) => {
                    source = (img.width(), img.height(), img.into_raw());
                    sourced_at = Instant::now();
                }
                // Not a transient: the display itself is gone. End the recording
                // and keep what was encoded, rather than filling the file with a
                // still of a monitor that no longer exists until someone
                // notices.
                Err(e) => {
                    source_lost = Some(e.to_string());
                    break;
                }
            }
        }

        let mut frame = vec![0u8; frame_bytes];
        crop_into(
            &mut frame,
            width,
            height,
            &source.2,
            source.0,
            source.1,
            origin_x,
            origin_y,
        );

        // M2.10's rasteriser, per emitted frame rather than per captured one: the
        // cursor moves over a static desktop that produces no duplication frames
        // at all, so this is also the only thing that makes it move in the clip.
        if job.capture_cursor {
            if let Some(cursor) = crate::cursor::capture_cursor() {
                if let Some(mut img) = RgbaImage::from_raw(width, height, frame) {
                    crate::cursor::composite(&mut img, &cursor, job.rect.x, job.rect.y);
                    frame = img.into_raw();
                } else {
                    // `from_raw` only fails on a length mismatch, which cannot
                    // happen here — but it takes the buffer by value, so there
                    // has to be a way back.
                    frame = vec![0u8; frame_bytes];
                    crop_into(
                        &mut frame,
                        width,
                        height,
                        &source.2,
                        source.0,
                        source.1,
                        origin_x,
                        origin_y,
                    );
                }
            }
        }

        queue.push(frame);
        written += 1;
        index += 1;
        progress.frames.store(written, Ordering::Relaxed);
        progress
            .dropped
            .store(queue.dropped(), Ordering::Relaxed);

        // Re-base rather than sprint: if the machine stalled, catching up would
        // mean emitting a burst of identical frames as fast as the pipe takes
        // them, which is exactly the wrong thing to do to an encoder that is
        // already behind.
        let lag = Instant::now().saturating_duration_since(started + period.saturating_mul(index as u32));
        if lag > MAX_LAG {
            index = (Instant::now().saturating_duration_since(started).as_nanos()
                / period.as_nanos().max(1)) as u64;
        }
    }

    // ---- teardown (M4 §4) ------------------------------------------------
    //
    // Order matters and is the whole point. The capture comes down first and
    // unconditionally, so "stop" is true the instant it is asked for, whatever
    // ffmpeg is doing. Only then do we start negotiating with the child, and
    // every step of that negotiation has a deadline that ends in a kill.

    let cancelled = control.mode() == Mode::Cancelling;
    progress.set_phase(if cancelled {
        PHASE_FINISHING
    } else {
        PHASE_ENCODING
    });

    // The recorder and its channel stay in the map for the next recording; the
    // guard is released when this function returns, which is also what keeps two
    // recordings off the same duplication.
    let _ = recorder.stop();

    // Unless the display it duplicates is gone, in which case keeping it would
    // hand the *next* recording the same dead handle — and, because that handle
    // fails silently, another whole clip of one frozen frame. Its thread has
    // already ended (that is what killed the duplication in the first place), so
    // dropping it leaks nothing.
    if source_lost.is_some() {
        recorders.remove(&job.monitor_id);
    }

    if cancelled {
        queue.abandon();
    } else {
        queue.close();
    }

    // A cancel is the only thing that gets a zero: nothing is being kept, so
    // there is nothing left to spend time on. Everything else — including a user
    // who has asked twice — still gets long enough for ffmpeg to close the file
    // it has been writing, because a killed MP4 has no `moov` atom and will not
    // open in anything.
    let writer_deadline = if cancelled {
        Duration::ZERO
    } else if control.escalated() {
        HURRIED_WRITER_DEADLINE
    } else {
        WRITER_DEADLINE
    };
    let writer_stuck = match writer_result.recv_timeout(writer_deadline) {
        Ok(None) => false,
        Ok(Some(e)) => {
            eprintln!("santi.sharex: writing frames to ffmpeg failed: {e}");
            false
        }
        Err(_) => true,
    };

    // A stuck writer means ffmpeg is not reading stdin, so it will never see the
    // EOF it is waiting for and waiting out the full deadline buys nothing —
    // but the short one is still not zero, for the same reason as above.
    let child_deadline = if cancelled {
        Duration::ZERO
    } else if control.escalated() || writer_stuck {
        HURRIED_CHILD_DEADLINE
    } else {
        CHILD_DEADLINE
    };
    let killed = wait_or_kill(&mut child, child_deadline);

    // A cancel that landed while ffmpeg was still finalising is still a cancel.
    // The frames are gone either way; what is left to honour is the promise that
    // nothing was written, and the only way to keep it is to check again here
    // rather than only at the top of the teardown.
    let cancelled = cancelled || control.mode() == Mode::Cancelling;

    if killed && !cancelled {
        eprintln!(
            "santi.sharex: ffmpeg did not finish within {}s and was stopped; the recording may be truncated",
            CHILD_DEADLINE.as_secs()
        );
    }

    let dropped = queue.dropped();
    let mut outcome = Outcome {
        frames: written,
        dropped,
        duration_ms: ((written as f64 / job.fps.max(1) as f64) * 1000.0).round() as u32,
        width,
        height,
        truncated: killed || writer_stuck,
        cancelled,
        source_lost: source_lost.clone(),
    };

    if cancelled {
        let _ = std::fs::remove_file(&encode_target);
        if encode_target != job.output {
            let _ = std::fs::remove_file(&job.output);
        }
        progress.set_phase(PHASE_DONE);
        return Ok(outcome);
    }

    if written == 0 {
        let _ = std::fs::remove_file(&encode_target);
        if let Some(why) = source_lost {
            return Err(format!(
                "the display being recorded went away before anything was captured ({why})"
            ));
        }
        let tail = stderr.tail();
        return Err(if tail.is_empty() {
            "nothing was captured — the recording produced no frames".to_string()
        } else {
            format!("nothing was captured — the recording produced no frames ({tail})")
        });
    }

    if !encode_target.exists() {
        let tail = stderr.tail();
        return Err(format!(
            "ffmpeg produced no file{}",
            if tail.is_empty() {
                String::new()
            } else {
                format!(": {tail}")
            }
        ));
    }

    if job.format == Format::Gif {
        let target_width = plan_gif_width(width, job.gif_max_width);
        let palette = job.work_dir.join("palette.png");

        // The two passes are the one stretch of a recording that happens after
        // the user has already stopped it and can still take minutes, so they
        // are the one stretch that has to stay interruptible.
        let abandon = || control.abandoning();
        let passes = (|| {
            run_pass(
                &job.ffmpeg,
                &palettegen_args(&encode_target, target_width, &palette),
                PASS_DEADLINE,
                "GIF palette",
                &abandon,
            )?;
            run_pass(
                &job.ffmpeg,
                &paletteuse_args(&encode_target, &palette, target_width, &job.output),
                PASS_DEADLINE,
                "GIF encode",
                &abandon,
            )
        })();

        let _ = std::fs::remove_file(&palette);
        let _ = std::fs::remove_file(&encode_target);
        if passes.is_err() {
            // A half-written GIF is not a shorter recording, it is a broken
            // file — and one that would otherwise sit in `save_dir` under a
            // perfectly ordinary-looking name.
            let _ = std::fs::remove_file(&job.output);

            // A cancel is what stopped the pass. That is not a failed encode,
            // it is the user's own decision arriving a moment later than the
            // teardown check above — so report it as the cancel it is rather
            // than as an error about something they asked for.
            if control.mode() == Mode::Cancelling {
                outcome.cancelled = true;
                progress.set_phase(PHASE_DONE);
                return Ok(outcome);
            }
        }
        passes?;

        if let Some(w) = target_width {
            // The height ffmpeg picked is not knowable without asking, and the
            // record wants real numbers — the thumbnail read below is what
            // corrects both.
            outcome.height = ((height as u64 * w as u64) / width.max(1) as u64) as u32;
            outcome.width = w;
        }
    }

    progress.set_phase(PHASE_DONE);
    Ok(outcome)
}

fn monitor_by_id(id: u32) -> Result<Monitor, String> {
    let monitors = Monitor::all().map_err(|e| format!("enumerating monitors failed: {e}"))?;
    monitors
        .into_iter()
        .find(|m| m.id().map(|got| got == id).unwrap_or(false))
        .ok_or_else(|| format!("no monitor with id {id}"))
}

// ---------------------------------------------------------------------------
// the thumbnail frame
// ---------------------------------------------------------------------------

/// A frame from the middle of a finished clip (M4 §5).
///
/// PNG on stdout rather than raw pixels, because the GIF path may have scaled
/// the output and a raw stream would have to be told a size we would then have
/// to guess.
pub fn extract_frame(ffmpeg: &Ffmpeg, file: &Path, at_ms: u32) -> Result<RgbaImage, String> {
    let seconds = at_ms as f64 / 1000.0;
    let args = vec![
        "-loglevel".to_string(),
        "error".into(),
        "-nostdin".into(),
        "-ss".into(),
        format!("{seconds:.3}"),
        "-i".into(),
        file.to_string_lossy().into_owned(),
        "-frames:v".into(),
        "1".into(),
        "-f".into(),
        "image2".into(),
        "-c:v".into(),
        "png".into(),
        "-".into(),
    ];

    let mut cmd = ffmpeg.command();
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start ffmpeg: {e}"))?;

    let mut stdout = child.stdout.take();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = stdout.as_mut() {
            let _ = pipe.read_to_end(&mut buf);
        }
        let _ = tx.send(buf);
    });

    let bytes = match rx.recv_timeout(EXTRACT_DEADLINE) {
        Ok(bytes) => {
            let _ = child.wait();
            bytes
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err("reading a frame back out of the recording timed out".into());
        }
    };

    if bytes.is_empty() {
        return Err("ffmpeg returned no frame".into());
    }
    Ok(image::load_from_memory(&bytes)
        .map_err(|e| format!("could not decode the extracted frame: {e}"))?
        .to_rgba8())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    // -- the drop policy ---------------------------------------------------

    /// The rule M4 §3 is emphatic about: bounded, dropping, counting — never
    /// growing. This is the pure-logic half.
    #[test]
    fn a_full_queue_drops_the_oldest_and_counts_it() {
        let queue = FrameQueue::new(3);
        for n in 0..10u8 {
            queue.push(vec![n]);
        }

        assert_eq!(queue.len(), 3, "the queue must never grow past capacity");
        assert_eq!(queue.dropped(), 7, "every dropped frame must be counted");
        // The newest three survived, in order: stale frames are the ones worth
        // losing in a live recording.
        assert_eq!(queue.pop(), Some(vec![7]));
        assert_eq!(queue.pop(), Some(vec![8]));
        assert_eq!(queue.pop(), Some(vec![9]));
    }

    /// And the threaded half: a genuinely slow consumer must not be able to make
    /// the producer allocate without bound.
    #[test]
    fn a_slow_consumer_makes_the_producer_drop_rather_than_grow() {
        let queue = Arc::new(FrameQueue::new(4));
        let seen = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));

        let consumer = {
            let queue = queue.clone();
            let seen = seen.clone();
            std::thread::spawn(move || {
                while queue.pop().is_some() {
                    seen.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(2));
                }
            })
        };

        for n in 0..400u32 {
            queue.push(n.to_le_bytes().to_vec());
            peak.fetch_max(queue.len(), Ordering::SeqCst);
        }
        queue.close();
        consumer.join().expect("consumer");

        assert!(
            peak.load(Ordering::SeqCst) <= 4,
            "the queue grew to {} with a capacity of 4",
            peak.load(Ordering::SeqCst)
        );
        assert!(
            queue.dropped() > 0,
            "a consumer this slow must have caused drops"
        );
        assert_eq!(
            seen.load(Ordering::SeqCst) as u64 + queue.dropped(),
            400,
            "every frame is either encoded or counted as dropped — none may vanish"
        );
    }

    /// Closing must not throw away what is already queued: a stop keeps the last
    /// frames the user watched being recorded. Cancelling is the one that
    /// discards.
    #[test]
    fn close_drains_and_cancel_discards() {
        let queue = FrameQueue::new(4);
        queue.push(vec![1]);
        queue.push(vec![2]);
        queue.close();
        assert_eq!(queue.pop(), Some(vec![1]));
        assert_eq!(queue.pop(), Some(vec![2]));
        assert_eq!(queue.pop(), None, "a drained closed queue ends the writer");

        let queue = FrameQueue::new(4);
        queue.push(vec![1]);
        queue.abandon();
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn the_queue_ceiling_is_bytes_not_frames() {
        // A 4K frame is 33 MB, so only a couple fit in the budget.
        let uhd = queue_capacity(3840 * 2160 * 4);
        assert!(uhd >= QUEUE_MIN_FRAMES && uhd <= 3, "got {uhd}");
        // A small region gets a deeper queue, but still a bounded one.
        assert_eq!(queue_capacity(320 * 180 * 4), QUEUE_MAX_FRAMES);
        assert_eq!(queue_capacity(0), QUEUE_MIN_FRAMES);
    }

    // -- cropping ----------------------------------------------------------

    fn pattern(w: u32, h: u32) -> Vec<u8> {
        let mut out = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                out[i] = x as u8;
                out[i + 1] = y as u8;
                out[i + 3] = 255;
            }
        }
        out
    }

    #[test]
    fn crop_lifts_the_right_window() {
        let src = pattern(8, 8);
        let mut dst = vec![0u8; 2 * 2 * 4];
        crop_into(&mut dst, 2, 2, &src, 8, 8, 3, 5);

        assert_eq!(&dst[0..4], &[3, 5, 0, 255]);
        assert_eq!(&dst[4..8], &[4, 5, 0, 255]);
        assert_eq!(&dst[8..12], &[3, 6, 0, 255]);
        assert_eq!(&dst[12..16], &[4, 6, 0, 255]);
    }

    /// A rect that hangs off the monitor must still produce a full-size frame —
    /// rawvideo has no way to say a frame changed shape mid-stream.
    #[test]
    fn a_rect_off_the_edge_still_fills_the_whole_frame() {
        let src = pattern(4, 4);
        let mut dst = vec![9u8; 4 * 4 * 4];
        crop_into(&mut dst, 4, 4, &src, 4, 4, -2, -2);

        assert_eq!(dst.len(), 4 * 4 * 4, "the frame size is fixed");
        // Bottom-right quadrant carries the source's top-left.
        let at = |x: usize, y: usize| &dst[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
        assert_eq!(at(2, 2), &[0, 0, 0, 255]);
        assert_eq!(at(3, 3), &[1, 1, 0, 255]);
        // The rest is untouched, i.e. whatever the caller allocated.
        assert_eq!(at(0, 0), &[9, 9, 9, 9]);
    }

    // -- encoder arguments -------------------------------------------------

    #[test]
    fn mp4_carries_the_flags_that_make_it_play_anywhere() {
        let args = mp4_args(1280, 720, 30, false, Path::new("out.mp4"));
        let joined = args.join(" ");

        assert!(joined.contains("-f rawvideo"));
        assert!(joined.contains("-pix_fmt rgba -s 1280x720 -r 30 -i -"));
        assert!(joined.contains("-c:v libx264"));
        assert!(joined.contains("-preset veryfast"));
        assert!(joined.contains("-crf 23"));
        // The one flag whose absence makes the file unplayable in half the
        // world's players.
        assert!(joined.contains("-pix_fmt yuv420p"));
        assert!(joined.contains("-movflags +faststart"));
    }

    #[test]
    fn hardware_encoding_is_opt_in_and_libx264_is_the_default() {
        let software = mp4_args(640, 480, 30, false, Path::new("a.mp4")).join(" ");
        assert!(software.contains("libx264"));
        assert!(!software.contains("nvenc"));

        let hardware = mp4_args(640, 480, 30, true, Path::new("a.mp4")).join(" ");
        assert!(hardware.contains("h264_nvenc"));
        assert!(!hardware.contains("libx264"));
        // Still yuv420p, whichever encoder ran.
        assert!(hardware.contains("-pix_fmt yuv420p"));
    }

    /// The whole difference between a usable recording and something
    /// unshareable: palettegen then paletteuse, never one pass.
    #[test]
    fn gif_is_two_passes_over_one_intermediate() {
        let source = Path::new("clip.mp4");
        let palette = Path::new("palette.png");
        let out = Path::new("clip.gif");

        let one = palettegen_args(source, Some(800), palette).join(" ");
        assert!(one.contains("palettegen=stats_mode=diff"));
        assert!(one.contains("scale=800:-1:flags=lanczos"));
        assert!(!one.contains("paletteuse"));

        let two = paletteuse_args(source, palette, Some(800), out).join(" ");
        assert!(two.contains("paletteuse"));
        assert!(two.contains("[x][1:v]"), "the palette must be the second input");
        assert!(two.contains("scale=800:-1:flags=lanczos"));
        assert!(two.contains("-loop 0"));
    }

    #[test]
    fn a_gif_narrower_than_the_ceiling_is_not_scaled() {
        assert_eq!(plan_gif_width(640, 800), None);
        assert_eq!(plan_gif_width(800, 800), None);
        assert_eq!(plan_gif_width(3840, 800), Some(800));
        // A hand-edited 0 must not become "scale to nothing".
        assert_eq!(plan_gif_width(3840, 0), Some(GIF_WIDTH_MIN));

        // And the filter graph stays valid when there is nothing to scale.
        let unscaled = paletteuse_args(
            Path::new("a.mp4"),
            Path::new("p.png"),
            None,
            Path::new("a.gif"),
        )
        .join(" ");
        assert!(unscaled.contains("[0:v]null[x]"));
    }

    // -- odds and ends -----------------------------------------------------

    /// libx264 with `yuv420p` refuses an odd dimension, and the flag is not
    /// negotiable, so the rect is.
    #[test]
    fn a_rect_is_rounded_to_even_dimensions() {
        let r = Rect {
            x: 11,
            y: 13,
            width: 321,
            height: 199,
        }
        .to_even();
        assert_eq!((r.width, r.height), (320, 198));
        // The origin is untouched: moving it would shift what was selected.
        assert_eq!((r.x, r.y), (11, 13));
    }

    #[test]
    fn overlap_is_what_the_hud_uses_to_stay_out_of_the_shot() {
        let capture = Rect { x: 0, y: 0, width: 100, height: 100 };
        assert!(capture.intersects(&Rect { x: 90, y: 90, width: 40, height: 40 }));
        assert!(!capture.intersects(&Rect { x: 100, y: 0, width: 40, height: 40 }));
        assert!(!capture.intersects(&Rect { x: 0, y: 100, width: 40, height: 40 }));
    }

    /// A cancel outranks a stop, and neither can be undone by a later request.
    #[test]
    fn cancel_wins_and_a_second_request_escalates() {
        let control = Control::default();
        let t0 = Instant::now();
        assert_eq!(control.mode(), Mode::Running);

        control.request_at(Mode::Stopping, t0);
        assert_eq!(control.mode(), Mode::Stopping);
        assert!(!control.escalated());

        // Long enough after the first that the user is plainly insisting.
        control.request_at(Mode::Stopping, t0 + ESCALATE_GRACE);
        assert_eq!(control.mode(), Mode::Stopping);
        assert!(control.escalated(), "asking twice, later, means 'now'");

        control.request_at(Mode::Cancelling, t0 + ESCALATE_GRACE);
        assert_eq!(control.mode(), Mode::Cancelling);

        // And a stop after a cancel does not resurrect the file.
        control.request_at(Mode::Stopping, t0 + ESCALATE_GRACE);
        assert_eq!(control.mode(), Mode::Cancelling);
    }

    /// The failure this grace window exists for: a user who taps the stop hotkey
    /// twice because they are not sure the first press landed. Escalation
    /// shortens ffmpeg's deadline, and an MP4 killed before it writes its `moov`
    /// atom does not open in anything — so a reflex double-tap must be *only*
    /// idempotent, never a kill order.
    #[test]
    fn a_reflex_double_tap_is_not_an_escalation() {
        let control = Control::default();
        let t0 = Instant::now();

        control.request_at(Mode::Stopping, t0);
        // The tray and the hotkey both firing, or a finger bouncing.
        control.request_at(Mode::Stopping, t0 + Duration::from_millis(40));
        control.request_at(Mode::Stopping, t0 + Duration::from_millis(300));

        assert_eq!(control.mode(), Mode::Stopping);
        assert!(
            !control.escalated(),
            "a second press inside the grace window must not shorten ffmpeg's deadline"
        );

        // And the grace window is a delay, not an exemption.
        control.request_at(Mode::Stopping, t0 + ESCALATE_GRACE + Duration::from_millis(1));
        assert!(control.escalated());
    }

    /// Cancelling never waits: there is no file to protect, so the grace window
    /// does not apply to it.
    #[test]
    fn a_cancel_escalates_immediately() {
        let control = Control::default();
        let t0 = Instant::now();

        control.request_at(Mode::Stopping, t0);
        assert!(!control.escalated());
        control.request_at(Mode::Cancelling, t0 + Duration::from_millis(5));
        assert!(control.escalated(), "a cancel is never a double-tap");
        assert!(control.abandoning());
    }

    /// The deadlines an escalation produces. Short, so a wedged encoder cannot
    /// hold the app — but never zero, which would kill ffmpeg mid-trailer.
    #[test]
    fn escalated_deadlines_are_short_but_never_zero() {
        assert!(HURRIED_WRITER_DEADLINE > Duration::ZERO);
        assert!(HURRIED_CHILD_DEADLINE > Duration::ZERO);
        assert!(HURRIED_WRITER_DEADLINE < WRITER_DEADLINE);
        assert!(HURRIED_CHILD_DEADLINE < CHILD_DEADLINE);
    }

    /// The GIF passes are the only part of a recording that happens *after* the
    /// stop and can still take minutes, so they are the only part that has to
    /// stay interruptible. `abandoning` is what they ask.
    #[test]
    fn a_stopping_run_is_not_abandoning_until_it_is_pressed_again() {
        let control = Control::default();
        let t0 = Instant::now();

        assert!(!control.abandoning());
        control.request_at(Mode::Stopping, t0);
        assert!(
            !control.abandoning(),
            "an ordinary stop must let the encode finish"
        );
        control.request_at(Mode::Stopping, t0 + ESCALATE_GRACE);
        assert!(control.abandoning());
    }

    #[test]
    fn format_parsing_falls_back_to_mp4() {
        assert_eq!(Format::parse("gif"), Format::Gif);
        assert_eq!(Format::parse("GIF"), Format::Gif);
        assert_eq!(Format::parse("mp4"), Format::Mp4);
        assert_eq!(Format::parse("webm"), Format::Mp4);
        assert_eq!(Format::parse(""), Format::Mp4);
        assert_eq!(Format::Gif.ext(), "gif");
    }
}
