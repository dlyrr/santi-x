# santi.sharex — M4: screen recording

Extends everything through M3. Record a region, window or monitor to MP4 or
GIF, with a HUD you can actually stop.

---

## 1. ffmpeg is found, never downloaded

ShareX downloads an ffmpeg build on first use. santi.sharex does not: fetching
and executing an ~80 MB binary sits badly beside M3 §1's consent rules, and it
is not needed here — ffmpeg 8.1.1 is already on this machine via scoop, with
`libx264`, `gif`, `libvpx-vp9` and NVENC/AMF/QSV.

Resolution order, first hit wins:

1. `Settings.ffmpeg_path`, when the user set one explicitly
2. `ffmpeg` on `PATH`
3. `%USERPROFILE%\scoop\shims\ffmpeg.exe`
4. `%LOCALAPPDATA%\Microsoft\WinGet\Links\ffmpeg.exe`, `%ProgramFiles%\ffmpeg\bin\ffmpeg.exe`

If none resolves, recording is **disabled with an explanation** — what is
missing, that `scoop install ffmpeg` or `winget install ffmpeg` fixes it, and a
Browse control to point at a binary directly. Never a silent failure, and never
an offer to download one.

Validate the binary once at resolution time (`ffmpeg -version`), cache the
result for the session, and check that the encoder a job needs is actually
present rather than discovering it in a failed run.

---

## 2. What gets recorded

Three sources, reusing the existing pickers:

- **Region** — the M2.5 overlay, in a mode where the selection sets the capture
  rect instead of committing an image. Annotation tools are not offered here;
  the overlay opens on Region and the toolbar is hidden.
- **Window** — the M2.5 window list.
- **Monitor** — the M1 monitor list.

The rect is fixed at start. A window that moves or resizes mid-recording keeps
its original rect; say so in the README rather than pretending to track it.

## 3. Capture and encode

Frames come from `xcap`'s `VideoRecorder` (`Monitor::video_recorder()`), which
exists in the version already vendored. Feed them to ffmpeg over **stdin as raw
`rgba` frames** (`-f rawvideo -pix_fmt rgba -s WxH -r FPS -i -`), rather than
writing thousands of PNGs to disk and re-reading them.

```rust
pub struct RecordSpec {
    pub source: RecordSource,   // Region { rect } | Window { id } | Monitor { id }
    pub format: RecordFormat,   // Mp4 | Gif
    pub fps: u32,               // default 30, 5..=60
    pub capture_cursor: bool,   // reuses Settings.capture_cursor
}
```

**MP4:** `libx264`, `-preset veryfast`, `-crf 23` by default, `-pix_fmt yuv420p`
(without it the file will not play in half the world's players), and
`+faststart`. Offer hardware encoding as an explicit option — `h264_nvenc` when
the probe found it — but keep libx264 the default, because NVENC quality at low
bitrates is worse and the failure mode is confusing.

**GIF:** two passes, `palettegen` then `paletteuse`. A single-pass GIF is a
dithered mess and this is the whole difference between a usable recording and
something unshareable. Default to 15 fps and a max width of 800px for GIF, both
configurable — a 4K 30fps GIF is a several-hundred-megabyte file nobody wants.

Cursor drawing reuses `cursor.rs` from M2.10, composited per frame when
`capture_cursor` is on.

**Dropped frames are expected.** If the encoder cannot keep up, drop frames
rather than growing an unbounded queue, count them, and report the count when
the recording ends. An app that silently eats RAM until it dies is worse than
one that admits it dropped 4% of frames.

---

## 4. The HUD

A small always-on-top, **unfocusable** window (`?w=recorder`), same discipline as
the capture preview in M2.9 §3 — `focused(false)`, never `set_focus`, built
hidden and reused, and every exit path hides it.

- Shows elapsed time, resolution, fps, format, and a live dropped-frame count if
  any.
- **Stop** and **Cancel** buttons: Stop finalizes, Cancel discards.
- Positioned out of the recorded rect where possible — bottom-right of the
  *monitor*, nudged clear if it would overlap the capture region. A HUD recorded
  into the video is a bug.
- A **global stop hotkey** that works while the HUD is not focused, since it
  cannot take focus. Default `Ctrl+Shift+4`, registered through the M2.6 hotkey
  path (plugin first, hook fallback) and shown in the HUD.

**A recording you cannot stop is the worst outcome in this milestone.** Stop must
be reachable from: the HUD button, the global hotkey, and the tray menu, and each
must work when the other two do not. If ffmpeg wedges, stopping must still tear
down the capture loop and keep whatever was encoded.

---

## 5. Output

The finished file goes through the existing pipeline as far as it applies:
named by the filename pattern, written to `save_dir`, added to history as
`kind: "recording"`, and emitted on `capture://new`.

`CaptureRecord` gains `duration_ms: Option<u32>` (`#[serde(default)]` — the user
has 82 real records that must keep loading). A recording's thumbnail is a frame
grabbed from the middle of the clip.

History must handle a video record honestly: the Lightbox plays it in a
`<video>` element rather than trying to render it as an image, the editor refuses
it with a clear message rather than opening on a broken canvas, and OCR is not
offered for it.

Uploading a recording is allowed where the destination accepts it — Imgur takes
MP4 and GIF, a custom uploader depends on its config, FTP takes anything. Do not
offer upload for a format the active destination will reject.

---

## 6. Settings

```rust
pub ffmpeg_path: String,          // "" = auto-resolve
pub record_fps: u32,              // default 30
pub record_format: String,        // "mp4" | "gif"
pub record_hw_encode: bool,       // default false
pub record_gif_fps: u32,          // default 15
pub record_gif_max_width: u32,    // default 800
pub record_stop_hotkey: String,   // default "CmdOrCtrl+Shift+4"
```

All with named serde defaults — an existing `settings.json` must not acquire
`0 fps`.

## 7. What must not regress

Everything through M3: the overlay Escape ladder, arm/ready handshake,
commit-on-release, the native-crop fast path, cursor capture, the preview
hiding before a grab, OCR, scrolling capture, the keyboard hook's scope, the
upload consent and credential rules, all four themes, and the ShareX shell's
no-faked-features rule — the **Screen recorder** row in Tools becomes genuinely
live.

`cargo check`, `cargo test`, `pnpm check`, `pnpm check:tokens`, `pnpm build`.
