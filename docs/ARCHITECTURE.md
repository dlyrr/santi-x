# santi.sharex — architecture contract (M1)

santi.sharex is a ShareX-style screen capture tool for Windows, rebuilt on
Tauri v2. This file is the **contract** every module is written against. If you
change a name here, change it everywhere.

Throughout these docs, **santi.sharex** is this app, **ShareX** is the original
C#/WinForms application, and `sharex` in code font is this app's ShareX-replica
theme id.

Stack (locked): Tauri v2 + SvelteKit (Svelte 5 runes, `adapter-static`, SPA
fallback) + TypeScript. Package manager: **pnpm** (the project has its own
`pnpm-workspace.yaml` — do not remove it, or pnpm walks up to the home dir).

Rust crates: `xcap` 0.9.7 (capture), `image` 0.25, `chrono` 0.4,
`tauri-plugin-global-shortcut`, `tauri-plugin-clipboard-manager`,
`tauri-plugin-dialog`, `tauri-plugin-opener`.

---

## 1. Windows

| Label     | URL                      | Notes |
|-----------|--------------------------|-------|
| `main`    | `index.html`             | The app shell. Closing it hides to tray instead of quitting. |
| `overlay` | `?w=overlay`             | Region-select overlay. Created on demand, destroyed after use. |

**Why a query param and not a route:** `adapter-static` with `fallback:
index.html` does not emit `/overlay.html`, so a `WebviewUrl::App("overlay")`
would 404 in a production bundle. `src/routes/+page.svelte` branches on
`location.search` instead.

**Why the bare query and not `index.html?w=overlay`:** Tauri joins the
`WebviewUrl::App` path onto the app base URL. In `tauri dev` that base is the
SvelteKit dev server, which has no `/index.html` route and answers it with a
404 page — a full-screen "Not found" pasted over the user's desktop. The bare
`?w=overlay` hits the root route in dev, and in a bundled app the `tauri://`
asset protocol strips the query and maps `/` to `index.html`. Both modes then
load the same document.

---

## 2. Coordinate spaces (read this before touching capture code)

There are two physical-pixel spaces and they are *usually* but not *always*
identical:

- **xcap space** — `Monitor::x()/y()` come from `DEVMODE.dmPosition`;
  `width()/height()` from `dmPelsWidth/dmPelsHeight`. True device pixels.
- **Tauri space** — `PhysicalPosition`/`PhysicalSize` on the overlay window.

On a uniform-DPI setup they coincide. On mixed-DPI multi-monitor they can
diverge. M1 accepts that divergence and stays correct anyway, because the
overlay never trusts the mapping:

> The overlay converts CSS px → image px using
> `scale = freeze.width / window.innerWidth`, measured at runtime from the
> image it was actually handed. Crop rects are therefore always expressed in
> **freeze-image pixel coordinates**, which is the only space `finish_region_capture`
> knows about.

Known M1 limitation, documented not fixed: on a mixed-DPI setup the overlay
window may not cover the virtual desktop exactly. Selection within the covered
area is still pixel-accurate.

---

## 3. Rust module layout (`src-tauri/src/`)

```
main.rs      unchanged scaffold entrypoint -> santi_sharex_lib::run()
lib.rs       run(): plugins, state, tray, hotkeys, invoke_handler, window events
store.rs     Settings + CaptureRecord types, JSON persistence, AppState
capture.rs   xcap wrappers: composite, crop, thumbnail, encode, save
overlay.rs   freeze-frame lifecycle + overlay window create/destroy
```

