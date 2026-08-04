/**
 * The one renderer — M2 §4.3. The on-screen stage and the full-resolution
 * export both call `render()`, so what you see is what you get.
 *
 * Selection chrome (marching ants, resize handles) is deliberately NOT drawn
 * here: it is stage-only and must never reach an export.
 *
 * The helpers exported alongside `render` exist so hit-testing and the stage's
 * text caret measure exactly what the renderer draws. They are the geometry
 * primitives for the whole editor — `tools.ts` imports them rather than
 * re-deriving them, which is what keeps a click on a glyph and the glyph
 * itself in the same place.
 */
import type {
  Arrow,
  EditorDoc,
  Ellipse,
  Highlight,
  Line,
  Pen,
  Rect,
  RectShape,
  Redact,
  Shape,
  Step,
  TextShape
} from './doc.svelte';

export interface RenderOptions {
  /** Image px -> canvas units. The export always passes 1. */
  scale: number;
  /**
   * Honour `doc.crop`. Default true. The crop tool passes `false` so the area
   * about to be discarded stays visible while it is being dragged.
   */
  crop?: boolean;
}

/**
 * Annotation type is content, not chrome: it must not follow the app theme,
 * and it must resolve identically on the stage and in the export.
 */
export const ANNOTATION_FONT_FAMILY = "'Segoe UI', system-ui, -apple-system, sans-serif";
export const TEXT_LINE_HEIGHT = 1.28;

const HIGHLIGHT_ALPHA = 0.42;

const clamp = (v: number, lo: number, hi: number) => (v < lo ? lo : v > hi ? hi : v);

/** Positive-width, positive-height form of a rect that may have been dragged up/left. */
export function normalizeRect(rect: Rect): Rect {
  const x = Math.min(rect.x, rect.x + rect.width);
  const y = Math.min(rect.y, rect.y + rect.height);
  return { x, y, width: Math.abs(rect.width), height: Math.abs(rect.height) };
}

/**
 * The exported area, in image px, snapped to whole pixels — the export canvas
 * is an integer size, and a fractional origin would shift everything by half a
 * pixel and soften the whole image.
 */
export function cropRect(doc: EditorDoc): Rect {
  const full: Rect = { x: 0, y: 0, width: doc.width, height: doc.height };
  if (!doc.crop) return full;
  const c = normalizeRect(doc.crop);
  const x0 = clamp(Math.round(c.x), 0, doc.width);
  const y0 = clamp(Math.round(c.y), 0, doc.height);
  const x1 = clamp(Math.round(c.x + c.width), x0, doc.width);
  const y1 = clamp(Math.round(c.y + c.height), y0, doc.height);
  if (x1 - x0 < 1 || y1 - y0 < 1) return full;
  return { x: x0, y: y0, width: x1 - x0, height: y1 - y0 };
}

export function textFont(size: number): string {
  return `600 ${Math.max(1, Math.round(size))}px ${ANNOTATION_FONT_FAMILY}`;
}

export function textLines(shape: TextShape): string[] {
  return shape.text.split('\n');
}

/** Badge radius, grown with the digit count so "12" is not cramped against the rim. */
export function stepRadius(shape: Step): number {
  const digits = String(Math.max(1, Math.round(shape.n))).length;
  return Math.max(10, shape.stroke * 3) * (1 + 0.26 * (digits - 1));
}

let measureCtx: CanvasRenderingContext2D | null | undefined;

function measurer(): CanvasRenderingContext2D | null {
  if (measureCtx !== undefined) return measureCtx;
  measureCtx =
    typeof document === 'undefined' ? null : document.createElement('canvas').getContext('2d');
  return measureCtx;
}

/**
 * Bounding box of a text shape in image px. `at` is the block's top-left, so
 * this is the box the renderer fills and the box the Select tool must hit.
 */
export function textBounds(shape: TextShape): Rect {
  const lines = textLines(shape);
  const lineHeight = shape.size * TEXT_LINE_HEIGHT;
  const ctx = measurer();
  let width = 0;
  if (ctx) {
    ctx.font = textFont(shape.size);
    for (const line of lines) width = Math.max(width, ctx.measureText(line).width);
  } else {
    // No DOM to measure with (SSR, worker): a rough advance width still gives
    // hit-testing something sane to work with.
    for (const line of lines) width = Math.max(width, line.length * shape.size * 0.58);
  }
  return {
    x: shape.at.x,
    y: shape.at.y,
    width: Math.max(width, shape.size * 0.4),
    height: Math.max(lines.length, 1) * lineHeight
  };
}

