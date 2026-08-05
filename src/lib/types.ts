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
 * the whole list on every rebind, so it is always the three actions in order.
 */
export interface HotkeyStatus {
  /** The `Hotkeys` field this describes. */
  action: keyof Hotkeys;
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
 */
export type CaptureKind = 'region' | 'fullscreen' | 'window' | 'monitor' | 'edit' | 'scroll';

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

/** Top-level navigation inside the `main` window. */
export type View = 'capture' | 'history' | 'settings';

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