### 3.1 `store.rs`

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hotkeys {
    pub region: String,        // default "CmdOrCtrl+PrintScreen"
    pub fullscreen: String,    // default "PrintScreen"
    pub active_window: String, // default "Alt+PrintScreen"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub save_dir: String,          // default: <Pictures>/santi.sharex
    pub filename_pattern: String,  // default "{kind}_{yyyy}-{MM}-{dd}_{HH}-{mm}-{ss}"
    pub save_to_disk: bool,        // default true
    pub copy_to_clipboard: bool,   // default true
    pub open_folder_after: bool,   // default false
    pub hide_window_on_capture: bool, // default true
    pub theme: String,             // "dark" | "claude"  (default "dark")
    pub hotkeys: Hotkeys,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureRecord {
    pub id: String,          // millis + atomic counter, e.g. "1754006400000-3"
    pub name: String,        // file name incl. extension
    pub path: String,        // absolute path on disk ("" when save_to_disk = false)
    pub thumb: String,       // absolute path to the thumbnail PNG
    pub width: u32,
    pub height: u32,
    pub kind: String,        // "region" | "fullscreen" | "window" | "monitor"
    pub created_at: i64,     // Utc millis
    pub size_bytes: u64,
    pub saved: bool,
    pub copied: bool,
}
```

Persistence, all under `app_data_dir()`:
- `settings.json` — `Settings`
- `history.json`  — `Vec<CaptureRecord>`, newest first, capped at 500
- `thumbs/{id}.png` — thumbnails, longest edge 480px

`AppState { settings: Mutex<Settings>, history: Mutex<Vec<CaptureRecord>>, freeze: Mutex<Option<FreezeFrame>> }`,
registered with `app.manage(...)`. Every mutation writes its JSON file
synchronously before returning.

**Filename pattern tokens:** `{kind} {yyyy} {MM} {dd} {HH} {mm} {ss} {rand}`.
Unknown tokens are left verbatim. Always append `.png`. If the target file
exists, suffix ` (2)`, ` (3)`, … before the extension.

### 3.2 `capture.rs`

```rust
pub struct Shot { pub image: RgbaImage, pub origin_x: i32, pub origin_y: i32 }

pub fn capture_virtual_desktop() -> Result<Shot, String>;      // all monitors composited
pub fn capture_monitor_by_id(id: u32) -> Result<Shot, String>;
pub fn capture_focused_window() -> Result<Shot, String>;
pub fn capture_window_by_id(id: u32) -> Result<Shot, String>;
pub fn thumbnail(img: &RgbaImage) -> RgbaImage;                 // longest edge 480
pub fn encode_png_fast(img: &RgbaImage) -> Result<Vec<u8>, String>;
pub fn encode_png(img: &RgbaImage) -> Result<Vec<u8>, String>;
```

Compositing: bounds are the union of every monitor's `(x, y, width, height)`.
Place each captured monitor image at `(mx - min_x, my - min_y)` with
`image::imageops::replace`, which clips safely — use the *captured image's*
real dimensions, never the reported ones, when they disagree.

`encode_png_fast` uses `PngEncoder` with `CompressionType::Fast` and is used
for the freeze frame only (a 4K desktop must encode in well under a second).
`encode_png` is the default-compression one used for saved captures.

### 3.3 Capture pipeline

Every capture path funnels through one function in `lib.rs`:

```rust
fn finalize(app: &AppHandle, img: RgbaImage, kind: &str) -> Result<CaptureRecord, String>
```

which, in order: writes the PNG to `save_dir` (if `save_to_disk`), writes the
thumbnail, copies to the clipboard (if `copy_to_clipboard`, via
`ClipboardExt::write_image` with `tauri::image::Image::new(&rgba, w, h)`),
pushes the record onto the front of the history, persists, emits
`capture://new` with the `CaptureRecord` payload, and optionally reveals the
folder.

When `hide_window_on_capture` is set and the `main` window is visible, hide it
before grabbing and restore it after — with a ~120 ms settle delay so the
window is actually gone from the framebuffer.

### 3.4 `overlay.rs`

```rust
pub struct FreezeFrame { pub path: PathBuf, pub width: u32, pub height: u32,
                         pub origin_x: i32, pub origin_y: i32 }
```

`start_region_capture` → capture virtual desktop → `encode_png_fast` to
`app_cache_dir()/freeze.png` → store `FreezeFrame` in state → build the overlay
window (`decorations(false)`, `always_on_top(true)`, `skip_taskbar(true)`,
`resizable(false)`, `shadow(false)`, `visible(false)`), then
`set_position(PhysicalPosition::new(origin_x, origin_y))`,
`set_size(PhysicalSize::new(width, height))`, `show()`, `set_focus()`.

Build it invisible and show it only after positioning, otherwise it flashes at
the wrong spot.

---

## 4. Tauri commands (exact names — the frontend calls these)

| Command | Args | Returns |
|---|---|---|
| `get_settings` | — | `Settings` |
| `save_settings` | `settings: Settings` | `Settings` (re-registers hotkeys) |
| `get_history` | — | `CaptureRecord[]` |
| `delete_capture` | `id: string, deleteFile: bool` | `CaptureRecord[]` |
| `clear_history` | `deleteFiles: bool` | `CaptureRecord[]` |
| `copy_capture` | `id: string` | `()` — re-copies that image to the clipboard |
| `reveal_capture` | `id: string` | `()` — selects the file in Explorer |
| `open_capture` | `id: string` | `()` — opens in the default viewer |
| `open_save_dir` | — | `()` |
| `list_monitors` | — | `MonitorInfo[]` |
| `list_windows` | — | `WindowInfo[]` |
| `capture_fullscreen` | — | `CaptureRecord` |
| `capture_monitor` | `id: u32` | `CaptureRecord` |
| `capture_active_window` | — | `CaptureRecord` |
| `capture_window` | `id: u32` | `CaptureRecord` |
| `start_region_capture` | — | `()` |
| `get_freeze_frame` | — | `FreezeInfo` |
| `finish_region_capture` | `rect: Rect` | `CaptureRecord` |
| `cancel_region_capture` | — | `()` |

```ts
type MonitorInfo = { id: number; name: string; x: number; y: number;
                     width: number; height: number; isPrimary: boolean; scaleFactor: number }
type WindowInfo   = { id: number; title: string; appName: string;
                      width: number; height: number }
type FreezeInfo   = { src: string; width: number; height: number }  // src = convertFileSrc'd
type Rect         = { x: number; y: number; width: number; height: number } // freeze-image px
```

Events emitted on the app handle:
- `capture://new` → `CaptureRecord`
- `capture://error` → `string`
- `settings://changed` → `Settings`

---

## 5. Frontend layout (`src/`)

```
app.html                  pre-paint theme script (see §6)
app.css                   design tokens + @font-face + base styles
lib/api.ts                typed wrappers over every command above + event helpers
lib/stores/settings.svelte.ts   runes store, loads on boot, persists on change
lib/stores/history.svelte.ts    runes store, listens to capture://new
lib/components/           Sidebar, CaptureCard, Toast, Toggle, Field, Icon…
lib/views/CaptureView.svelte    the capture launcher grid
lib/views/HistoryView.svelte    the gallery
lib/views/SettingsView.svelte   settings incl. the theme picker
lib/overlay/RegionOverlay.svelte  the freeze-frame selector
routes/+layout.ts         `export const ssr = false; export const prerender = false;`
routes/+page.svelte       branches on ?w=overlay
```

Navigation inside `main` is a plain rune (`view: 'capture' | 'history' | 'settings'`),
not the SvelteKit router — the router is only used to separate the two windows.

---

## 6. Theming

Two selectable themes, stamped on `<html data-theme>`:

- `dark` — the default. Modern near-black UI, blue accent. Its own identity,
  deliberately *not* ShareX's gray WinForms look.
- `claude` — warm paper, clay accent, Anthropic Sans. Flat: no blur, no glass.

**The theme must be applied before first paint.** `app.html` carries an inline
script that reads `localStorage['santi-sharex.theme']` and sets
`document.documentElement.dataset.theme`. Applying it on mount flashes the
wrong palette. The Rust `Settings.theme` is the durable source of truth;
localStorage is the pre-paint cache and is rewritten whenever settings load or
change.

The key was `localStorage['nimbus.theme']` before the rename. The inline script
reads the new key, falls back to the old one when the new is absent, and writes
the value forward under the new key — otherwise the first launch after the
rename has no cache and paints the default palette for a frame, which is the
exact flash the script exists to prevent. Both names live in `src/lib/types.ts`
as `THEME_STORAGE_KEY` and `LEGACY_THEME_STORAGE_KEY`; nothing ever writes the
legacy one.

The fallback often misses, by construction rather than by mistake: Tauri forces
the WebView2 user-data directory to `<LocalData>/<identifier>`, so the rename
also handed the webview an empty profile and there is no supported way to reach
the old one's localStorage. The residual cost is one frame of `dark` on the
first launch — and, if the stored theme were `sharex`, one frame of the default
shell — before the settings round trip corrects both.

All theme values are CSS custom properties in `app.css`. Components read tokens
only — no hard-coded colours anywhere. Token set:

```
--bg  --bg-raised  --bg-inset  --surface  --border  --border-strong
--text  --text-dim  --text-faint
--accent  --accent-hover  --accent-text  --danger  --success
--radius  --radius-lg  --shadow  --blur
--font  --font-display  --font-mono
```

Plus one group the region overlay owns, defined in both blocks with *identical*
values because that window paints over the frozen desktop rather than inside the
app chrome and must not change with the palette:

```
--ov-scrim  --ov-line  --ov-line-ring  --ov-guide
--ov-panel  --ov-panel-border  --ov-panel-shadow  --ov-panel-inset
--ov-hairline  --ov-text  --ov-text-dim
```

Both windows load the same document, so the overlay's document-level rules
(`position: fixed`, transparent background) are gated on an `overlay-window`
class that the same pre-paint script in `app.html` stamps on `<html>` when
`?w=overlay` is present. Ungated they would strip the main window's background,
because component CSS ships in one bundle.

Both `html[data-theme="dark"]` and `html[data-theme="claude"]` blocks live at
the **end** of `app.css`, dark first, and every token is defined in both — a
token defined in only one theme is a bug.

Fonts ship in `static/fonts/`. Anthropic Sans is used by the `claude` theme
only (`--font`, `--font-display`); the `dark` theme uses a system stack.
