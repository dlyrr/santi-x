# santi.sharex — M5: OCR, scrolling capture, and fixing the eraser

Extends everything through M2.11. Three pieces: one bug fix that needs a better
algorithm, one regrouping, and the two big remaining ShareX features.

---

## 1. The smart eraser streaks — and why

**The report:** erasing over anything that is not flat produces vertical
coloured streaks across the whole box.

**The cause is the algorithm, not a coding slip.** M2.11 §3 specified a four-tap
fill: for each interior pixel, sample left/right on its row and top/bottom on
its column, weight by inverse distance to each edge, average the two axes. On a
box that is wider than it is tall — which is most of them, since you are usually
erasing a line of text — the vertical samples are *much* closer than the
horizontal ones. Inverse-distance weighting therefore hands the column pair
nearly all the weight, and every column is filled with a vertical blend of
whatever sat directly above and below it. Varied content above and below
becomes exactly the vertical banding in the report.

It is doing what it was told. What it was told was too naive.

### The fix: solve for a smooth surface instead of interpolating

Replace the four-tap fill with a **Laplace diffusion solve** — the standard way
to fill a hole so that its interior is as smooth as possible while exactly
matching its border. Treat the border ring as fixed boundary values and relax
the interior until each pixel is the average of its four neighbours. The result
has no preferred axis, so it cannot band, and it reproduces a linear gradient
exactly.

Doing that at full resolution would be slow, and it does not need to be:

1. Downscale the region **plus its one-pixel border ring** to a grid whose
   longest edge is at most 64px, per channel.
2. Run Jacobi/Gauss-Seidel relaxation on the interior with the ring pinned —
   a few hundred iterations at that size is sub-millisecond.
3. Bilinearly upscale the solved interior back to the region's real size and
   blit it.

The upscale is not a compromise: the solution of Laplace's equation is smooth by
construction, so it carries no high-frequency detail that the downscale could
have destroyed. Iterate to a fixed count rather than to convergence, so the cost
is bounded and identical every frame.

Keep everything M2.11 §3 already got right: it samples `doc.src` only, it clamps
at image bounds, a fully-off-image selection falls back to the mean colour, it
composites in painter's order, and the patch is cached.

**This still is not content-aware fill.** Over a photo it produces a smooth
smudge rather than invented texture — but a smooth smudge is what "blend with
nearby colours" means, and it is a categorical improvement on banding.

---

## 2. The eraser becomes a redaction mode

It currently sits as its own tool. It belongs with blur and pixelate: all three
answer "make this unreadable", differing only in how.

```ts
type RedactMode = 'blur' | 'pixelate' | 'erase'
```

- The standalone `erase` tool is **removed** from `TOOLS`. The `Erase` shape
  kind goes with it; an erase becomes a `Redact` shape with `mode: 'erase'`.
- The Redact tool's mode control becomes three segments: Blur, Pixelate,
  Smart eraser.
- `redactAmount` is meaningless for `erase` — hide the amount slider when that
  mode is active, the same way the whole options group is hidden for a tool with
  none. Do not leave a dead slider on the bar.
- Shortcuts follow the existing preset mechanism (M2.11 §4, M2 §4.4): `B`
  selects Redact in blur, `P` in pixelate, and **`X` now selects Redact in erase
  mode** rather than a separate tool. Same keys, one fewer tool.
- Migration: nothing persisted stores shapes — annotations live only for the
  life of an editor session — so no on-disk migration is needed. Confirm that is
  actually true before relying on it.

---

## 3. OCR

Extract the text from a capture. Windows ships an offline OCR engine
(`Windows.Media.Ocr`), so this needs no service, no key and no network.

### Rust

New `src-tauri/src/ocr.rs`. The `windows` crate is already a dependency; add the
WinRT features it needs (`Media_Ocr`, `Graphics_Imaging`, `Storage_Streams`,
`Foundation_Collections`).

```rust
pub fn recognise(img: &RgbaImage) -> Result<OcrResult, String>
```

- Build a `SoftwareBitmap` in `Bgra8` from the `RgbaImage` (channel swap
  required, same as the clipboard path in M2.7 §1 — and worth a unit test for
  the same reason: a swapped image still "works", it just reads worse, which is
  a silent failure).
- `OcrEngine::TryCreateFromUserProfileLanguages()`, falling back to
  `TryCreateFromLanguage("en-US")`. If neither yields an engine — a machine with
  no OCR language pack installed — return a clear error saying exactly that and
  naming the Windows setting, not a bare "OCR failed".
- Return the full text plus per-line text, so the UI can offer both.

```ts
type OcrLine = { text: string }
type OcrResult = { text: string; lines: OcrLine[]; language: string }
```

Command: `ocr_capture(id: string) -> OcrResult`, reading the capture from disk.
Error clearly when the record has no file (`saveToDisk` off).

### UI

An **Extract text** action in the `Lightbox` and on `CaptureCard`, plus a toolbar
button in the editor. It opens a panel showing the recognised text with **Copy
text** and a per-line list. Empty result is a real state — "No text found" —
not an empty box.

Long OCR runs must not freeze the UI: the command is `async`/`spawn_blocking`
like every other heavy path here.

---

## 4. Scrolling capture

The hardest thing in this milestone, and the one most likely to be imperfect.
Be honest about that in the UI rather than pretending.

### How it works

1. The user picks a window (reuse the M2.5 window list).
2. Capture the window. Send it a scroll step (`WM_MOUSEWHEEL` to the window
   under the point, or `SendInput` wheel with the cursor parked over it).
3. Wait a settle delay, capture again.
4. **Stitch by finding the actual overlap**, not by assuming the scroll delta.
   Applications scroll by wildly different amounts — smooth scrolling, momentum,
   snapping to lines — so a fixed offset guarantees seams and duplicated rows.
   For each candidate offset, score how well the bottom strip of frame *n*
   matches the top strip of frame *n+1* (sum of absolute differences over a
   subsampled grid is enough), and take the best-scoring offset.
5. Stop when the best score for a new frame indicates no movement (the content
   stopped scrolling — the end of the page), or a frame budget is hit.

### Settings

```rust
pub scroll_delay_ms: u32,   // default 250, settle time between steps
pub scroll_step: i32,       // default 3, wheel notches per step
pub scroll_max_frames: u32, // default 60, hard stop
```

### Honesty requirements

- Show progress while it runs — frames captured so far — with a **Cancel** that
  really stops it. A silent multi-second freeze is unacceptable.
- If stitching finds no confident overlap between two frames, stop and keep what
  was stitched so far rather than concatenating blindly and producing a garbled
  image. Tell the user it stopped early and why.
- Document in the README that this works well on ordinary scrolling content and
  poorly on virtualised lists, parallax, and anything with fixed headers that
  repeat in every frame.

---

## 5. What must not regress

- Everything through M2.11 and the rename: overlay Escape ladder, arm/ready
  handshake, commit-on-release, the native-crop fast path, the keyboard hook's
  scope, cursor capture, the capture preview.
- `render()` stays the one renderer shared by overlay, stage and export.
- `cargo check`, `cargo test`, `pnpm check`, `pnpm check:tokens`, `pnpm build`.
