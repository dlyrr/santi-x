/**
 * TypeScript mirrors of every Rust type that crosses the Tauri bridge.
 * Rust serialises with `#[serde(rename_all = "camelCase")]`, so field names
 * here are the camelCase form of the Rust struct fields.
 */

/**
 * Every selectable theme, in the order `app.css` defines them and the settings
 * picker shows them. Kept as a value, not just a type, so the picker and the
 * runtime guard below cannot drift from the union.
 */
export const THEMES = ['dark', 'claude', 'claude-dark', 'sharex'] as const;

/** Value of `<html data-theme>` and of `Settings.theme`. */
export type Theme = (typeof THEMES)[number];

/** The theme any unknown value falls back to — an untokenised page otherwise. */
export const DEFAULT_THEME: Theme = 'dark';

/**
 * Rust stores `theme` as a plain `String`, so a settings file hand-edited or
 * written by a future version can carry a name this build has no block for.
 * Stamping that on `<html>` renders every token unresolved.
 */
export function isTheme(value: unknown): value is Theme {
  return typeof value === 'string' && (THEMES as readonly string[]).includes(value);
}

/** localStorage key holding the pre-paint theme cache. See ARCHITECTURE §6. */
export const THEME_STORAGE_KEY = 'santi-sharex.theme';

/**
 * The pre-rename key, from when this app was called Nimbus. Read-only, and only
 * when `THEME_STORAGE_KEY` is absent: without it the first launch after the
 * rename has no cache to read and paints the default palette for a frame. The
 * inline script in `app.html` migrates the value forward, so this is a fallback
 * rather than a second source of truth — nothing ever writes it.
 *
 * It will often find nothing, and that is not a bug in the fallback. Tauri
 * forces the WebView2 user-data directory to `<LocalData>/<identifier>`, so the
 * same rename that moved the app data also handed the webview a fresh profile
 * with empty storage — there is no supported way to read the old profile's
 * localStorage from the new one. The cost when it misses is bounded to one
 * frame of `dark` before the settings round trip lands, which is why this is a
 * cheap read rather than something more elaborate.
 */
export const LEGACY_THEME_STORAGE_KEY = 'nimbus.theme';

/** Accelerator strings in Tauri's `CmdOrCtrl+Shift+X` syntax. */
export interface Hotkeys {
  region: string;
  fullscreen: string;
  activeWindow: string;
}

/**
 * Everything a hotkey status row can describe that santi.sharex itself owns.
 *
 * The three capture bindings live inside `Settings.hotkeys`; `recordStop` does
 * not — M4 §6 puts it at the top level as `Settings.recordStopHotkey`, because
 * it stops a recording rather than starting a capture and nothing iterating
 * `Hotkeys` should find it. It is still registered through the same M2.6 path,
 * so it can still be reported here.
 *
 * Consumers that only know about the capture three keep working: they look their
 * own key up by name, and a row they have no opinion about is simply not found.
 */
export type BuiltinHotkeyAction = keyof Hotkeys | 'recordStop';

/**
 * The built-ins in the order every hotkey table lists them, and what each one
 * is called when something else tries to claim its combination (M6 §3). A
 * conflict has to name what already owns the combo, and "region" is not a name
 * a user recognises from the outside.
 */
export const BUILTIN_HOTKEY_ACTIONS = [
  'region',
  'fullscreen',
  'activeWindow',
  'recordStop'
] as const satisfies readonly BuiltinHotkeyAction[];

export const BUILTIN_HOTKEY_OWNER: Record<BuiltinHotkeyAction, string> = {
  region: 'Capture region',
  fullscreen: 'Capture fullscreen',
  activeWindow: 'Capture active window',
  recordStop: 'Stop recording'
};

/**
 * Which binding a `HotkeyStatus` row describes.
 *
 * M6 §3 puts workflow hotkeys through the same M2.6 registry as the built-ins,
 * so the table stops being three fixed rows: a workflow's binding is reported
 * as `workflow:<workflow id>`. Nothing may hard-code that prefix —
 * `workflowHotkeyAction()` and `workflowIdOfHotkeyAction()` below are the only
 * two places it is spelled out.
 */
export type HotkeyAction = BuiltinHotkeyAction | `workflow:${string}`;

/** The shipped stop-recording accelerator (M4 §4). */
export const RECORD_STOP_HOTKEY_DEFAULT = 'CmdOrCtrl+Shift+4';

/**
 * Which mechanism ended up owning a hotkey (M2.6 §1).
 *
 * - `plugin` — `RegisterHotKey`, the ordinary path.
 * - `hook` — the `WH_KEYBOARD_LL` fallback, the only way to claim a combo
 *   another process already registered.
 * - `none` — nothing claimed it. The hotkey does nothing when pressed.
 */
export type HotkeyMechanism = 'plugin' | 'hook' | 'none';

/**
 * One row of `get_hotkey_status` / the `hotkeys://status` event. Rust rebuilds
 * the whole list on every rebind, so it is always every action it registered.
 */
export interface HotkeyStatus {
  /** Which binding this describes. */
  action: HotkeyAction;
  /** The accelerator as it was registered — compare against `Settings.hotkeys`
   *  to tell a live report from one that predates an unsaved rebind. */
  accelerator: string;
  mechanism: HotkeyMechanism;
  bound: boolean;
  /** Why it is unbound; `null` whenever it is bound. */
  error: string | null;
}

export interface Settings {
  saveDir: string;
  filenamePattern: string;
  saveToDisk: boolean;
  copyToClipboard: boolean;
  openFolderAfter: boolean;
  hideWindowOnCapture: boolean;
  openEditorAfter: boolean;
  /**
   * Show the small bottom-right preview card after each capture (M2.9 §3).
   * Off, nothing appears and the preview window is never shown.
   */
  showCapturePreview: boolean;
  /**
   * Draw on a region selection inside the overlay before it is committed.
   * When false the overlay commits on pointerup, the M1 fast path.
   */
  annotateInOverlay: boolean;
  /**
   * The region overlay's magnifier factor, 2–20 (M2.9 §1). The wheel writes it
   * through the ordinary settings path, so a user who zooms to 12× is still at
   * 12× on the next capture and after a restart.
   */
  loupeZoom: number;
  /**
   * Draw the mouse cursor into the capture. The framebuffer never contains it —
   * Windows composites the cursor separately — so santi.sharex rasterises the cursor
   * icon and blends it in explicitly.
   */
  captureCursor: boolean;
  /** Hex colour the editor pre-selects for new shapes, e.g. `#f2555a`. */
  editorDefaultColor: string;
  /** Stroke width in image pixels, 1–24. */
  editorDefaultStroke: number;
  theme: Theme;
  /**
   * Fall back to the low-level keyboard hook for combos `RegisterHotKey`
   * refuses (M2.6 §1). Off means plugin-only: those combos stay unbound.
   */
  useLowLevelHotkeys: boolean;
  /** Launch with only the tray icon, ShareX-style (M2.6 §2). */
  startHidden: boolean;
  /** Register santi.sharex to start with Windows. Kept in sync with the real
   *  registration on every load and save. */
  launchAtLogin: boolean;
  /**
   * Settle time in milliseconds between one wheel step and the screenshot that
   * follows it (M5 §4). Too short and the frame catches a smooth-scroll
   * mid-flight, which the overlap search then reads as a bad match.
   */
  scrollDelayMs: number;
  /** Wheel notches sent per step. */
  scrollStep: number;
  /** Hard stop on the number of frames one run may capture. */
  scrollMaxFrames: number;
  /**
   * Where to find ffmpeg, or `''` to resolve it (M4 §1). santi.sharex **finds**
   * ffmpeg and never downloads one, so this is the escape hatch for a machine
   * where none of the four resolution steps hits: the user points at a binary
   * and recording works.
   */
  ffmpegPath: string;
  /** Capture rate, 5–60. See `RECORD_FPS`. */
  recordFps: number;
  /**
   * `'mp4'` or `'gif'`, and a plain `string` for the same reason `theme` is —
   * a hand-edited or future `settings.json` can name something else, and one
   * unknown value must not take every other setting down with it. Read it
   * through `recordFormatOf()`, never by comparing to a literal.
   */
  recordFormat: string;
  /**
   * Encode MP4 with the GPU (`h264_nvenc` and friends) instead of `libx264`.
   * Default **false**: NVENC quality at low bitrates is worse and its failure
   * mode is confusing (M4 §3), so this is opt-in and only offered when the
   * ffmpeg probe actually found a hardware encoder.
   */
  recordHwEncode: boolean;
  /** GIF capture rate, kept apart from `recordFps`: 30fps GIF is unshareable. */
  recordGifFps: number;
  /** GIFs are downscaled to at most this many pixels wide. */
  recordGifMaxWidth: number;
  /**
   * The global stop-recording accelerator (M4 §4). Not inside `hotkeys` — see
   * `HotkeyAction`. It is what makes a recording stoppable while the HUD, which
   * cannot take focus, is not reachable.
   */
  recordStopHotkey: string;
  /**
   * Where the **Upload** action sends a capture (M3 §1). `'none'` is the
   * shipped default and the out-of-box state: nothing can leave the machine
   * until the user picks a destination and configures it.
   *
   * A plain `String` on the Rust side, for the same reason `theme` is — but
   * `normalize_destination` runs on the way in *and* on the way out, and an id
   * this build does not know becomes `'none'` rather than some destination. So
   * by the time one arrives here it is one of these four.
   */
  destination: DestinationChoice;
  /**
   * Upload **every** capture the moment it is taken, including hotkey captures
   * taken while the user is doing something else (M3 §1).
   *
   * Defaults to **false**, with a named serde default on the Rust side so an
   * existing `settings.json` cannot silently acquire it as `true`. The UI must
   * not flip it on a bare toggle — see `SettingsView`'s confirmation.
   */
  autoUpload: boolean;
  /** Put the returned link on the clipboard when an upload succeeds. Default true. */
  copyUrlAfterUpload: boolean;
  /** Non-secret FTP configuration; the password lives in Credential Manager. */
  ftp: FtpSettings;
  /**
   * The imported `.sxcu`. Always present rather than nullable — an empty `name`
   * and `requestUrl` is the "nothing imported" state, which is what Rust's
   * `Default` writes. Any header that looked like a credential was moved to
   * Credential Manager at import time and is only *named* here (M3 §4).
   */
  customUploader: CustomUploaderSettings;
  hotkeys: Hotkeys;
}

