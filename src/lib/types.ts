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
  hotkeys: Hotkeys;
}

/**
 * Which capture path produced a record. Mirrors `CaptureRecord.kind`.
 * `edit` comes from `saveEdit(..., replace: false)`, not from a capture.
 */
export type CaptureKind = 'region' | 'fullscreen' | 'window' | 'monitor' | 'edit';

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
