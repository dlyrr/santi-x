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
 * The only shape that reads pixels back. It samples `doc.src` and nothing
 * else, so an arrow crossing a redaction never smears into it — but it still
 * composites at its own place in painter's order, so a redaction added after
 * an arrow does cover that arrow (M2 §4.3, matches ShareX).
 */
function drawRedact(ctx: CanvasRenderingContext2D, doc: EditorDoc, shape: Redact): void {
  const n = normalizeRect(shape.rect);
  const x0 = clamp(Math.round(n.x), 0, doc.width);
  const y0 = clamp(Math.round(n.y), 0, doc.height);
  const x1 = clamp(Math.round(n.x + n.width), x0, doc.width);
  const y1 = clamp(Math.round(n.y + n.height), y0, doc.height);
  const w = x1 - x0;
  const h = y1 - y0;
  if (w < 1 || h < 1) return;

  const src = doc.src;
  if (typeof HTMLImageElement !== 'undefined' && src instanceof HTMLImageElement) {
    if (!src.complete || src.naturalWidth === 0) return;
  }

  const patch =
    shape.mode === 'pixelate'
      ? pixelatePatch(src, x0, y0, w, h, shape.amount)
      : blurPatch(src, x0, y0, w, h, shape.amount, doc);
  if (!patch) return;

  // Pixelate must not be resampled on the way back or the blocks turn to mush;
  // blur is already a smooth raster and drawn 1:1 anyway.
  ctx.imageSmoothingEnabled = shape.mode !== 'pixelate';
  ctx.drawImage(patch, x0, y0, w, h);
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