/**
 * Bounds and defaults for the three scrolling-capture settings, kept beside the
 * fields they describe so the sliders and Rust's clamps cannot drift apart.
 * `def` is what a fresh install writes (M5 §4).
 */
export const SCROLL_DELAY_MS = { min: 50, max: 2000, step: 25, def: 250 } as const;
export const SCROLL_STEP = { min: 1, max: 10, step: 1, def: 3 } as const;
export const SCROLL_MAX_FRAMES = { min: 5, max: 200, step: 5, def: 60 } as const;

/**
 * Which capture path produced a record. Mirrors `CaptureRecord.kind`.
 * `edit` comes from `saveEdit(..., replace: false)`, not from a capture;
 * `scroll` is a stitched scrolling capture (M5 §4), which goes through the same
 * `finalize()` as everything else and so is an ordinary record in every other
 * respect.
 *
 * `recording` is the one kind that is **not** a still image (M4 §5). Every place
 * that renders, edits, copies, reads text out of or uploads a record has to ask
 * before assuming a PNG is behind it — `captureIsRecording` and
 * `capturePlaysAsVideo` below are how, so no view matches the string itself.
 */
export type CaptureKind =
  | 'region'
  | 'fullscreen'
  | 'window'
  | 'monitor'
  | 'edit'
  | 'scroll'
  | 'recording';

export interface CaptureRecord {
  id: string;
  name: string;
  /** Absolute path on disk; empty string when `saveToDisk` was false. */
  path: string;
  /** Absolute path to the thumbnail PNG; feed through `assetUrl()` to render. */
  thumb: string;
  width: number;
  height: number;
  kind: CaptureKind;
  /** Unix epoch milliseconds, UTC. */
  createdAt: number;
  sizeBytes: number;
  saved: boolean;
  copied: boolean;
  /**
   * The public URL this capture was uploaded to (M3 §6), or `null` when it
   * never was — which is every record until the user asks for one.
   *
   * `#[serde(default)]` on the Rust side, so the 82 records in an existing
   * `history.json` still load; a record written before M3 arrives here with the
   * field absent rather than null, so test it for truthiness, never for `null`.
   */
  url: string | null;
  /** The destination's delete URL, when it hands one back. Same serde default. */
  deletionUrl: string | null;
  /**
   * How long a recording runs, in milliseconds, or `null` for everything else
   * (M4 §5).
   *
   * `#[serde(default)]` on the Rust side for the same reason `url` is: the 82
   * records already on this machine carry no such key and must keep loading, so
   * a pre-M4 record arrives here with the field **absent** rather than null.
   * Test it for truthiness, never for `null`.
   */
  durationMs: number | null;
}

/* ------------------------------------------------- screen recording (M4) */

/** The two containers M4 ships. Mirrors `RecordFormat` on the Rust side. */
export const RECORD_FORMATS = ['mp4', 'gif'] as const;

export type RecordFormat = (typeof RECORD_FORMATS)[number];

/** What an unrecognised `Settings.recordFormat` is read as. */
export const DEFAULT_RECORD_FORMAT: RecordFormat = 'mp4';

export function isRecordFormat(value: unknown): value is RecordFormat {
  return typeof value === 'string' && (RECORD_FORMATS as readonly string[]).includes(value);
}

/**
 * `Settings.recordFormat` as one of the two, coerced rather than refused: unlike
 * `FtpSettings.security`, where guessing would misdescribe a live connection,
 * the worst a wrong guess does here is start a recording in the other container,
 * and a picker with no selection at all is worse.
 */
export function recordFormatOf(value: string | undefined | null): RecordFormat {
  const found = (RECORD_FORMATS as readonly string[]).find(
    (format) => format === (value ?? '').trim().toLowerCase()
  );
  return (found as RecordFormat | undefined) ?? DEFAULT_RECORD_FORMAT;
}

export const RECORD_FORMAT_LABEL: Record<RecordFormat, string> = {
  mp4: 'MP4',
  gif: 'GIF'
};

/**
 * Bounds and defaults for the recording settings, beside the fields they
 * describe so the sliders and Rust's clamps cannot drift apart — the same
 * arrangement as `SCROLL_*` above. `def` is what a fresh install writes (M4 §6).
 */
export const RECORD_FPS = { min: 5, max: 60, step: 1, def: 30 } as const;
export const RECORD_GIF_FPS = { min: 5, max: 30, step: 1, def: 15 } as const;
export const RECORD_GIF_MAX_WIDTH = { min: 320, max: 1920, step: 40, def: 800 } as const;

/** Where the frames come from (M4 §2). All three reuse an existing picker. */
export const RECORD_SOURCES = ['region', 'window', 'monitor'] as const;

export type RecordSourceKind = (typeof RECORD_SOURCES)[number];

/**
 * The source half of a `RecordSpec`. Internally tagged on **`type`**, matching
 * `#[serde(tag = "type", rename_all = "camelCase")] enum RecordSource` — not
 * `kind`, which is what `CaptureRecord` already uses for something else.
 *
 * A region carries its rect in **absolute xcap physical screen pixels**, the
 * same space `WindowRect` and `ArmPayload.originX/originY` live in: a caller
 * working from the region overlay adds the freeze frame's origin before sending.
 * Window and monitor carry the id from the pickers the capture view already has.
 *
 * The rect is fixed at start whichever source it came from — a window that moves
 * or resizes mid-recording keeps the rect it had (M4 §2).
 */
