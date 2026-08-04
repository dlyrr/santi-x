# santi.sharex — M2.5: the region overlay grows up

Extends `ARCHITECTURE.md` (M1) and `ARCHITECTURE-M2.md` (M2), both still
authoritative. Three changes, all driven by using the thing:

1. **Instant open.** Pressing the hotkey must show the overlay immediately.
   Today it builds a webview and loads a page first.
2. **Window auto-detect.** Hovering a window highlights it; a click captures it.
   This is the ShareX behaviour the app is missing most.
3. **Annotate in place.** Draw on the selection before committing, instead of
   capturing first and opening a separate editor.

---

## 0. A bug this must not repeat

The M2 editor loaded its image with `new Image()` pointed at an asset-protocol
URL. That origin (`http://asset.localhost`) **taints every canvas it is drawn
into**, and the failure only surfaces later, at `toDataURL()`, as
*"Tainted canvases may not be exported"* — the editor could not save or copy at
all. Fixed by decoding through `fetch` → `blob` → `createImageBitmap`, which
carries no origin.

The overlay is about to start exporting from a canvas too, so it must load the
freeze frame the same way. **Never `new Image()` on an asset URL anywhere in
this project.**

---

## 1. Instant open — a pre-warmed overlay

The cost today is webview creation plus a SvelteKit page load on the critical
path, every single capture. Remove both: build the overlay window **once**, at
startup, hidden, and reuse it forever.

### Lifecycle

- **At startup**, after setup and off the main thread so launch is not delayed,
  build the `overlay` window exactly as M1 §3.4 describes but leave it hidden
  and unpositioned. It renders `?w=overlay` in an **idle** state: no freeze
  frame, nothing drawn, no key handlers armed.
- **On capture**, `start_region_capture`:
  1. hides the main window *only if it is actually visible* (the hotkey path
     usually runs with it already hidden — do not pay the settle delay for
     nothing),
  2. grabs the virtual desktop and `encode_png_fast`s it to `freeze.png`,
  3. enumerates window rects (§2) in the same instant, so they match the frame,
  4. emits **`overlay://arm`** with `ArmPayload`,
  5. waits for the overlay to call **`overlay_ready`**, then positions, sizes,
     shows and focuses it.

  Step 5 is a deliberate round-trip: showing before the bitmap has decoded
  flashes an empty black window across the whole desktop. The round-trip costs
  a decode (tens of ms), not a page load (hundreds).
- **On commit or cancel**, `hide()` the window and clear its state. Never
  destroy it — destroying is what put the page load back on the critical path.

```ts
type ArmPayload = {
  src: string          // absolute path to freeze.png; frontend converts + fetches
  width: number        // freeze image pixels
  height: number
  originX: number      // virtual-desktop origin, xcap space
  originY: number
  windows: WindowRect[]
  annotate: boolean    // Settings.annotate_in_overlay, resolved at arm time
}
```

New command: `overlay_ready()` → `()`. Idempotent; an arm that is superseded
before ready must not show a stale frame, so carry a monotonic `armId` in the
payload and have `overlay_ready(armId)` ignore anything but the current one.

**Guard:** a pre-warmed always-on-top borderless window that leaks visible is a
full-screen lockout. It must be created hidden, `skip_taskbar`, and every exit
path (commit, cancel, error, image decode failure) must hide it.

---

## 2. Window auto-detect

### Rust

`capture.rs` gains window geometry. `WindowInfo` (M1 §4) grows `x`, `y`, `z`,
or a parallel `WindowRect` is added — either is fine, but the TS mirror must
match:

```ts
type WindowRect = { id: number; title: string; appName: string
                    x: number; y: number; width: number; height: number; z: number }
```

Enumerated with `xcap::Window::all()`, in **xcap screen coordinates**, and
filtered:

- drop minimized windows, empty titles, and zero/absurd sizes
- **drop santi.sharex's own windows** — the overlay itself is always-on-top and would
  otherwise be the top hit under every cursor position
