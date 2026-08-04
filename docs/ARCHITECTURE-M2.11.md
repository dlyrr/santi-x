# santi.sharex — M2.11: the smart eraser

Extends M1, M2, M2.5, M2.6, M2.7 and M2.9, all still authoritative. One new
annotation tool, available in both the region overlay and the editor.

---

## 1. What it is

The real ShareX describes its smart eraser as *"Cover small areas by blending
with nearby colors."* You drag a box over something — a name, a token, a
tooltip — and it is replaced by the background it was sitting on, rather than
by a blur or a black bar. On a solid background the result is invisible, which
is the whole point: it reads as though the content was never there.

ShareX's own implementation samples essentially one background colour, and its
documented weakness is that it "will appear to be smudgy" on a background with
multiple hues.

**We do slightly better, for free.** Instead of one sampled colour, fill each
pixel by interpolating between the pixels immediately *outside* the selection
on the same row and column. On a solid background every edge pixel is the same
colour, so the result is identical to ShareX's. On a gradient — a subtle window
background, a syntax-highlighted line — it follows the gradient instead of
flattening it. Same tool, strictly a superset.

This is not "content-aware fill". It cannot invent texture, and over a busy
photo it will look like a smear. That is expected and matches the tool's stated
purpose: small areas, nearby colours.

---

## 2. The shape

```ts
type Erase = ShapeBase & { kind: 'erase'; rect: Rect }
```

Added to the `Shape` union in `src/lib/editor/doc.svelte.ts`. It carries
`color` and `stroke` from `ShapeBase` because every shape does, but **neither is
used** — the fill comes entirely from the image. The toolbar must therefore
show no colour or stroke control for it.

## 3. Rendering

In `src/lib/editor/render.ts`, alongside the redaction cases:

- Sample the ring of pixels immediately outside the rect: for a pixel at
  `(x, y)` inside, take the pixels at `(rect.left - 1, y)`, `(rect.right, y)`,
  `(x, rect.top - 1)` and `(x, rect.bottom)`.
- Blend the four by inverse distance to their respective edges, so a pixel near
  the left edge is dominated by the left sample. Horizontal and vertical pairs
  are each interpolated, then the two results averaged.
- Clamp sampling at the image bounds. A selection flush against an edge simply
  has fewer contributors — it must not read out of bounds or fall back to
  transparent black, which would paint a hard dark rectangle exactly where the
  user wanted something invisible.
- A selection where *every* edge is off-image (the whole picture selected) has
  nothing to sample; fill with the image's mean colour rather than leaving the
  shape unrendered.

**It samples `doc.src` and nothing else** — the same rule redaction follows, and
for the same reason. Sampling the composited canvas would pull already-drawn
arrows into the fill, so an arrow crossing an erased box would smear into it.

Like redaction it composites at its position in painter's order, so an erase
drawn after an arrow does cover that arrow.

## 4. The tool

`src/lib/editor/tools.ts`:

```
id: 'erase'   label: 'Smart eraser'   key: 'x'   creates: 'erase'
options: []   shift: 'Square'
```

**On the key:** ShareX does not bind one — its own issue tracker carries open
requests for exactly that. `X` is ShareX's Cut Out key, which santi.sharex does
not implement, so it is free here and sits naturally beside the other
content-removal tools. Note in the M2 keymap table that this one is ours, not
ShareX's, so nobody later "corrects" it.

Available in **both** surfaces: the editor toolbar and the overlay's top bar
(`OVERLAY_TOOLS`). It needs an `Icon.svelte` glyph — add one, do not repurpose
an existing path.

Hit-testing and `shapeBounds` in `tools.ts` treat it exactly like a rect shape,
so Select can move and resize it.

---

## 5. What must not regress

- Everything through M2.9, and the rename. In particular the overlay Escape
  ladder, commit-on-release for the Region tool, and the native-crop fast path
  when no shapes were drawn — an erase **is** a shape, so a selection carrying
  one correctly takes the annotated path.
- `render()` stays the single renderer shared by the overlay, the editor stage
  and `export.ts`. The erase must look identical in all three.
- `cargo check`, `cargo test`, `pnpm check`, `pnpm check:tokens`, `pnpm build`.