export type RecordSource =
  | { type: 'region'; x: number; y: number; width: number; height: number }
  | { type: 'window'; id: number }
  | { type: 'monitor'; id: number };

/**
 * What `start_recording` is asked for.
 *
 * Everything but the source is optional, and **omitting it means "whatever
 * Settings says"**. An explicit value overrides for this one recording and is
 * never written back, so the capture screen's format picker does not silently
 * rewrite the setting the user chose in Settings.
 */
export interface RecordSpec {
  source: RecordSource;
  format?: RecordFormat;
  fps?: number;
  captureCursor?: boolean;
  hwEncode?: boolean;
}

/**
 * The recorder's whole model: what `recording_status()` returns and what
 * `record://status` carries. One event, emitted on start, about four times a
 * second while a recording runs, and once more as `idle` when it ends.
 *
 * `active: false` with `phase: 'idle'` is the resting state, and it is a real
 * value rather than an absence — the HUD reads it as "there is nothing to
 * describe" and takes itself off screen.
 */
export interface RecordStatus {
  active: boolean;
  /** Identifies the recording, so a late event for a finished one is ignorable. */
  id: string;
  /** One of `RECORD_PHASES`, and a plain string so an added phase still renders. */
  phase: string;
  /** Wall clock since the first frame. The file may be shorter — see `dropped`. */
  elapsedMs: number;
  frames: number;
  /**
   * Frames the encoder could not be handed. **Not** the frames the pacer skipped
   * because the screen updates faster than the recording rate — those are
   * downsampling, not loss — so a non-zero value here is worth showing.
   */
  dropped: number;
  width: number;
  height: number;
  fps: number;
  /** `'mp4'` or `'gif'`. */
  format: string;
  /** `'region'`, `'window'` or `'monitor'`. */
  source: string;
  cursor: boolean;
  /** The MP4 is being encoded on the GPU. */
  hardware: boolean;
  /** The stop accelerator, or `''` when nothing claimed it — say so, never lie. */
  stopHotkey: string;
  outputName: string;
}

/**
 * The phases a recording moves through. Read for display only: `phase` is typed
 * as a plain string precisely so a build that grows a phase this one has never
 * heard of still renders it rather than falling through to nothing.
 */
export const RECORD_PHASES = [
  'idle',
  'starting',
  'recording',
  'finishing',
  'encoding',
  'done'
] as const;

/** What a finished recording produced. Payload of `record://finished` / `record://cancelled`. */
export interface RecordOutcome {
  /** `null` for a cancel, which by definition kept nothing. */
  record: CaptureRecord | null;
  frames: number;
  dropped: number;
  durationMs: number;
  cancelled: boolean;
  /**
   * ffmpeg had to be killed, so the file may be missing its last moments. Said
   * out loud rather than hidden behind a record that looks like any other.
   */
  truncated: boolean;
}

/* ------------------------------------------------------------- ffmpeg (M4 §1) */

/** One thing that is not there. `id` is switchable; `label` is a noun phrase. */
export interface FfmpegMissing {
  id: string;
  label: string;
}

/**
 * A fix the user can run, verbatim. `command` is **empty** for
 * `FFMPEG_REMEDY_BROWSE`, which is a control in the UI rather than something to
 * paste — so render the commands and the Browse button from the same list
 * instead of hard-coding either.
 */
export interface FfmpegRemedy {
  id: string;
  label: string;
  command: string;
}

export const FFMPEG_REMEDY_BROWSE = 'browse';

/** A location that was tried, and what came of it. */
export interface FfmpegAttempt {
  source: string;
  path: string;
  /** `'ok'` | `'notFound'` | `'unusable'`. */
  outcome: string;
  detail: string | null;
}

/**
 * Whether recording can be offered at all, and what to say when it cannot
 * (M4 §1).
 *
 * santi.sharex **finds** ffmpeg and never downloads one — fetching and executing
 * an ~80MB binary sits badly beside M3 §1's consent rules — so "unavailable" is
 * a state the UI renders properly rather than a transient one it waits out.
 * `missing` says what is absent, `remedies` are the exact commands that fix it,
 * and `searched` is where santi.sharex looked, so a user whose ffmpeg lives
 * somewhere unusual can see that it did.
 *
 * **Never render a download affordance from any of this.**
 */
export interface FfmpegAvailability {
  /** True only when a binary answered *and* it has every encoder M4 needs. */
  available: boolean;
  path: string;
  version: string;
  /** Which resolution step hit: `setting`, `path`, `scoop`, `winget`, … */
  source: string;
  encoders: string[];
  hardwareEncoders: string[];
  /** Empty exactly when `available` is true. */
  missing: FfmpegMissing[];
  remedies: FfmpegRemedy[];
  searched: FfmpegAttempt[];
}

/**
 * How each resolution step is named on screen. A source this build has no name
 * for falls back to the raw id rather than to a blank, because the point of
 * showing it at all is answering "why is it using *that* ffmpeg".
 */
export const FFMPEG_SOURCE_LABEL: Record<string, string> = {
  setting: 'the path you set',
  path: 'PATH',
  scoop: 'scoop',
  winget: 'winget',
  programFiles: 'Program Files'
};

/** Whether this record is a screen recording rather than a still image. */
export function captureIsRecording(record: CaptureRecord): boolean {
  return record.kind === 'recording';
}

/** The record's file extension, lowercased and without the dot. `''` when none. */
export function captureExtension(record: CaptureRecord): string {
  const name = record.path || record.name;
  const dot = name.lastIndexOf('.');
  return dot === -1 ? '' : name.slice(dot + 1).toLowerCase();
}

/**
 * Whether the record needs a `<video>` element rather than an `<img>` (M4 §5).
 *
 * A recording is not automatically a video: a GIF recording *is* an image as far
 * as the webview is concerned, and it animates in an `<img>` while a `<video>`
 * would show nothing at all. So this asks about the file, not about the kind.
 */
export function capturePlaysAsVideo(record: CaptureRecord): boolean {
  return captureIsRecording(record) && captureExtension(record) !== 'gif';
}

/**
 * Formats such as `MM:SS`, or `H:MM:SS` past an hour. For durations, so it pads
 * minutes and seconds but never the leading unit.
 */
export function formatDuration(ms: number): string {
  const total = Math.max(0, Math.round(ms / 1000));
  const seconds = total % 60;
  const minutes = Math.floor(total / 60) % 60;
  const hours = Math.floor(total / 3600);
  const pad = (n: number) => String(n).padStart(2, '0');
  return hours > 0 ? `${hours}:${pad(minutes)}:${pad(seconds)}` : `${minutes}:${pad(seconds)}`;
}

export interface MonitorInfo {
  id: number;
  name: string;
  x: number;
  y: number;
  width: number;
  height: number;
  isPrimary: boolean;
  scaleFactor: number;
}

export interface WindowInfo {
  id: number;
  title: string;
  appName: string;
  width: number;
  height: number;
}

/**
 * A window's geometry in **xcap screen coordinates**, enumerated in the same
 * instant as the freeze frame so the rects describe the frozen pixels rather
 * than the live desktop. Shipped in `ArmPayload.windows`, topmost first by `z`
 * — hit-testing takes the first match. santi.sharex's own windows are already
 * filtered out on the Rust side.
 *
 * Convert to freeze-image pixels once, on arm: `x - originX`, `y - originY`.
 */
export interface WindowRect {
  id: number;
  title: string;
  appName: string;
  x: number;
  y: number;
  width: number;
  height: number;
  z: number;
}

/**
 * Payload of `overlay://arm` (M2.5 §1). The overlay window is pre-warmed and
 * lives for the whole session, so this — not a page load — is what tells it a
 * capture has begun.
 *
 * `src` arrives as an absolute filesystem path; `onOverlayArm()` in `$lib/api`
 * converts it, so consumers get a webview-loadable URL. Decode it through
 * `fetch` → `blob` → `createImageBitmap`, never `new Image()` (M2.5 §0).
 */
