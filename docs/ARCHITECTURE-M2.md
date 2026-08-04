# santi.sharex — M2: the annotation editor

Extends `ARCHITECTURE.md`, which stays authoritative for everything in M1.
Same rules: names here are binding, tokens come from `app.css`, Svelte 5 runes
only, Tauri v2 only.

M2 adds a third window: a post-capture editor with arrows, shapes, text,
redaction, a step counter, and crop — the thing you reach for between taking a
screenshot and sending it.

---

## 1. The editor window

| Label    | URL                                   |
|----------|---------------------------------------|
| `editor` | `index.html?w=editor&id=<captureId>`  |

Same query-param reasoning as the overlay (see M1 §1). `src/routes/+page.svelte`
gains a third branch: `w=editor` renders `EditorRoot.svelte`, which reads the
`id` param and loads that capture.

Opened from three places:
- automatically after a capture, when `Settings.open_editor_after` is true
- the **Edit** action on a `CaptureCard` and in the `Lightbox`
- a new tray menu item is *not* added — the editor always edits a specific capture

One editor window at a time. Opening a second edit request focuses the existing
window and loads the new capture into it, discarding nothing silently: if the
current document is dirty, prompt inside the editor first.

Window: 1280×820 default, resizable, decorated, min 900×600, centered, its own
title `Edit — {capture name}`.

---

## 2. New settings

Added to `Settings` (M1 §3.1) — **both** the Rust struct and `src/lib/types.ts`:

```rust
pub open_editor_after: bool,     // default false
pub editor_default_color: String, // default "#f2555a"
pub editor_default_stroke: u32,   // default 4
```

Surfaced in `SettingsView` under Capture ("Open the editor after each capture")
and a new Editor section (default colour swatch + default stroke width).

A settings migration must not break existing `settings.json` files: missing
fields deserialize to their defaults via `#[serde(default)]` on each new field.
Applying `#[serde(default)]` to the whole struct is not enough on its own —
put it on the new fields.

---

## 3. New Tauri commands

| Command | Args | Returns |
|---|---|---|
| `open_editor` | `id: string` | `()` — creates/focuses the `editor` window |
| `get_capture` | `id: string` | `CaptureRecord` — one record by id |
| `get_capture_image` | `id: string` | `string` — absolute path to the full PNG |
| `save_edit` | `id: string, png: string, replace: bool` | `CaptureRecord` |
| `copy_edit` | `png: string` | `()` — writes the edited image to the clipboard |
| `close_editor` | — | `()` |

`png` is a **base64-encoded PNG** (no data-URI prefix). Rust decodes it with a
small hand-rolled base64 decoder or the `base64` crate — add `base64 = "0.22"`
to `Cargo.toml`, do not hand-roll.

`save_edit` with `replace: true` overwrites the original file in place and
updates the existing record's `size_bytes`/`width`/`height` and thumbnail.
With `replace: false` it creates a **new** `CaptureRecord` of
`kind: "edit"` (add that to the kind union in M1 §3.1 and to the History
filter), leaving the original untouched. Either way it regenerates the
thumbnail, persists, and emits `capture://new` (for a new record) or a new
`capture://updated` event carrying the changed `CaptureRecord`.

`capture://updated` must be added to `api.ts` (`onCaptureUpdated`) and handled
by the history store — replacing the record in place, not prepending.

---

## 4. Editor architecture (`src/lib/editor/`)

```
EditorRoot.svelte      window shell: toolbar, canvas stage, footer
stage/Stage.svelte     the canvas + pointer interaction layer
stage/render.ts        pure render(ctx, doc, opts) — draws a document to a 2D context
doc.svelte.ts          the document rune store: shapes, selection, history
tools.ts               tool definitions, cursors, hit-testing
export.ts              renders the doc at full resolution and returns base64 PNG
Toolbar.svelte         tool picker + per-tool options
ColorPicker.svelte     palette + custom colour
```

### 4.1 Document model

```ts
type ShapeId = string
type Point = { x: number; y: number }   // ALWAYS in image pixels, never CSS px

type ShapeBase = { id: ShapeId; kind: string; color: string; stroke: number }

type Arrow     = ShapeBase & { kind: 'arrow';     from: Point; to: Point }
type Line      = ShapeBase & { kind: 'line';      from: Point; to: Point }
type RectShape = ShapeBase & { kind: 'rect';      rect: Rect; fill: boolean }
type Ellipse   = ShapeBase & { kind: 'ellipse';   rect: Rect; fill: boolean }
type Pen       = ShapeBase & { kind: 'pen';       points: Point[] }
type Highlight = ShapeBase & { kind: 'highlight'; rect: Rect }
type TextShape = ShapeBase & { kind: 'text'; at: Point; text: string; size: number }
type Redact    = ShapeBase & { kind: 'redact'; rect: Rect; mode: 'blur' | 'pixelate'; amount: number }
type Step      = ShapeBase & { kind: 'step'; at: Point; n: number }

type Shape = Arrow | Line | RectShape | Ellipse | Pen | Highlight | TextShape | Redact | Step

type EditorDoc = {
  src: HTMLImageElement | ImageBitmap   // the loaded capture
  width: number; height: number          // image pixels
  crop: Rect | null                      // applied at export time, non-destructive
  shapes: Shape[]                        // painter's order, index 0 drawn first
  selected: ShapeId | null
}
```