/* -------------------------------------------------------------------- draw */

export function render(ctx: CanvasRenderingContext2D, doc: EditorDoc, opts: RenderOptions): void {
  const area =
    opts.crop === false ? { x: 0, y: 0, width: doc.width, height: doc.height } : cropRect(doc);
  const scale = opts.scale > 0 ? opts.scale : 1;

  ctx.save();
  // Relative, not `setTransform`: the caller may already have scaled for
  // devicePixelRatio or translated for panning, and that must survive.
  ctx.scale(scale, scale);
  ctx.translate(-area.x, -area.y);
  ctx.beginPath();
  ctx.rect(area.x, area.y, area.width, area.height);
  ctx.clip();

  drawSource(ctx, doc);
  for (const shape of doc.shapes) {
    ctx.save();
    drawShape(ctx, doc, shape);
    ctx.restore();
  }

  ctx.restore();
}

function drawSource(ctx: CanvasRenderingContext2D, doc: EditorDoc): void {
  // Nothing loaded: the store hands out a blank stand-in for `src` so it can
  // satisfy `EditorDoc`, and that must never reach drawImage().
  if (doc.width < 1 || doc.height < 1) return;
  const src = doc.src;
  // Drawing an image that has not decoded yet throws; the stage can call
  // render() on its first frame before onload has fired.
  if (typeof HTMLImageElement !== 'undefined' && src instanceof HTMLImageElement) {
    if (!src.complete || src.naturalWidth === 0) return;
  }
  ctx.drawImage(src, 0, 0, doc.width, doc.height);
}

function drawShape(ctx: CanvasRenderingContext2D, doc: EditorDoc, shape: Shape): void {
  switch (shape.kind) {
    case 'arrow':
      drawArrow(ctx, shape);
      break;
    case 'line':
      drawLine(ctx, shape);
      break;
    case 'rect':
      drawRect(ctx, shape);
      break;
    case 'ellipse':
      drawEllipse(ctx, shape);
      break;
    case 'pen':
      drawPen(ctx, shape);
      break;
    case 'highlight':
      drawHighlight(ctx, shape);
      break;
    case 'text':
      drawText(ctx, shape);
      break;
    case 'redact':
      drawRedact(ctx, doc, shape);
      break;
    case 'step':
      drawStep(ctx, shape);
      break;
  }
}

function strokeWidth(shape: { stroke: number }): number {
  return Math.max(1, shape.stroke);
}

function drawArrow(ctx: CanvasRenderingContext2D, shape: Arrow): void {
  const dx = shape.to.x - shape.from.x;
  const dy = shape.to.y - shape.from.y;
  const len = Math.hypot(dx, dy);
  if (len < 0.5) return;

  const w = strokeWidth(shape);
  const ux = dx / len;
  const uy = dy / len;
  // A real filled head, scaled to the stroke: a two-line V reads as a scribble
  // at high stroke widths.
  const head = Math.min(Math.max(w * 3.4, w + 6), len);
  const halfBase = head * 0.42;
  // Stop the shaft short of the head or a thick stroke pokes through the tip.
  const shaft = Math.max(0, len - head * 0.85);

  ctx.strokeStyle = shape.color;
  ctx.fillStyle = shape.color;
  ctx.lineWidth = w;
  ctx.lineCap = 'round';
  ctx.lineJoin = 'round';

  if (shaft > 0.5) {
    ctx.beginPath();
    ctx.moveTo(shape.from.x, shape.from.y);
    ctx.lineTo(shape.from.x + ux * shaft, shape.from.y + uy * shaft);
    ctx.stroke();
  }

  const baseX = shape.to.x - ux * head;
  const baseY = shape.to.y - uy * head;
  const px = -uy;
  const py = ux;
  ctx.beginPath();
  ctx.moveTo(shape.to.x, shape.to.y);
  ctx.lineTo(baseX + px * halfBase, baseY + py * halfBase);
  ctx.lineTo(baseX - px * halfBase, baseY - py * halfBase);
  ctx.closePath();
  ctx.fill();
}

function drawLine(ctx: CanvasRenderingContext2D, shape: Line): void {
  ctx.strokeStyle = shape.color;
  ctx.lineWidth = strokeWidth(shape);
  ctx.lineCap = 'round';
  ctx.beginPath();
  ctx.moveTo(shape.from.x, shape.from.y);
  ctx.lineTo(shape.to.x, shape.to.y);
  ctx.stroke();
}