export interface ArmPayload {
  src: string;
  /** Freeze-image pixels. */
  width: number;
  height: number;
  /** Virtual-desktop origin in xcap space. */
  originX: number;
  originY: number;
  windows: WindowRect[];
  /** `Settings.annotateInOverlay`, resolved at arm time. */
  annotate: boolean;
  /**
   * Monotonic. Hand it straight back to `overlayReady()`; an arm superseded
   * before the overlay was ready is ignored rather than shown stale.
   */
  armId: number;
}

/**
 * Rust returns `src` as a plain absolute filesystem path; `getFreezeFrame()` in
 * `$lib/api` runs it through `convertFileSrc` before it reaches a consumer, so
 * by the time you hold a `FreezeInfo` the `src` is webview-loadable.
 */
export interface FreezeInfo {
  src: string;
  width: number;
  height: number;
}

/** Crop rect in freeze-image pixel coordinates — the only space Rust understands. */
export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

/**
 * Top-level navigation inside the `main` window.
 *
 * `workflows` joins the three original entries in M6 §4 — it is a screen of its
 * own in both shells rather than a section of Settings, because a workflow is a
 * thing you run, not a preference.
 */
export type View = 'capture' | 'history' | 'workflows' | 'settings';

/* -------------------------------------------------------------------- OCR */

/**
 * One line as the engine segmented it, in reading order (M5 §3). A line is not
 * a paragraph and not a sentence — `Windows.Media.Ocr` splits on the layout it
 * sees, so a two-column page interleaves. That is why the UI offers the lines
 * *and* the joined block rather than only one of them.
 */
export interface OcrLine {
  text: string;
}

/** Result of `ocr_capture`. */
export interface OcrResult {
  /** Every line joined with newlines. Empty when nothing was recognised. */
  text: string;
  lines: OcrLine[];
  /** BCP-47 tag of the engine that ran, e.g. `en-US`. */
  language: string;
}

/* ---------------------------------------------------- scrolling capture */

/**
 * Why a scrolling run ended (M5 §4), mirroring `Stop::code` in `scroll.rs`.
 * Only `end` is the bottom of the content; every other value stopped short.
 *
 * This exists to be *read*, not to be branched on — `ScrollOutcome.incomplete`
 * is the flag that decides how a run is presented, and `message` is the prose.
 * Anything here that this build has no opinion about is still handled, because
 * neither of those two depends on recognising the code.
 */
export const SCROLL_STOP_REASONS = [
  'end',
  'maxFrames',
  'cancelled',
  'noOverlap',
  'resized',
  'obscured',
  'pointerMoved',
  'pointerLost',
  'wheelBlocked',
  'heightLimit',
  'captureFailed'
] as const;

export type ScrollStopReason = (typeof SCROLL_STOP_REASONS)[number];

export function isScrollStopReason(value: unknown): value is ScrollStopReason {
  return typeof value === 'string' && (SCROLL_STOP_REASONS as readonly string[]).includes(value);
}

/** Payload of `scroll://progress`, emitted after every grab. */
export interface ScrollProgress {
  /** Frames grabbed, duplicates included. */
  frames: number;
  /** Frames actually merged into the image. */
  stitched: number;
  /** The run's frame budget, already clamped by Rust. */
  maxFrames: number;
  /** Height of the stitch so far, in image pixels. */
  height: number;
}

/**
 * What `start_scroll_capture` produced.
 *
 * There is always a `record` — the run finalizes whatever it managed to stitch,
 * so a cancel and a lost overlap both still hand back an image — and there is
 * always a `message`. `incomplete` is the one the UI must not ignore: it is
 * true whenever the run ended before the bottom of the content *or* nothing
 * scrolled at all, and a truncated stitch is indistinguishable from a complete
 * one by looking at it.
 */
export interface ScrollOutcome {
  record: CaptureRecord;
  frames: number;
  stitched: number;
  width: number;
  height: number;
  reason: ScrollStopReason;
  /** One finished sentence for the user, populated on success too. */
  message: string;
  incomplete: boolean;
}

/* ------------------------------------------------- upload destinations (M3) */

/**
 * The three destinations M3 ships, by the ids Rust dispatches on — mirrors
 * `DESTINATIONS` in `store.rs` and `DestinationKind::id()` in `upload/mod.rs`.
 *
 * SFTP is deliberately absent: it needs an SSH stack this build has no C
 * toolchain for, and a picker entry that always fails would be worse than
 * saying so (M3 §5). The FTP form says it in words; nothing here may grow an
 * `'sftp'` member that does not work.
 */
export const DESTINATION_KINDS = ['imgur', 'custom', 'ftp'] as const;

export type DestinationKind = (typeof DESTINATION_KINDS)[number];

/**
 * `Settings.destination`. `'none'` is the shipped default and the state a fresh
 * install stays in until the user chooses otherwise — with it, no capture can
 * leave the machine however the other switches are set (M3 §1). Rust's
 * `normalize_destination` turns anything it does not recognise into this rather
 * than into some destination.
 */
export type DestinationChoice = DestinationKind | 'none';

export function isDestinationKind(value: unknown): value is DestinationKind {
  return typeof value === 'string' && (DESTINATION_KINDS as readonly string[]).includes(value);
}

/**
 * Display names, matching `DestinationKind::label()` so the two sides never
 * name the same destination differently in the same sentence. `'none'` has no
 * Rust counterpart — it is a UI state, not a destination.
 */
export const DESTINATION_LABEL: Record<DestinationChoice, string> = {
  none: 'None',
  imgur: 'Imgur',
  custom: 'Custom uploader',
  ftp: 'FTP'
};

/**
 * Where Imgur hands out a Client ID. Shown verbatim beside the field, because
 * an empty box with no explanation reads as broken rather than as unconfigured
 * (M3 §3) — santi.sharex embeds no client ID of its own and never will.
 */
export const IMGUR_REGISTER_URL = 'https://api.imgur.com/oauth2/addclient';

/**
 * How the FTP connection is secured. These are the exact strings
 * `FtpSettings.security` holds — `FTP_SECURITY_PLAIN` / `_EXPLICIT_TLS` /
 * `_SFTP` in `store.rs`, which `upload::ftp` matches on, case-insensitively,
 * before it reads a password.
 *
 * `sftp` is listed because Rust's constant is. Nothing in this UI may ever
 * *offer* it — SFTP is a subsystem of SSH and shares no code with FTPS, and
 * santi.sharex carries no SSH stack (M3 §5) — but a hand-edited `settings.json`
 * can name it, and `ftp.rs` refuses it by name rather than guessing. A UI that
 * did not know the value existed would render it as "not encrypted" and warn
 * about clear text, which is not what happens: nothing is sent at all.
 */
export const FTP_SECURITY = {
  plain: 'plain',
  explicitTls: 'explicitTls',
  sftp: 'sftp'
} as const;

export type FtpSecurity = (typeof FTP_SECURITY)[keyof typeof FTP_SECURITY];

/**
 * Everything about an FTP destination that is *not* a secret. The password is
 * never here, never in `settings.json`, and never crosses the IPC back (M3 §2).
 */
export interface FtpSettings {
  host: string;
  port: number;
  username: string;
  /** Remote directory the file is written into, e.g. `/public_html/shots`. */
  remoteDir: string;
  passive: boolean;
  /**
   * One of `FTP_SECURITY`, and typed as a plain `string` for the same reason
   * Rust types it `String`: an unrecognised value must not make the settings
   * file fail to parse and take every other setting down with it. Unlike
   * `theme` there is no coercion on the way in — `ftp.rs` **refuses** a mode it
   * does not know rather than downgrading it to plain — so the UI has to be
   * able to represent a value outside the three. Read it through
   * `ftpSecurityOf()`, never by comparing it to a literal.
   */
  security: string;
  /**
   * Public URL the remote directory is served at, so the copied link points at
   * the web host rather than at an `ftp://` path. Empty means the upload
   * reports the remote path instead of a link.
   */
  urlPrefix: string;
}