**All geometry is stored in image pixels.** The stage scales for display (fit
to window, zoom 10–800%, pan with space-drag or middle-drag) and converts
pointer coordinates through a single `toImage(clientPoint)` helper. Nothing
else may do its own coordinate maths — this is the same discipline that keeps
the M1 overlay correct, and for the same reason.

### 4.2 Undo/redo

A plain command stack in `doc.svelte.ts`: `commit(label)` snapshots the shape
array (structuredClone) before each mutation, `undo()`/`redo()` walk it, cap 100
entries. Ctrl+Z / Ctrl+Shift+Z and Ctrl+Y. Live drags mutate a *draft* shape and
commit once on pointerup — dragging an arrow must produce one undo entry, not
one per pointermove.

### 4.3 Rendering

`render.ts` exports one pure function used by BOTH the on-screen stage and the
full-resolution export, so what you see is exactly what you get:

```ts
export function render(ctx: CanvasRenderingContext2D, doc: EditorDoc, opts: { scale: number }): void
```

Redaction is the one shape that reads pixels back: draw the source region into
an offscreen canvas, downscale-and-upscale for `pixelate` (with
`imageSmoothingEnabled = false`), or use `ctx.filter = 'blur(Npx)'` for `blur`,
then draw it over the region. Blur must sample **only** the source image, never
already-drawn annotations, or arrows smear when they overlap a redaction.

Redaction is applied at its position in painter's order, so a redaction added
after an arrow does cover that arrow. That is intentional and matches ShareX.

### 4.4 Tools and interaction

Toolbar, in order, with single-key shortcuts:

Keys match **ShareX's region-capture keymap** so the muscle memory transfers
([getsharex.com/docs/region-capture](https://getsharex.com/docs/region-capture)).

| Tool | Key | Notes |
|---|---|---|
| Select | `M` | move/resize existing shapes via handles, Delete removes |
| Arrow | `A` | Shift constrains to 15° increments |
| Rectangle | `R` | Shift = square. Toggle fill in tool options |
| Ellipse | `E` | Shift = circle |
| Line | `L` | Shift constrains |
| Pen | `F` | freehand, simplified on commit (drop points within 1.5px) |
| Text | `T` | click places a caret, types inline on canvas, Esc commits, empty text is discarded |
| Highlight | `H` | translucent marker, multiply blend |
| Redact | `B` / `P` | ShareX has two tools where santi.sharex has one with a mode, so **both keys select Redact** and set it: `B` → blur, `P` → pixelate |
| Step | `I` | auto-incrementing numbered badge, counter resets per document |
| Crop | `C` | drag a rect, Enter applies, Esc cancels. Non-destructive: sets `doc.crop` |

A key may therefore carry option overrides with it — `ToolDef.preset` /
`altPreset`, resolved by `toolHitForKey()`. That is the only reason a shortcut
touches anything beyond the active tool.

Shared options bar: colour palette (8 swatches + custom), stroke width 1–24,
text size for `T`, and per-tool extras. Changing an option with a shape
selected applies it to that shape.

Footer: zoom control, image dimensions, and the actions —
**Copy** (Ctrl+C when nothing is selected), **Save** (Ctrl+S, replaces),
**Save as new** (Ctrl+Shift+S), **Cancel** (Esc twice, or once when clean).

Escape hierarchy, in order: cancel the in-progress drag → cancel the active
tool back to Select → clear selection → prompt to close if dirty.

### 4.5 Export

`export.ts` builds an offscreen canvas of the **cropped** size at 1:1 image
resolution, runs the same `render()`, and returns
`canvas.toDataURL('image/png')` stripped of its prefix. Never export from the
on-screen canvas — it is scaled for display and would ship a blurry image.

---

## 5. What must not regress

- M1 capture paths, hotkeys, tray and history behaviour are untouched.
- `pnpm check` and `cargo check` stay clean.
- No new hard-coded colours: the editor chrome uses M1 tokens and must look
  right in both `dark` and `claude`. The annotation *palette* itself is
  content, not chrome, so those swatches are literal colours and are the one
  documented exception.