function drawRect(ctx: CanvasRenderingContext2D, shape: RectShape): void {
  const r = normalizeRect(shape.rect);
  if (r.width < 0.5 || r.height < 0.5) return;
  if (shape.fill) {
    ctx.fillStyle = shape.color;
    ctx.fillRect(r.x, r.y, r.width, r.height);
    return;
  }
  ctx.strokeStyle = shape.color;
  ctx.lineWidth = strokeWidth(shape);
  ctx.lineJoin = 'miter';
  ctx.strokeRect(r.x, r.y, r.width, r.height);
}

function drawEllipse(ctx: CanvasRenderingContext2D, shape: Ellipse): void {
  const r = normalizeRect(shape.rect);
  if (r.width < 0.5 || r.height < 0.5) return;
  ctx.beginPath();
  ctx.ellipse(r.x + r.width / 2, r.y + r.height / 2, r.width / 2, r.height / 2, 0, 0, Math.PI * 2);
  if (shape.fill) {
    ctx.fillStyle = shape.color;
    ctx.fill();
    return;
  }
  ctx.strokeStyle = shape.color;
  ctx.lineWidth = strokeWidth(shape);
  ctx.stroke();
}

function drawPen(ctx: CanvasRenderingContext2D, shape: Pen): void {
  const pts = shape.points;
  if (pts.length === 0) return;

  ctx.strokeStyle = shape.color;
  ctx.lineWidth = strokeWidth(shape);
  ctx.lineCap = 'round';
  ctx.lineJoin = 'round';

  if (pts.length === 1) {
    // A tap still leaves a dot, otherwise the stroke silently vanishes.
    ctx.fillStyle = shape.color;
    ctx.beginPath();
    ctx.arc(pts[0].x, pts[0].y, ctx.lineWidth / 2, 0, Math.PI * 2);
    ctx.fill();
    return;
  }

  ctx.beginPath();
  ctx.moveTo(pts[0].x, pts[0].y);
  // Quadratics through the midpoints: the sampled points become control
  // points, which smooths the corners a raw polyline shows at high zoom.
  for (let i = 1; i < pts.length - 1; i++) {
    const mx = (pts[i].x + pts[i + 1].x) / 2;
    const my = (pts[i].y + pts[i + 1].y) / 2;
    ctx.quadraticCurveTo(pts[i].x, pts[i].y, mx, my);
  }
  const last = pts[pts.length - 1];
  ctx.lineTo(last.x, last.y);
  ctx.stroke();
}

function drawHighlight(ctx: CanvasRenderingContext2D, shape: Highlight): void {
  const r = normalizeRect(shape.rect);
  if (r.width < 0.5 || r.height < 0.5) return;
  // Multiply keeps the text underneath readable — a plain translucent fill
  // washes dark glyphs out towards the marker colour.
  ctx.globalCompositeOperation = 'multiply';
  ctx.globalAlpha = HIGHLIGHT_ALPHA;
  ctx.fillStyle = shape.color;
  ctx.fillRect(r.x, r.y, r.width, r.height);
}

function drawText(ctx: CanvasRenderingContext2D, shape: TextShape): void {
  const lines = textLines(shape);
  if (lines.every((l) => l.length === 0)) return;

  const lineHeight = shape.size * TEXT_LINE_HEIGHT;
  ctx.font = textFont(shape.size);
  ctx.textAlign = 'left';
  ctx.textBaseline = 'top';
  ctx.lineJoin = 'round';
  ctx.miterLimit = 2;
  ctx.lineWidth = Math.max(2, shape.size * 0.17);
  ctx.strokeStyle = haloColor(shape.color);

  // Halo first for every line, then every fill: per-line interleaving would
  // let one line's halo eat into the line above it.
  for (let i = 0; i < lines.length; i++) {
    if (lines[i]) ctx.strokeText(lines[i], shape.at.x, shape.at.y + i * lineHeight);
  }
  ctx.fillStyle = shape.color;
  for (let i = 0; i < lines.length; i++) {
    if (lines[i]) ctx.fillText(lines[i], shape.at.x, shape.at.y + i * lineHeight);
  }
}