/**
 * `FtpSettings.security` as one of the three known modes, or `null` when it is
 * something else — a file written by a future build, or edited by hand.
 *
 * Case-folded because `ftp.rs` compares with `eq_ignore_ascii_case`: a settings
 * file saying `"ExplicitTLS"` uploads over TLS, and a picker that showed it as
 * unencrypted would be describing a connection that is not the one being made.
 */
export function ftpSecurityOf(value: string | undefined | null): FtpSecurity | null {
  const found = (Object.values(FTP_SECURITY) as string[]).find(
    (mode) => mode.toLowerCase() === (value ?? '').trim().toLowerCase()
  );
  return (found as FtpSecurity | undefined) ?? null;
}

/**
 * What a never-configured FTP destination looks like; mirrors Rust's `Default`,
 * **including `security: 'plain'`**. Rust is the authority on what an
 * unconfigured destination is, and a frontend default of `explicitTls` here
 * would draw an encrypted connection over a stored one that is not — the one
 * lie this form must never tell (M3 §5).
 */
export const FTP_DEFAULTS: FtpSettings = {
  host: '',
  port: 21,
  username: '',
  remoteDir: '',
  passive: true,
  security: FTP_SECURITY.plain,
  urlPrefix: ''
};

/**
 * An imported ShareX `.sxcu`, reduced to the subset M3 §4 supports and stored
 * as ordinary configuration. Field names transcribe the `.sxcu` vocabulary, so
 * a user comparing this against the original in Notepad recognises every line.
 *
 * There is no `null` state: an **empty `name` and `requestUrl` mean nothing has
 * been imported**, which is what Rust's `Default` writes. Test `requestUrl`
 * rather than the object.
 *
 * Anything outside the subset — OAuth, `RegexList`, a body type other than the
 * two, a non-image destination — is rejected at *import* time with a message
 * naming the field, so this type never has to represent it. That message is the
 * user's only explanation, so the UI shows it verbatim.
 */
export interface CustomUploaderSettings {
  /** `Name` from the file. Empty means nothing has been imported. */
  name: string;
  /** `"POST"` or `"PUT"` — a plain string, because the file supplies it. */
  requestMethod: string;
  requestUrl: string;
  /** Non-secret headers only; the credential-looking ones went to the store. */
  headers: Record<string, string>;
  /** `Parameters`, or `Arguments` in older files. */
  parameters: Record<string, string>;
  /** `"MultipartFormData"` or `"Binary"`. */
  body: string;
  fileFormName: string;
  /** Response templates: `$json:path.to.field$` / `$response$`. */
  url: string;
  thumbnailUrl: string;
  deletionUrl: string;
  /**
   * Header names whose values live in Credential Manager. The *names* are not
   * secret — they are what lets this page show a "Configured" indicator. The
   * values are gone from here for good.
   */
  secretHeaders: string[];
}

/**
 * The "nothing imported" value, mirroring Rust's `Default`. Writing this back
 * through `saveSettings` is how an imported uploader is forgotten — clear its
 * credentials with `clearDestinationSecret` **first**, or their names go with
 * the config and the values are left orphaned in Credential Manager.
 */
export const EMPTY_CUSTOM_UPLOADER: CustomUploaderSettings = {
  name: '',
  requestMethod: '',
  requestUrl: '',
  headers: {},
  parameters: {},
  body: '',
  fileFormName: '',
  url: '',
  thumbnailUrl: '',
  deletionUrl: '',
  secretHeaders: []
};

/**
 * One credential slot a destination defines — **a boolean, never a value**
 * (M3 §2). `field` is what goes back to `setDestinationSecret` /
 * `clearDestinationSecret`; `label` is what to call it on screen.
 *
 * Rust builds this list, which is why nothing in `src/` hard-codes a field
 * name: a custom uploader's slots are whichever headers its `.sxcu` declared.
 */
export interface SecretStatus {
  field: string;
  label: string;
  /** Whether an upload to this destination fails without it. */
  required: boolean;
  /** Whether one is stored. The whole of what the frontend ever learns. */
  set: boolean;
}

/** One row of `destination_status()`, which returns all three in a fixed order. */
export interface DestinationStatus {
  kind: DestinationKind;
  label: string;
  /**
   * Everything this destination needs is present — its non-secret config *and*
   * every `required` secret — so an upload has a chance of working.
   */
  configured: boolean;
  /** This is the destination uploads currently go to. */
  active: boolean;
  secrets: SecretStatus[];
}

/**
 * Whether anything can be uploaded right now: the active destination exists and
 * is `configured`. The one question the capture preview asks before offering an
 * upload affordance at all (M3 §6).
 */
export function canUploadNow(status: DestinationStatus[] | null): boolean {
  return !!status?.some((d) => d.active && d.configured);
}

/**
 * File extensions a destination will take, or `null` for "anything" (M4 §5).
 *
 * Only Imgur constrains, and it is the one whose list can be stated as fact: it
 * publishes what it accepts, and a video it rejects comes back as an error the
 * user cannot act on. FTP is a file server and takes any bytes. A custom
 * uploader depends entirely on its `.sxcu`, which declares no format — so the
 * honest answer there is "try it", with the server's own refusal as the
 * authority, exactly as M3 treats every other custom-uploader failure.
 */
export const DESTINATION_ACCEPTS: Record<DestinationKind, readonly string[] | null> = {
  imgur: ['png', 'jpg', 'jpeg', 'gif', 'apng', 'tiff', 'mp4', 'webm', 'mov', 'avi'],
  custom: null,
  ftp: null
};

/**
 * Whether the active destination will take this record's format.
 *
 * The gate on offering the Upload action for a recording (M4 §5): an MP4 button
 * that a destination is going to refuse is worse than no button. Still images
 * are unaffected — every destination has always taken a PNG — so this only ever
 * subtracts an affordance for a format that is genuinely at risk.
 */
export function destinationAcceptsCapture(
  destination: DestinationChoice,
  record: CaptureRecord
): boolean {
  if (destination === 'none') return false;
  const accepts = DESTINATION_ACCEPTS[destination];
  if (accepts === null) return true;
  const ext = captureExtension(record);
  // No extension to judge by — a record whose file was never written. The
  // caller's own `hasFile` check is what stops that reaching an upload.
  return ext === '' || accepts.includes(ext);
}

/** Payload of `upload://progress`. */
export interface UploadProgress {
  /** `CaptureRecord.id` — the only way to tell whose progress this is. */
  id: string;
  /**
   * Bytes of the *image* handed to the destination, not bytes on the wire: a
   * multipart envelope is a few hundred bytes the user did not ask about, and
   * counting them would put the bar at 100% before the request was done.
   */
  sent: number;
  /** Total image bytes, or `0` when unknown — indeterminate, not zero-length. */
  total: number;
}

/** Payload of `upload://done`. The record's own `url` follows on `capture://updated`. */
export interface UploadDone {
  id: string;
  url: string;
}

/**
 * Payload of `upload://error`.
 *
 * `message` is shown as it arrives — Rust guarantees it never interpolates a
 * credential (M3 §2), and it is the only thing that says whether to retry, fix
 * a credential, or wait out a rate limit.
 *
 * `cancelled` is an addition to the shape in M3 §6, and it matters: a cancel
 * ends on this event too, and raising a red toast because the user got exactly
 * what they asked for is a bug. Read the flag; never match on the message.
 */
export interface UploadError {
  id: string;
  message: string;
  cancelled: boolean;
}

/* ---------------------------------------------------------- accelerators */

/**
 * Modifier spellings that all mean the same key, folded to the one form
 * `Settings.hotkeys` is written in.
 *
 * `Control` folds into `CmdOrCtrl` because this is a Windows-only app and they
 * are the same physical key here — a workflow bound to `Control+Shift+1` and
 * the region hotkey on `CmdOrCtrl+Shift+1` are one combination, and a conflict
 * check that could not see that would let the user bind a collision the
 * registry then resolves by silence.
 */
