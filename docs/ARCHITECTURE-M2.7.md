# santi.sharex — M2.7: capture like ShareX actually captures

Extends M1, M2, M2.5 and M2.6, all still authoritative. Four changes, all from
the user comparing santi.sharex side by side with ShareX 21.0.

---

## 1. Clipboard copy is failing (bug)

The user reports copying to the clipboard errors. The Rust side looks correct —
`copy_rgba_to_clipboard` in `lib.rs` builds `tauri::image::Image::new(rgba, w, h)`
and calls `ClipboardExt::write_image`, which reaches
`arboard::Clipboard::set_image` through the plugin. The exact error text is not
yet known.

Two things to do, in this order:

**Surface the real error.** The message is already interpolated
(`clipboard write failed: {e}`) but it is emitted on `capture://error` and shown
as a toast that may be truncated or dismissed. Make the underlying error
reachable: log it to stderr as well, so it lands in the `tauri dev` console
where it can actually be read.

**Add a native Win32 fallback.** The plugin holds ONE `arboard::Clipboard`
created at plugin init and reused for the process lifetime, behind a mutex.
That is a single point of failure: on Windows the clipboard is a shared,
contended, openable-by-one-process-at-a-time resource, and a stale handle or a
losing race with another app surfaces as a hard error rather than a retry.

So: on failure, retry through a direct Win32 path —
`OpenClipboard` / `EmptyClipboard` / `SetClipboardData` / `CloseClipboard`,
writing **`CF_DIBV5`** (32-bit BGRA, top-down, with a proper `BITMAPV5HEADER`
carrying the alpha mask) and, when practical, also registering and writing the
`"PNG"` clipboard format, which is what most modern apps prefer and what
preserves transparency losslessly. Windows expects BGRA, so the channel swap
from our RGBA is required — getting this backwards produces an image with red
and blue swapped, which is a silent, plausible-looking wrong answer, so it is
worth a unit test on a known pixel.

Retry `OpenClipboard` a few times with a short backoff before giving up; losing
the clipboard for a few milliseconds is normal on Windows.

This lives in a new `src-tauri/src/clipboard.rs`. `copy_rgba_to_clipboard`
becomes: try the plugin, and on `Err` try the native path, reporting only if
both fail.

---

## 2. Release the drag, get the capture

M2.5 made a completed region selection enter an `annotating` phase. The user
wants ShareX's behaviour back: **release the drag and the capture is taken**,
immediately, no confirmation step.

The `annotate_in_overlay` setting is now obsolete as a *gate* and is removed
from the flow (leave the field in `Settings` deserializing harmlessly for
compatibility, but stop reading it to decide the commit). The new model is
§3 below: the active tool decides what a drag does.

The commit-path split from M2.5 is unchanged and still matters: a capture with
no shapes drawn takes the native `finish_region_capture(rect)` with no base64;
only a capture that actually has annotations pays for
`finish_region_capture_annotated(png)`.

---

## 3. Tools draw anywhere — no region required

Today a tool only makes sense after a region exists. The user wants what ShareX
does: pick Arrow, drag an arrow anywhere on the frozen desktop, drag another,
and only *then* decide the region — or never, and capture the lot.

**The active tool decides what a drag does.** That is the whole model:

| Active tool | Drag does | On release |
|---|---|---|
| **Region** (default) | draws the selection rect | **commits immediately** |
| Arrow / Rect / Ellipse / Line / Pen / Highlight / Redact | draws that shape, anywhere on the frozen image | nothing — stays open for more |
| Text | places a caret, types inline | commits the shape, not the capture |
| Step | places the next numbered badge | nothing |

- The overlay opens with **Region** active, so the muscle memory of "drag and
  it's captured" is the default path and costs no extra keystroke.
- Selecting any drawing tool takes the overlay out of committing-on-release.
  Switching back to Region (`V`, or clicking it) restores it.
- Shapes are **not** clipped to any selection while drawing — they live on the
  full frozen desktop in image pixels, exactly like the editor's document.
- With shapes drawn and no region: **Enter** captures the whole frozen desktop
  including them. The toolbar's Capture button does the same.
- With shapes drawn and then a Region drag: release commits, cropped to the
  region, with any shapes that intersect it composited in — the existing
  `shapesInside(rect)` test already expresses this and must keep working.
- Window auto-detect (M2.5 §2) stays live only while **Region** is the active
  tool. Hovering must not highlight windows while the user is drawing an arrow.

Undo/redo, the Escape ladder, and the M2.5 arm/ready lifecycle are all
unchanged. Escape's first rung now also covers "cancel the shape being drawn".

---

## 4. The `sharex` theme gets ShareX's layout

M2.6 gave `sharex` ShareX's palette and font. The user's screenshot shows the
rest of it: this is a *layout*, not just colours.

When `theme === "sharex"`, the **main window shell** adopts ShareX's
arrangement. Nothing else — the overlay, the editor, and the other three themes
keep their own layout.

From the reference:

- A **narrow left menu column** (~200px) on `--bg-raised`, separated from the
  content by a single hairline, holding dense 24px rows of `icon + label`, each
  row's icon a small coloured glyph. Rows are grouped with thin separators and
  generous gaps between groups, not boxes.
- The **content pane** is plain `--bg` with no card, no rounded panel, no
  shadow — ShareX puts content directly on the background.
- The default content is a **hotkey table**: a bordered grid with a header row
  (`Hotkey` | `Description`), centred cells, 1px `--border` gridlines, and a
  **coloured status bar down the left edge of each row** — green where the
  hotkey is bound, red where it is not. That maps directly onto the
  `HotkeyStatus` data M2.6 already exposes (`"plugin"`/`"hook"` → green,
  `"none"` → red), so this is a real status display rather than decoration.
- Menu rows map onto santi.sharex's actual features — Capture, Upload *(disabled,
  M3)*, Workflows *(disabled, M5)*, Tools, then After capture tasks,
  Destinations *(disabled)*, then Application settings, Hotkey settings, then
  Screenshots folder, History. **Do not fake working menus**: anything santi.sharex
  cannot do yet is visibly disabled with a tooltip saying which milestone it
  lands in. A homage that pretends to have features it lacks is a worse
  homage.

Implementation: a `ShareXShell.svelte` chosen by the theme in the app shell,
reusing the same view components for the actual content where possible. Keep it
token-driven — it must still be the `sharex` tokens doing the colouring, not
hard-coded greys.

---

## 5. What must not regress

- M1 capture/history/settings/tray; M2 editor (including the canvas-taint fix);
  M2.5 arm/ready handshake, window auto-detect, native-crop fast path;
  M2.6 hotkey fallback and its scope constraints, start-to-tray, four themes.
- The overlay Escape ladder: every phase still has a path out. The tool model
  in §3 adds states, and each one needs its rung.
- `cargo check`, `pnpm check`, `pnpm build` stay clean.