- sort topmost-first by `z`, because hit-testing takes the first match

These are captured *with* the freeze frame and shipped in `ArmPayload`. Do not
add a separate command the overlay calls after arming — the window list would
then describe a desktop that no longer matches the frozen pixels.

### Overlay behaviour

Convert to image pixels once, on arm: `x - originX`, `y - originY`.

- **Not dragging:** the topmost window containing the cursor is the *candidate*.
  Outline it in `--accent`, leave its interior undimmed, and show its size in
  the readout badge along with its title.
- **Click without dragging** (pointerup with movement under ~4px) commits the
  candidate's rect.
- **Dragging** overrides to freehand selection; the candidate highlight
  disappears the moment a drag starts.
- Hit-testing runs on pointermove against the pre-converted list — it is a
  linear scan of a small array, so no spatial index, but do not re-convert
  coordinates per frame.

---

## 3. Annotate in place

After a selection exists — by drag or by window click — the overlay enters an
**annotate** state instead of committing, when `Settings.annotate_in_overlay`
is true.

### New setting

```rust
pub annotate_in_overlay: bool,  // default true, #[serde(default)]
```

Surfaced in Settings › Capture as "Annotate before saving". When **false** the
overlay commits on release exactly as it does today — that path must survive
intact, because it is the fast one.

### Rendering

The overlay stops using an `<img>` backdrop and becomes a canvas:

- a full-viewport `<canvas>` renders the freeze frame **and** the annotation
  shapes via the M2 `render()` from `src/lib/editor/render.ts` — the same
  function the editor and its export use, so nothing can drift
- the dimming scrim and the selection hole stay DOM rects layered above it
  (M1's four-rect approach, which stays crisp at any DPI)
- chrome — toolbar, badges, magnifier, crosshairs — stays DOM

Reuse the M2 engine wholesale: `doc.svelte.ts`, `tools.ts`, `render.ts`,
`export.ts`. Do not fork it. The overlay is a second consumer of that engine,
which is exactly why M2 §4.3 made `render()` pure.

Set `doc.crop` to the selection rect and let the existing crop-aware export do
the cropping.

### Committing

Two paths, and picking the right one matters:

- **No shapes drawn** → call the existing `finish_region_capture(rect)`. Rust
  crops the already-decoded image natively. No base64, no IPC payload.
- **Shapes drawn** → `renderToPng(doc)` and call the new
  `finish_region_capture_annotated(png: string)`. A 4K annotated PNG is 15–25 MB
  of base64 through the IPC, so this path is only taken when it earns its cost.

`finish_region_capture_annotated` decodes and runs the result through M1's
`finalize(app, img, "region")` — same history record, same clipboard copy, same
`capture://new` emit. It is the annotated twin of `finish_region_capture`,
nothing more.

### Interaction

A compact floating toolbar appears anchored to the selection (flipping to stay
on-screen), carrying the M2 tool set that makes sense here — arrow, rect,
ellipse, line, pen, text, highlight, redact, step — plus colour and stroke. It
must not cover the selection: prefer below, then above, then inside-bottom.

Keys, extending the M1 overlay's existing map:

| Key | Action |
|---|---|
| `Enter` | commit |
| `Esc` | cancel the active draw → clear the tool → back to selecting → cancel the capture |
| `Ctrl+Z` / `Ctrl+Shift+Z` | undo / redo, via the M2 doc stack |
| tool shortcuts | as M2 §4.4 |
| drag a selection edge | resize the selection after it exists |

The Escape ladder is load-bearing. The overlay is borderless and always-on-top,
so a state with no way out is a hard lockout — every state must have an Escape
that eventually reaches cancel.

---

## 4. What must not regress

- M1 capture paths, history, tray, hotkeys, settings, themes.
- M2 editor — it keeps working as the post-capture editor, reached from History
  and from `open_editor_after`.
- `cargo check`, `pnpm check`, `pnpm build` stay clean.
- The non-annotated region path stays native-crop and base64-free.