const MODIFIER_ALIASES: Record<string, string> = {
  cmdorctrl: 'CmdOrCtrl',
  commandorcontrol: 'CmdOrCtrl',
  ctrl: 'CmdOrCtrl',
  control: 'CmdOrCtrl',
  cmd: 'CmdOrCtrl',
  command: 'CmdOrCtrl',
  alt: 'Alt',
  option: 'Alt',
  shift: 'Shift',
  super: 'Super',
  meta: 'Super',
  win: 'Super'
};

/** Tauri writes modifiers in this order; so does anything built here. */
const MODIFIER_ORDER = ['CmdOrCtrl', 'Alt', 'Shift', 'Super'];

/**
 * One accelerator in a single canonical spelling, so two that name the same
 * combination compare equal whichever order or alias they were written in.
 * `''` for an accelerator with no key in it, which is "not bound to anything"
 * rather than a combination that matches everything.
 */
export function normalizeAccelerator(accel: string | null | undefined): string {
  const parts = (accel ?? '')
    .split('+')
    .map((part) => part.trim())
    .filter((part) => part !== '');
  const modifiers = new Set<string>();
  const keys: string[] = [];
  for (const part of parts) {
    const modifier = MODIFIER_ALIASES[part.toLowerCase()];
    if (modifier) modifiers.add(modifier);
    else keys.push(part.length === 1 ? part.toUpperCase() : part);
  }
  if (keys.length === 0) return '';
  return [...MODIFIER_ORDER.filter((m) => modifiers.has(m)), ...keys].join('+');
}

/**
 * Whether two accelerators are the same combination. An empty one matches
 * nothing, including another empty one — "unset" is not a collision.
 */
export function sameAccelerator(a: string | null | undefined, b: string | null | undefined): boolean {
  const left = normalizeAccelerator(a).toLowerCase();
  const right = normalizeAccelerator(b).toLowerCase();
  return left !== '' && left === right;
}

/**
 * An accelerator split into the parts a row of `<kbd>` renders, in the words
 * printed on a Windows keyboard rather than Tauri's.
 */
export function displayAccelerator(accel: string | null | undefined): string[] {
  if (!accel) return ['Not set'];
  return accel
    .split('+')
    .map((part) => part.trim())
    .filter((part) => part !== '')
    .map((part) => {
      if (part === 'CmdOrCtrl' || part === 'CommandOrControl') return 'Ctrl';
      if (part === 'Super' || part === 'Meta') return 'Win';
      if (part === 'PrintScreen') return 'Print Screen';
      return part;
    });
}

/**
 * The physical key a `KeyboardEvent` landed on, as an accelerator base — layout
 * independent, so a Dvorak or AZERTY user binds the key they pressed rather than
 * the letter it produced. `null` for a bare modifier, which is not a
 * combination.
 */
function acceleratorKey(code: string, key: string): string | null {
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) return code;
  if (/^Numpad[0-9]$/.test(code)) return code;
  const named = new Set([
    'PrintScreen',
    'Space',
    'Enter',
    'Tab',
    'Backspace',
    'Delete',
    'Insert',
    'Home',
    'End',
    'PageUp',
    'PageDown',
    'ArrowUp',
    'ArrowDown',
    'ArrowLeft',
    'ArrowRight',
    'Minus',
    'Equal',
    'BracketLeft',
    'BracketRight',
    'Backslash',
    'Semicolon',
    'Quote',
    'Comma',
    'Period',
    'Slash',
    'Backquote',
    'CapsLock',
    'NumLock',
    'ScrollLock',
    'Pause'
  ]);
  if (named.has(code)) return code;
  if (key === 'PrintScreen') return 'PrintScreen';
  return null;
}

/**
 * The accelerator a keypress describes, or `null` when it does not describe one.
 *
 * Shared by every click-to-record control in the app — Settings' four rows and a
 * workflow's trigger — so a combination typed in one place is spelled exactly
 * the way the other would have spelled it. That is what makes the M6 §3 conflict
 * check able to compare them at all.
 */
export function acceleratorFromEvent(event: KeyboardEvent): string | null {
  const base = acceleratorKey(event.code, event.key);
  if (!base) return null;
  const parts: string[] = [];
  if (event.ctrlKey) parts.push('CmdOrCtrl');
  if (event.altKey) parts.push('Alt');
  if (event.shiftKey) parts.push('Shift');
  if (event.metaKey) parts.push('Super');
  parts.push(base);
  return parts.join('+');
}

/* ------------------------------------------------------------ workflows (M6) */

/**
 * A workflow chains **capture → actions → destination** and binds the whole
 * chain to one hotkey (M6 §1).
 *
 * Stored in `workflows.json` beside `settings.json`, never inside it — a corrupt
 * workflow must not cost the user their hotkeys and save directory.
 *
 * M6 adds no new capability. Every step below is an existing path called in
 * order, which is why none of these types describes anything the rest of the app
 * cannot already do: if a step here needs new plumbing, the step is wrong.
 */
export interface Workflow {
  id: string;
  name: string;
  /** Off means the hotkey is unregistered and only Run now reaches it. */
  enabled: boolean;
  trigger: WorkflowTrigger;
  capture: WorkflowCapture;
  actions: WorkflowAction[];
  /**
   * A destination id, or `null` to skip uploading entirely.
   *
   * A plain `string` for the same reason `Settings.destination` is: a
   * hand-edited or future `workflows.json` can name something this build has no
   * uploader for. Read it through `workflowDestinationOf()`, which answers
   * `null` for both "no destination" and "a destination this build cannot
   * reach" — `workflowUploads()` is what tells those two apart, and the
   * difference matters because the second is an error and the first is a
   * perfectly ordinary local workflow.
   */
  destination: string | null;
}

/**
 * All three workflow enums are internally tagged on **`type`**, matching
 * `#[serde(tag = "type", rename_all = "camelCase")]` — the same arrangement
 * `RecordSource` already uses, and for the same reason: `kind` is taken by
 * `CaptureRecord`.
 */
export type WorkflowTrigger = { type: 'hotkey'; accelerator: string } | { type: 'manual' };

/**
 * Which capture path the workflow opens with. Every member names a path the app
 * already has: `region` arms the pre-warmed overlay and waits for the commit,
 * `scrolling` is M5 §4's stitcher, `record` is M4's recorder.
 */
export type WorkflowCapture =
  | { type: 'region' }
  | { type: 'fullscreen' }
  | { type: 'activeWindow' }
  | { type: 'monitor'; id: number }
  | { type: 'window'; id: number }
  | { type: 'scrolling'; window: number }
  /** `'mp4'` or `'gif'`, read through `recordFormatOf()` like every other. */
  | { type: 'record'; format: string };

/**
 * One step between the capture and the destination, in listed order.
 *
 * `annotate` opens the editor and **blocks the rest of the chain** until it is
 * saved or cancelled (M6 §2). That wait is the whole point of the step: without
 * it a redaction workflow uploads the un-redacted image, which is a privacy
 * failure rather than a bug.
 */
export type WorkflowAction =
  | { type: 'annotate' }
  | { type: 'saveToDisk' }
  | { type: 'copyImage' }
  | { type: 'ocr'; copyText: boolean }
  | { type: 'openFolder' };

export const WORKFLOW_CAPTURE_KINDS = [
  'region',
  'fullscreen',
  'activeWindow',
  'monitor',
  'window',
  'scrolling',
  'record'
] as const;

export type WorkflowCaptureKind = (typeof WORKFLOW_CAPTURE_KINDS)[number];

export const WORKFLOW_ACTION_KINDS = [
  'annotate',
  'saveToDisk',
  'copyImage',
  'ocr',
  'openFolder'
] as const;

export type WorkflowActionKind = (typeof WORKFLOW_ACTION_KINDS)[number];

/**
 * The terse names the chain summary is built from — `Region → Annotate → Save →
 * Imgur`. Short on purpose: the summary has to fit on one row beside everything
 * else, and it is the row's most load-bearing text.
 */