function drawStep(ctx: CanvasRenderingContext2D, shape: Step): void {
  const r = stepRadius(shape);
  const label = String(Math.max(1, Math.round(shape.n)));

  ctx.beginPath();
  ctx.arc(shape.at.x, shape.at.y, r, 0, Math.PI * 2);
  ctx.fillStyle = shape.color;
  ctx.fill();

  ctx.font = `700 ${Math.round(r * 1.12)}px ${ANNOTATION_FONT_FAMILY}`;
  ctx.textAlign = 'center';
  ctx.textBaseline = 'middle';
  ctx.fillStyle = inkColor(shape.color);
  // `middle` sits a shade high for digits; nudge back onto the optical centre.
  ctx.fillText(label, shape.at.x, shape.at.y + r * 0.05);
}

/**
 * The only shape that reads pixels back — all three redaction modes, since M5
 * §2 folded the smart eraser in beside blur and pixelate. It samples `doc.src`
 * and nothing else, so an arrow crossing a redaction never smears into it — but
 * it still composites at its own place in painter's order, so a redaction added
 * after an arrow does cover that arrow (M2 §4.3, matches ShareX).
 */
function drawRedact(ctx: CanvasRenderingContext2D, doc: EditorDoc, shape: Redact): void {
  const area = sampleArea(doc, shape.rect);
  if (!area) return;
  const { x, y, w, h } = area;

  const src = readableSource(doc);
  if (!src) return;

  const patch =
    shape.mode === 'pixelate'
      ? pixelatePatch(src, x, y, w, h, shape.amount)
      : shape.mode === 'erase'
        ? // `amount` is deliberately not consulted: an erase has nothing to
          // scale, which is why the bar hides the slider for it (M5 §2).
          erasePatch(src, x, y, w, h, doc)
        : blurPatch(src, x, y, w, h, shape.amount, doc);
  if (!patch) return;

  // Pixelate must not be resampled on the way back or the blocks turn to mush;
  // blur and the eraser are already smooth rasters and drawn 1:1 anyway.
  ctx.imageSmoothingEnabled = shape.mode !== 'pixelate';
  ctx.drawImage(patch, x, y, w, h);
}

/**
 * The part of a shape's rect that actually lies on the image, snapped to whole
 * pixels. Every shape that reads pixels back goes through here, so redaction
 * and the eraser cover exactly the same pixels for the same rect.
 */
