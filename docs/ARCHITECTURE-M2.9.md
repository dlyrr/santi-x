# santi.sharex — M2.9: loupe zoom, live window highlight, capture preview

Extends M1, M2, M2.5, M2.6 and M2.7, all still authoritative. Three additions,
all from watching ShareX side by side.

---

## 1. Scroll to zoom the loupe

The region overlay's magnifier is fixed magnification. Make the wheel change it.

- Wheel over the overlay adjusts loupe zoom across **2×–20×**, in steps that
  feel right on both a notched wheel and a trackpad (normalise `deltaY`; do not
  assume 100px per notch).
- Clamp at both ends, and show the current factor in the loupe itself
  (`8×`), small and unobtrusive, so the control is discoverable.
- Persist the chosen factor for the session — a user who zooms to 12× once
  should still be at 12× on the next capture. `Settings.loupe_zoom: u32`
  (default 8, `#[serde(default = "…")]`) makes it survive a restart. Write it
  through the normal settings path; do not add a bespoke command.
- The wheel must be consumed (`preventDefault`) so the page cannot scroll —
  the overlay is a fixed, non-scrolling surface and a stray scroll would be a
  visible glitch.
- Wheel handling is only live when the loupe is (`showLoupe`), so it cannot
  fight a future zoom/pan gesture on a drawing tool.

---

## 2. The window highlight animates

ShareX's window auto-detect glides its highlight between windows instead of
snapping. santi.sharex snaps.

The candidate highlight (M2.5 §2) should **animate its position and size**
between one window and the next:

- Transition `left/top/width/height` over ~110ms with an ease-out curve. It
  must read as "the box moved", not as a fade-out/fade-in of two boxes.
- Fade in when the first candidate appears and out when the pointer leaves all
  windows, so appearing and disappearing are not instant either.
- **Honour `prefers-reduced-motion: reduce`** — no transition at all in that
  case. This is decoration, and decoration is exactly what that media query
  exists to switch off.
- The transition must never apply while a drag is in flight, and must not delay
  the *commit* rect: clicking a window captures its true bounds immediately,
  whatever the animation is mid-way through showing. The animation is presentation
  only; the geometry it is catching up to is already authoritative.

Same treatment for the toolbar buttons' hover state — a 120ms ease on
background/border, matching the app's existing motion budget (M1 §VISUAL: "no
entrance animations, 120–160ms on hover/active only").

---

## 3. The capture preview

After a capture, ShareX shows a small thumbnail in the corner. Add one.

### The window

| Label     | URL              |
|-----------|------------------|
| `preview` | `?w=preview`     |

Bare `?w=` form, same reasoning as M1 §1 and M2.5 — `index.html?…` 404s under
`tauri dev`.

- ~300×200 logical, borderless, `always_on_top`, `skip_taskbar`, `resizable(false)`,
  `shadow(false)`.
- **`focused(false)`, and never `set_focus()`.** This is the single most
  important property of this window: it appears while the user is doing
  something else, and stealing focus mid-keystroke would be far worse than the
  feature is useful. It must appear without disturbing anything.
- Positioned bottom-right of the monitor the capture came from, inset ~24px,
  and clear of the taskbar — use the monitor's **work area** where available,
  not its full bounds.
- Created on demand, then **hidden and reused**, exactly like the overlay
  (M2.5 §1). Never destroyed.

### Behaviour

- Shows the capture's thumbnail, its filename, and its dimensions.
- Auto-dismisses after ~4s. **Hovering cancels the countdown; leaving restarts
  it** — a preview that vanishes while you are reaching for it is worse than no
  preview.
- Clicking it opens the capture in the editor (`open_editor`). A small × closes
  it. Escape closes it when it happens to have focus.
- A second capture while one is showing **replaces** its content and restarts
  the timer rather than opening a second window or queueing.
- It renders the thumbnail via the asset protocol — and like everywhere else,
  **never `new Image()` on an asset URL**; the taint bug in M2 §0 is a
  standing hazard. An `<img>` tag is fine here since nothing is exported from a
  canvas, but use `versionedAssetUrl` so an edited capture does not show its
  stale thumbnail.

### Setting

```rust
pub show_capture_preview: bool,  // default true, #[serde(default = "…")]
```

Surfaced in Settings › Capture as "Show a preview after capture".

### The trap to avoid

An always-on-top, unfocusable, borderless window that fails to hide is a
permanent sticker on the user's screen that they cannot click away, because it
does not take focus and has no titlebar. Every path — timer, click, close
button, a failed thumbnail load, a second capture, an error — must end with it
hidden. Treat this with the same seriousness as the overlay lockout audit.

---

## 4. What must not regress

- M1 capture/history/settings/tray/themes; M2 editor; M2.5 arm-ready handshake,
  window auto-detect, native-crop fast path; M2.6 hotkey fallback and its scope,
  start-to-tray, four themes; M2.7 tool model and commit-on-release.
- The overlay Escape ladder, in every phase.
- `cargo check`, `cargo test`, `pnpm check`, `pnpm build` stay clean.