export const WORKFLOW_CAPTURE_LABEL: Record<WorkflowCaptureKind, string> = {
  region: 'Region',
  fullscreen: 'Fullscreen',
  activeWindow: 'Active window',
  monitor: 'Monitor',
  window: 'Window',
  scrolling: 'Scrolling',
  record: 'Record'
};

export const WORKFLOW_ACTION_LABEL: Record<WorkflowActionKind, string> = {
  annotate: 'Annotate',
  saveToDisk: 'Save',
  copyImage: 'Copy',
  ocr: 'OCR',
  openFolder: 'Open folder'
};

/**
 * The unabbreviated names, for the editor — where there is room to say what a
 * step does and no reason to make the user expand `OCR` in their head. Kept
 * apart from the terse set rather than derived from it, because "Save" in a
 * one-line chain and "Save to disk" in a list of steps are both right.
 */
export const WORKFLOW_CAPTURE_NAME: Record<WorkflowCaptureKind, string> = {
  region: 'Region',
  fullscreen: 'Fullscreen',
  activeWindow: 'Active window',
  monitor: 'Monitor',
  window: 'Window',
  scrolling: 'Scrolling capture',
  record: 'Screen recording'
};

export const WORKFLOW_ACTION_NAME: Record<WorkflowActionKind, string> = {
  annotate: 'Annotate',
  saveToDisk: 'Save to disk',
  copyImage: 'Copy image',
  ocr: 'Read text (OCR)',
  openFolder: 'Open folder'
};

/** What each step is for, one line, shown where the step is chosen. */
export const WORKFLOW_ACTION_HELP: Record<WorkflowActionKind, string> = {
  annotate: 'Open the editor and wait. Nothing after this runs until you save or cancel.',
  saveToDisk: 'Write the image into the screenshots folder.',
  copyImage: 'Put the image on the clipboard.',
  ocr: 'Recognise the text in the image with the offline Windows engine.',
  openFolder: 'Reveal the file in Explorer.'
};

/**
 * A label from one of the maps above, falling back to the raw tag.
 *
 * A `workflows.json` written by a future build can carry a step this one has
 * never heard of. Showing its tag is honest; showing a blank segment in the one
 * line that says what a workflow does is not.
 */
function labelOf(table: Record<string, string>, tag: string): string {
  return table[tag] ?? tag;
}

/** The chain summary's first segment, e.g. `Region` or `Record MP4`. */
export function workflowCaptureSummary(capture: WorkflowCapture): string {
  if (capture.type === 'record') {
    return `Record ${RECORD_FORMAT_LABEL[recordFormatOf(capture.format)]}`;
  }
  return labelOf(WORKFLOW_CAPTURE_LABEL, capture.type);
}

export function workflowActionSummary(action: WorkflowAction): string {
  return labelOf(WORKFLOW_ACTION_LABEL, action.type);
}

/** Whether this workflow ends by sending the capture off the machine. */
export function workflowUploads(workflow: Workflow): boolean {
  return (workflow.destination ?? '').trim() !== '';
}

/**
 * The workflow's destination as one this build can actually reach, or `null`.
 *
 * `null` covers both "no destination" and "an id this build has no uploader
 * for". Pair it with `workflowUploads()` to tell them apart: the first is an
 * ordinary local workflow, the second is a workflow that would run every step
 * and then fail, which the editor refuses.
 */
export function workflowDestinationOf(value: string | null | undefined): DestinationKind | null {
  const id = (value ?? '').trim().toLowerCase();
  return isDestinationKind(id) ? id : null;
}

/** One segment of the one-line chain summary. */
export interface WorkflowChainSegment {
  text: string;
  /**
   * This segment is the destination — the only part of the chain with
   * consequences off this machine, and so the part that must survive
   * truncation and must not read like the steps beside it.
   */
  uploads: boolean;
}

/** What separates the segments, and the only place it is spelled. */
export const WORKFLOW_CHAIN_ARROW = ' → ';

/** Roughly how much of the chain fits on one row before it has to give way. */
export const WORKFLOW_CHAIN_MAX_CHARS = 58;

/**
 * The whole chain, capture first and destination last.
 *
 * **This is the feature, not decoration** (M6 §4): it is what lets a user tell
 * that a hotkey uploads their screen without opening the workflow. So it is
 * built from the workflow itself rather than from a stored description, it names
 * every step, and an unknown step still gets a segment.
 */
export function workflowChain(workflow: Workflow): WorkflowChainSegment[] {
  const segments: WorkflowChainSegment[] = [
    { text: workflowCaptureSummary(workflow.capture), uploads: false }
  ];
  for (const action of workflow.actions) {
    segments.push({ text: workflowActionSummary(action), uploads: false });
  }
  if (workflowUploads(workflow)) {
    const kind = workflowDestinationOf(workflow.destination);
    segments.push({
      text: kind ? DESTINATION_LABEL[kind] : (workflow.destination ?? '').trim(),
      uploads: true
    });
  }
  return segments;
}

/** A chain measured against a row, with the middle folded away if it must be. */
export interface WorkflowChainLine {
  /** Render these in order, with the marker inserted after `shown[0]`. */
  shown: WorkflowChainSegment[];
  /** Segments folded away, or `0` when the whole chain fits. */
  hidden: number;
  /** The chain in full, for the row's `title`. */
  full: string;
}

/**
 * The chain as it is rendered, truncated **in the middle** when it is too long.
 *
 * The head says what is grabbed and the tail says where it ends up, so the steps
 * in between are what gives way — the destination is the part with
 * consequences, and a summary that elided it would be worse than no summary at
 * all. The capture and the destination are therefore never dropped, whatever the
 * budget.
 */
export function workflowChainLine(
  workflow: Workflow,
  maxChars: number = WORKFLOW_CHAIN_MAX_CHARS
): WorkflowChainLine {
  const segments = workflowChain(workflow);
  const full = segments.map((segment) => segment.text).join(WORKFLOW_CHAIN_ARROW);

  const shown = [...segments];
  let hidden = 0;
  const width = (count: number): number => {
    const base = shown.map((segment) => segment.text).join(WORKFLOW_CHAIN_ARROW).length;
    return count > 0 ? base + WORKFLOW_CHAIN_ARROW.length + 1 + String(count).length : base;
  };
  while (shown.length > 2 && width(hidden + 1) > maxChars) {
    shown.splice(1, 1);
    hidden++;
  }

  return { shown, hidden, full };
}

/* ------------------------------------------------- workflow validation (M6 §4) */

/**
 * Why a workflow will not do what it looks like it does.
 *
 * Every one of these is checked **at edit time**. A workflow that looks fine and
 * fails when you press its hotkey is the outcome this whole type exists to
 * design against — by then the user is in another app, the overlay has already
 * come and gone, and the only evidence is a toast they may not be looking at.
 */
export interface WorkflowIssue {
  /** `error` refuses something; `warning` is said plainly and allowed. */
  level: 'error' | 'warning';
  message: string;
  /**
   * Whether the workflow may still be **saved** with this issue outstanding.
   *
   * An unconfigured destination is fixed on another screen, so refusing the save
   * would leave the draft with nowhere to go — it is refused the *enabled*
   * switch instead, which is what stops it ever firing, and a disabled workflow
   * has no hotkey registered to fail. Everything else here describes a chain
   * that cannot run at all, and those are refused outright.
   */
  savable: boolean;
}

/** Everything `workflowIssues` needs that is not the workflow itself. */
export interface WorkflowContext {
  /** Every workflow, the one under test included — it is matched out by id. */
  workflows: Workflow[];
  /** `destination_status()`, or `null` while it has not been read yet. */
  destinations: DestinationStatus[] | null;
  /** The live settings, for the built-in hotkeys. `null` before they load. */
  settings: Settings | null;
}

/** The built-in accelerator for one action, whichever field backs it. */
function builtinAccelerator(settings: Settings, action: BuiltinHotkeyAction): string {
  return action === 'recordStop'
    ? (settings.recordStopHotkey ?? RECORD_STOP_HOTKEY_DEFAULT)
    : settings.hotkeys[action];
}