function sampleArea(
  doc: EditorDoc,
  rect: Rect
): { x: number; y: number; w: number; h: number } | null {
  const n = normalizeRect(rect);
  const x0 = clamp(Math.round(n.x), 0, doc.width);
  const y0 = clamp(Math.round(n.y), 0, doc.height);
  const x1 = clamp(Math.round(n.x + n.width), x0, doc.width);
  const y1 = clamp(Math.round(n.y + n.height), y0, doc.height);
  if (x1 - x0 < 1 || y1 - y0 < 1) return null;
  return { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
}

/** `doc.src`, or null while it is the blank stand-in or has not decoded yet. */
function readableSource(doc: EditorDoc): CanvasImageSource | null {
  const src = doc.src;
  if (typeof HTMLImageElement !== 'undefined' && src instanceof HTMLImageElement) {
    if (!src.complete || src.naturalWidth === 0) return null;
  }
  return src;
}

/**
 * Patches are memoised because the fill is per-pixel work and `render()` runs on
 * every repaint — panning, zooming, or drawing another shape beside an existing
 * erase must not recompute it. The fill depends only on the source pixels and
 * the rect, so geometry is the whole key.
 *
 * Held weakly per source image: `doc.src` is a fresh `ImageBitmap` per load
 * (M2.5 §0), so a new document can never read the previous one's patches, and
 * loading a second capture drops the first one's cache with the bitmap itself.
 */
const ERASE_CACHE_LIMIT = 6;
const erasePatchCache = new WeakMap<CanvasImageSource, Map<string, HTMLCanvasElement>>();

function erasePatch(
  src: CanvasImageSource,
  x: number,
  y: number,
  w: number,
  h: number,
  doc: EditorDoc
): HTMLCanvasElement | null {
  let cache = erasePatchCache.get(src);
  if (!cache) {
    cache = new Map();
    erasePatchCache.set(src, cache);
  }
  const key = `${x},${y},${w},${h}`;
  const hit = cache.get(key);
  if (hit) {
    // Re-insert so eviction is least-recently-USED, not least-recently-built.
    // Without this a drag is pathological: every frame of it mints a fresh
    // geometry, and once those inserts have pushed the settled erases out by
    // age the renderer rebuilds EVERY erase in the document on EVERY frame
    // rather than just the one under the pointer.
    cache.delete(key);
    cache.set(key, hit);
    return hit;
  }

  const patch = buildErasePatch(src, x, y, w, h, doc);
  if (!patch) return null;
  // A Map iterates in insertion order, so the first key is now the least
  // recently used one.
  if (cache.size >= ERASE_CACHE_LIMIT) {
    const oldest = cache.keys().next().value;
    if (oldest !== undefined) cache.delete(oldest);
  }
  cache.set(key, patch);
  return patch;
}

/**
 * Longest edge of the grid the fill is solved on. A Laplace solution is smooth
 * by construction, so it carries no high-frequency detail a downscale could
 * destroy — solving small and scaling back up costs nothing real, and it is
 * what keeps the cost of a 2000px-wide erase the same as a 200px one.
 */
const ERASE_GRID_MAX = 64;
/**
 * Relaxation sweeps. FIXED rather than run to convergence, so the cost is
 * bounded and identical on every frame of a drag: at most
 * ERASE_GRID_MAX² × 4 channels × this, and typically an order of magnitude
 * less because the usual erase is a line of text, not a square.
 */
const ERASE_SWEEPS = 96;

/** The ring of real pixels around the rect, resampled to the solve grid. */
interface EraseRing {
  /** One RGBA sample per grid column, from the pixel row above the rect. */
  top: Uint8ClampedArray | null;
  /** …and from the row below it. */
  bottom: Uint8ClampedArray | null;
  /** One RGBA sample per grid row, from the pixel column left of the rect. */
  left: Uint8ClampedArray | null;
  /** …and from the column right of it. */
  right: Uint8ClampedArray | null;
}

/**
 * The smart eraser (M5 §1). The rect is filled by SOLVING for the smoothest
 * surface that meets the ring of pixels immediately outside it — Laplace's
 * equation with the ring as a Dirichlet boundary — rather than by interpolating
 * between four taps.
 *
 * Why it changed: M2.11 §3 weighted the horizontal and vertical pairs by
 * inverse distance to their edges. On a box wider than it is tall — most of
 * them, since you are usually erasing a line of text — the row above and below
 * sit far closer than the columns left and right, so the vertical pair took
 * nearly all the weight and every column became a vertical blend of whatever
 * happened to sit above and below it. That is the vertical banding the report
 * describes; the algorithm was doing exactly what it was told.
 *
 * A harmonic fill has no preferred axis, so it cannot band, and it reproduces a
 * linear gradient exactly. It is still not content-aware fill: over a photo it
 * produces a smooth smudge rather than invented texture, which is what "blend
 * with nearby colours" means.
 */
function buildErasePatch(
  src: CanvasImageSource,
  x: number,
  y: number,
  w: number,
  h: number,
  doc: EditorDoc
): HTMLCanvasElement | null {
  // Which edges have a pixel outside them at all. A selection flush against the
  // image edge simply has fewer contributors — it must never read out of bounds
  // and pick up transparent black, which would paint a hard dark rectangle
  // exactly where the user wanted something invisible.
  const hasLeft = x > 0;
  const hasRight = x + w < doc.width;
  const hasTop = y > 0;
  const hasBottom = y + h < doc.height;

  const out = scratch(w, h);
  if (!out) return null;

  if (!hasLeft && !hasRight && !hasTop && !hasBottom) {
    // The whole picture is selected: there is no ring to sample, so fall back
    // to the image's mean colour rather than leaving the shape unrendered.
    const mean = meanColor(src, doc);
    if (!mean) return null;
    const flat = out.createImageData(w, h);
    const data = flat.data;
    for (let i = 0; i < data.length; i += 4) {
      data[i] = mean[0];
      data[i + 1] = mean[1];
      data[i + 2] = mean[2];
      data[i + 3] = mean[3];
    }
    out.putImageData(flat, 0, 0);
    return out.canvas;
  }

  // Never larger than the region itself: a small erase is solved at its true
  // size and the scale back up below is a 1:1 blit.
  const shrink = Math.min(1, ERASE_GRID_MAX / Math.max(w, h));
  const gw = Math.max(1, Math.min(w, Math.round(w * shrink)));
  const gh = Math.max(1, Math.min(h, Math.round(h * shrink)));

  /*
   * Only the RING is resampled, never the region with it. Downscaling the whole
   * band in one go would fold the content being erased into the outermost row
   * of the grid — on a 400x30 box each grid cell covers ~6x6 source pixels — and
   * the boundary would then be contaminated by exactly what it is supposed to
   * replace. Each strip is one pixel of real image, box-filtered to the grid, so
   * a boundary value is the average of the pixels it stands for.
   */
  const top = hasTop ? ringStrip(src, x, y - 1, w, 1, gw, 1) : null;
  const bottom = hasBottom ? ringStrip(src, x, y + h, w, 1, gw, 1) : null;
  const left = hasLeft ? ringStrip(src, x - 1, y, 1, h, 1, gh) : null;
  const right = hasRight ? ringStrip(src, x + w, y, 1, h, 1, gh) : null;
  // A strip that was asked for and did not arrive means a tainted canvas, and
  // nothing in the app loads an image that can taint one (M2.5 §0). Drawing
  // nothing still beats throwing out of render() and blanking the stage.
  if ((hasTop && !top) || (hasBottom && !bottom) || (hasLeft && !left) || (hasRight && !right)) {
    return null;
  }

  const solved = solveErase(gw, gh, { top, bottom, left, right });

  const grid = scratch(gw, gh);
  if (!grid) return null;
  const img = grid.createImageData(gw, gh);
  const px = img.data;
  // The solved field carries a one-cell apron of boundary values; only its
  // interior is the fill. Uint8ClampedArray rounds and clamps on assignment.
  const stride = (gw + 2) * 4;
  for (let j = 0; j < gh; j++) {
    const from = (j + 1) * stride + 4;
    const to = j * gw * 4;
    for (let i = 0; i < gw * 4; i++) px[to + i] = solved[from + i];
  }
  grid.putImageData(img, 0, 0);

  // Bilinearly back to the real size. See ERASE_GRID_MAX: there is no detail
  // here to lose, and the alternative — relaxing at full resolution — is the
  // same answer for a hundred times the work.
  out.imageSmoothingEnabled = true;
  out.imageSmoothingQuality = 'high';
  out.drawImage(grid.canvas, 0, 0, gw, gh, 0, 0, w, h);
  return out.canvas;
}

/**
 * One strip of source pixels, resampled to `dw` x `dh`. Null only when the
 * canvas could not be read back, which is a tainted source and nothing else.
 */
function ringStrip(
  src: CanvasImageSource,
  sx: number,
  sy: number,
  sw: number,
  sh: number,
  dw: number,
  dh: number
): Uint8ClampedArray | null {
  const ctx = scratch(dw, dh);
  if (!ctx) return null;
  // Smoothed, so shrinking a 400px edge to 64 samples averages each run of
  // pixels rather than point-sampling one out of it.
  ctx.imageSmoothingEnabled = true;
  ctx.imageSmoothingQuality = 'high';
  ctx.drawImage(src, sx, sy, sw, sh, 0, 0, dw, dh);
  try {
    return ctx.getImageData(0, 0, dw, dh).data;
  } catch {
    return null;
  }
}

/**
 * Gauss-Seidel relaxation towards the harmonic fill, all four channels at once.
 *
 * The field is stored with a one-cell APRON around the interior, holding the
 * ring values. That is what keeps the inner loop branch-free — every interior
 * cell has four neighbours in the array, boundary or not — and it is where a
 * missing edge is handled: `mirrorApron` copies the adjacent interior line into
 * it before each sweep, which is a zero-flux boundary, the discrete spelling of
 * "this side contributes nothing". A selection flush against the image edge
 * therefore has fewer contributors rather than reading out of bounds.
 *
 * Returns the whole field, apron included; the caller takes the interior.
 */
function solveErase(gw: number, gh: number, ring: EraseRing): Float32Array {
  const sw = gw + 2;
  const sh = gh + 2;
  const stride = sw * 4;
  const f = new Float32Array(sw * sh * 4);
  const { top, bottom, left, right } = ring;

  // The apron. Its four corners are never read by a five-point stencil, so they
  // are left at zero.
  for (let i = 0; i < gw; i++) {
    const t = (i + 1) * 4;
    const b = (sh - 1) * stride + (i + 1) * 4;
    for (let c = 0; c < 4; c++) {
      if (top) f[t + c] = top[i * 4 + c];
      if (bottom) f[b + c] = bottom[i * 4 + c];
    }
  }
  for (let j = 0; j < gh; j++) {
    const l = (j + 1) * stride;
    const r = (j + 1) * stride + (sw - 1) * 4;
    for (let c = 0; c < 4; c++) {
      if (left) f[l + c] = left[j * 4 + c];
      if (right) f[r + c] = right[j * 4 + c];
    }
  }

  /*
   * Seed the interior with the axis-averaged linear ramp between opposite
   * edges. It is not the answer, but it is already exact for a linear gradient
   * and carries the low-frequency part of anything else — which is precisely
   * the part relaxation is slowest to fix, and why a fixed sweep count is
   * enough. The sweeps then remove the rest, and high-frequency error is what
   * Gauss-Seidel kills fastest.
   *
   * Note the EQUAL split between the two axes. Weighting them by distance to
   * their edges is what banded (M5 §1); an axis is dropped here only when it
   * has no ring at all.
   */
  for (let j = 0; j < gh; j++) {
    const v = (j + 0.5) / gh;
    for (let i = 0; i < gw; i++) {
      const u = (i + 0.5) / gw;
      const o = (j + 1) * stride + (i + 1) * 4;
      for (let c = 0; c < 4; c++) {
        let sum = 0;
        let n = 0;
        if (left && right) {
          sum += left[j * 4 + c] * (1 - u) + right[j * 4 + c] * u;
          n++;
        } else if (left) {
          sum += left[j * 4 + c];
          n++;
        } else if (right) {
          sum += right[j * 4 + c];
          n++;
        }
        if (top && bottom) {
          sum += top[i * 4 + c] * (1 - v) + bottom[i * 4 + c] * v;
          n++;
        } else if (top) {
          sum += top[i * 4 + c];
          n++;
        } else if (bottom) {
          sum += bottom[i * 4 + c];
          n++;
        }
        // `n` is at least 1: the caller has already dealt with the case where
        // no edge exists at all.
        f[o + c] = sum / n;
      }
    }
  }

  for (let s = 0; s < ERASE_SWEEPS; s++) {
    mirrorApron(f, gw, gh, ring);
    for (let j = 1; j <= gh; j++) {
      const row = j * stride;
      for (let i = 1; i <= gw; i++) {
        const o = row + i * 4;
        // Each cell becomes the mean of its four neighbours — in place, so the
        // two already visited this sweep are the updated ones.
        f[o] = (f[o - 4] + f[o + 4] + f[o - stride] + f[o + stride]) * 0.25;
        f[o + 1] = (f[o - 3] + f[o + 5] + f[o - stride + 1] + f[o + stride + 1]) * 0.25;
        f[o + 2] = (f[o - 2] + f[o + 6] + f[o - stride + 2] + f[o + stride + 2]) * 0.25;
        f[o + 3] = (f[o - 1] + f[o + 7] + f[o - stride + 3] + f[o + stride + 3]) * 0.25;
      }
    }
  }
  return f;
}

/**
 * Refresh the apron on the sides that have no ring, by copying the interior
 * line beside them. Cheap — O(gw + gh) — and it is the whole of the
 * "fewer contributors" rule: a mirrored neighbour exerts no pull of its own.
 */
function mirrorApron(f: Float32Array, gw: number, gh: number, ring: EraseRing): void {
  const sw = gw + 2;
  const sh = gh + 2;
  const stride = sw * 4;
  if (!ring.top) f.copyWithin(4, stride + 4, stride + 4 + gw * 4);
  if (!ring.bottom) {
    const src = (sh - 2) * stride + 4;
    f.copyWithin((sh - 1) * stride + 4, src, src + gw * 4);
  }
  if (!ring.left) {
    for (let j = 1; j <= gh; j++) f.copyWithin(j * stride, j * stride + 4, j * stride + 8);
  }
  if (!ring.right) {
    for (let j = 1; j <= gh; j++) {
      const src = j * stride + (sw - 2) * 4;
      f.copyWithin(j * stride + (sw - 1) * 4, src, src + 4);
    }
  }
}

/**
 * Mean colour of the whole image, taken from a small downscale rather than a
 * full-resolution read. Only reached when a selection covers the entire picture
 * and has no ring to sample.
 */
function meanColor(
  src: CanvasImageSource,
  doc: EditorDoc
): [number, number, number, number] | null {
  const w = clamp(Math.round(doc.width), 1, 64);
  const h = clamp(Math.round(doc.height), 1, 64);
  const small = scratch(w, h);
  if (!small) return null;
  small.imageSmoothingEnabled = true;
  small.imageSmoothingQuality = 'high';
  small.drawImage(src, 0, 0, doc.width, doc.height, 0, 0, w, h);

  let px: Uint8ClampedArray;
  try {
    px = small.getImageData(0, 0, w, h).data;
  } catch {
    return null;
  }
  let r = 0;
  let g = 0;
  let b = 0;
  let a = 0;
  for (let i = 0; i < px.length; i += 4) {
    r += px[i];
    g += px[i + 1];
    b += px[i + 2];
    a += px[i + 3];
  }
  const n = px.length / 4;
  return [r / n, g / n, b / n, a / n];
}

function scratch(width: number, height: number): CanvasRenderingContext2D | null {
  if (typeof document === 'undefined') return null;
  const canvas = document.createElement('canvas');
  canvas.width = Math.max(1, Math.round(width));
  canvas.height = Math.max(1, Math.round(height));
  return canvas.getContext('2d');
}

function pixelatePatch(
  src: CanvasImageSource,
  x: number,
  y: number,
  w: number,
  h: number,
  amount: number
): HTMLCanvasElement | null {
  const cell = clamp(Math.round(amount) || 1, 2, 400);
  const small = scratch(Math.max(1, Math.round(w / cell)), Math.max(1, Math.round(h / cell)));
  if (!small) return null;
  // Downscale WITH smoothing: that averages each cell instead of point-
  // sampling one pixel out of it, which is what makes the blocks readable.
  small.imageSmoothingEnabled = true;
  small.imageSmoothingQuality = 'high';
  small.drawImage(src, x, y, w, h, 0, 0, small.canvas.width, small.canvas.height);

  const out = scratch(w, h);
  if (!out) return null;
  out.imageSmoothingEnabled = false;
  out.drawImage(small.canvas, 0, 0, small.canvas.width, small.canvas.height, 0, 0, w, h);
  return out.canvas;
}

function blurPatch(
  src: CanvasImageSource,
  x: number,
  y: number,
  w: number,
  h: number,
  amount: number,
  doc: EditorDoc
): HTMLCanvasElement | null {
  const radius = clamp(Math.round(amount) || 1, 1, 200);
  // Sample a margin of real pixels around the region: blurring a canvas that
  // stops at the region edge pulls in transparent black and leaves a washed-out
  // frame around the redaction.
  const pad = radius * 2;
  const sx = clamp(x - pad, 0, doc.width);
  const sy = clamp(y - pad, 0, doc.height);
  const sw = clamp(x + w + pad, sx, doc.width) - sx;
  const sh = clamp(y + h + pad, sy, doc.height) - sy;
  if (sw < 1 || sh < 1) return null;

  const off = scratch(sw, sh);
  if (!off) return null;
  off.filter = `blur(${radius}px)`;
  off.drawImage(src, sx, sy, sw, sh, 0, 0, sw, sh);
  off.filter = 'none';

  const out = scratch(w, h);
  if (!out) return null;
  out.drawImage(off.canvas, x - sx, y - sy, w, h, 0, 0, w, h);
  return out.canvas;
}

/* ------------------------------------------------------------------ colour */

/** sRGB relative luminance, or null when the colour is not one we can parse. */
function luminance(color: string): number | null {
  const rgb = parseColor(color);
  if (!rgb) return null;
  const [r, g, b] = rgb.map((c) => {
    const v = c / 255;
    return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

function parseColor(color: string): [number, number, number] | null {
  const value = color.trim();
  const hex = /^#([0-9a-f]{3,8})$/i.exec(value);
  if (hex) {
    const d = hex[1];
    if (d.length === 3 || d.length === 4) {
      return [
        parseInt(d[0] + d[0], 16),
        parseInt(d[1] + d[1], 16),
        parseInt(d[2] + d[2], 16)
      ];
    }
    if (d.length === 6 || d.length === 8) {
      return [
        parseInt(d.slice(0, 2), 16),
        parseInt(d.slice(2, 4), 16),
        parseInt(d.slice(4, 6), 16)
      ];
    }
    return null;
  }
  const fn = /^rgba?\(\s*([\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)/i.exec(value);
  if (fn) return [Number(fn[1]), Number(fn[2]), Number(fn[3])];
  return null;
}

/** Outline colour that keeps a glyph legible over any background. */
function haloColor(color: string): string {
  const l = luminance(color);
  return l !== null && l < 0.42 ? 'rgba(255,255,255,0.9)' : 'rgba(0,0,0,0.72)';
}

/** Numeral colour that reads on top of a filled badge. */
function inkColor(color: string): string {
  const l = luminance(color);
  return l !== null && l > 0.55 ? '#111111' : '#ffffff';
}