/**
 * Actions that need a still image, and the sentence to say when the capture is a
 * screen recording.
 *
 * Not a new rule: these are the same three guards `CaptureCard`, the lightbox
 * and the ShareX shell already make on a recording (M4 §5), moved to the one
 * place where they can be made *before* the hotkey is pressed rather than after
 * the recording has already been taken.
 */
const RECORD_INCOMPATIBLE: Partial<Record<WorkflowActionKind, string>> = {
  annotate:
    'The editor works on still images, and this workflow records a video. Remove Annotate, or change the capture.',
  ocr: 'There is no page of text in a video. Remove Read text, or change the capture.',
  copyImage:
    'A recording cannot go on the clipboard as an image. Remove Copy image, or change the capture.'
};

/**
 * Everything wrong with a workflow, worst first.
 *
 * Pure, and shared by the editor and the list on purpose: a row that shows no
 * warning while the editor shows one would be the same lie in a quieter voice.
 */
export function workflowIssues(workflow: Workflow, context: WorkflowContext): WorkflowIssue[] {
  const issues: WorkflowIssue[] = [];
  const error = (message: string, savable = false) =>
    issues.push({ level: 'error', message, savable });
  const warn = (message: string) => issues.push({ level: 'warning', message, savable: true });

  if (workflow.name.trim() === '') {
    error('Give the workflow a name — it is what the hotkey list and the toasts call it.');
  }

  /* ------------------------------------------------------------- the hotkey */
  if (workflow.trigger.type === 'hotkey') {
    const accel = workflow.trigger.accelerator.trim();
    if (normalizeAccelerator(accel) === '') {
      error('Press a key combination for the trigger, or set it to Run manually.');
    } else {
      const keys = displayAccelerator(accel).join(' + ');
      const settings = context.settings;
      if (settings) {
        for (const action of BUILTIN_HOTKEY_ACTIONS) {
          if (sameAccelerator(builtinAccelerator(settings, action), accel)) {
            error(`${keys} already runs ${BUILTIN_HOTKEY_OWNER[action]}. Pick another combination.`);
          }
        }
      }
      for (const other of context.workflows) {
        if (other.id === workflow.id || other.trigger.type !== 'hotkey') continue;
        if (sameAccelerator(other.trigger.accelerator, accel)) {
          error(`${keys} is already bound to the workflow “${other.name.trim() || 'Untitled'}”.`);
        }
      }
    }
  }

  /* --------------------------------------------------------- the destination */
  if (workflowUploads(workflow)) {
    const kind = workflowDestinationOf(workflow.destination);
    if (!kind) {
      error(
        `This build has no uploader called “${(workflow.destination ?? '').trim()}”. Pick a destination, or clear it so the workflow stays on this machine.`
      );
    } else if (context.destinations) {
      const status = context.destinations.find((entry) => entry.kind === kind);
      if (status && !status.configured) {
        error(
          `${status.label} is not set up yet, so this workflow would take the capture, run every step, and then fail at the upload. Configure it in Destinations, or clear the destination.`,
          true
        );
      }
    }
  }

  /* ----------------------------------------------------------- the chain */
  if (workflow.capture.type === 'record') {
    for (const action of workflow.actions) {
      const message = RECORD_INCOMPATIBLE[action.type];
      if (message) error(message);
    }
  }

  const indexOfAction = (kind: WorkflowActionKind) =>
    workflow.actions.findIndex((action) => action.type === kind);
  const annotateAt = indexOfAction('annotate');
  const saveAt = indexOfAction('saveToDisk');
  const copyAt = indexOfAction('copyImage');

  if (annotateAt >= 0 && saveAt >= 0 && annotateAt > saveAt) {
    warn(
      'Save to disk runs before Annotate, so the file on disk would be the picture without your annotations. Move Annotate above Save to disk.'
    );
  }
  if (annotateAt >= 0 && copyAt >= 0 && annotateAt > copyAt) {
    warn(
      'Copy image runs before Annotate, so the clipboard would hold the picture without your annotations. Move Annotate above Copy image.'
    );
  }

  if (workflow.actions.length === 0 && !workflowUploads(workflow)) {
    warn(
      'This workflow takes the capture and stops there. Add an action or a destination, or a plain capture hotkey does the same job.'
    );
  }

  return issues.sort((a, b) => Number(b.level === 'error') - Number(a.level === 'error'));
}

/** Whether the draft may be written at all. */
export function workflowCanSave(issues: WorkflowIssue[]): boolean {
  return !issues.some((issue) => issue.level === 'error' && !issue.savable);
}

/**
 * Whether it may be switched on. Any error keeps it off, which is what makes an
 * unconfigured destination harmless: a disabled workflow has no hotkey
 * registered, so there is nothing to press and nothing to fail.
 */
export function workflowCanEnable(issues: WorkflowIssue[]): boolean {
  return !issues.some((issue) => issue.level === 'error');
}

/* ---------------------------------------------------- workflow hotkeys (M6 §3) */

/**
 * How a workflow's binding is named in the one hotkey registry it shares with
 * the built-ins. Spelled here and nowhere else.
 */
export const WORKFLOW_HOTKEY_PREFIX = 'workflow:' as const;

export function workflowHotkeyAction(id: string): HotkeyAction {
  return `${WORKFLOW_HOTKEY_PREFIX}${id}`;
}

/** The workflow id inside a `HotkeyStatus.action`, or `null` for a built-in. */
export function workflowIdOfHotkeyAction(action: string): string | null {
  return action.startsWith(WORKFLOW_HOTKEY_PREFIX)
    ? action.slice(WORKFLOW_HOTKEY_PREFIX.length)
    : null;
}

/* ------------------------------------------------------ workflow runs (M6 §2) */

/**
 * How each step of a run is named on screen.
 *
 * `WorkflowProgress.step` is a plain string and is rendered as it arrives when
 * it is not in here, so a runner that sends a finished label and one that sends
 * a tag both render — a chain that runs silently for eight seconds looks broken,
 * and an unrecognised step name is not a reason to go quiet.
 */
export const WORKFLOW_STEP_LABEL: Record<string, string> = {
  capture: 'Capture',
  region: 'Region',
  fullscreen: 'Fullscreen',
  activeWindow: 'Active window',
  monitor: 'Monitor',
  window: 'Window',
  scrolling: 'Scrolling capture',
  record: 'Recording',
  annotate: 'Annotate',
  saveToDisk: 'Save to disk',
  copyImage: 'Copy image',
  ocr: 'Read text',
  openFolder: 'Open folder',
  upload: 'Upload'
};

export function workflowStepLabel(step: string): string {
  return labelOf(WORKFLOW_STEP_LABEL, step);
}

/**
 * Payload of `workflow://progress`, emitted for **every** step (M6 §2).
 *
 * `index` is 0-based and `total` counts the capture, every action and the
 * upload, so `index + 1` of `total` is what a reader says out loud.
 */
export interface WorkflowProgress {
  /** `Workflow.id` — the only way to tell whose run this is. */
  id: string;
  step: string;
  index: number;
  total: number;
}

/**
 * Payload of `workflow://done`: the run reached the end, or was ended cleanly.
 *
 * `cancelled` is the flag that matters. A cancelled region selection ends the
 * workflow **cleanly, not as an error** (M6 §2), and so does backing out of the
 * editor — raising a red toast for something the user just asked for is a bug.
 * Read the flag; never match on the message.
 */
export interface WorkflowDone {
  id: string;
  cancelled: boolean;
  /** What the run produced, or `null` when it kept nothing. */
  record: CaptureRecord | null;
  /** One finished sentence for the user, populated on success too. */
  message: string;
}

/**
 * Payload of `workflow://error`: a step failed and the chain stopped there.
 *
 * It names the step, because "workflow failed" leaves the user to guess whether
 * their capture was taken, saved, or uploaded. The chain does **not** carry on
 * to the destination with a half-finished result.
 */
export interface WorkflowError {
  id: string;
  step: string;
  index: number;
  message: string;
}
