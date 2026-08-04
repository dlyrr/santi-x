/**
 * The tool table and the geometry the Select tool needs — M2 §4.4.
 *
 * Everything here is in image px. Hit tolerances are the one place a CSS px
 * leaks in, and only as an argument: `scale` is the stage's current zoom, and
 * the tolerance is divided by it so a 1px line stays grabbable at 20% zoom and
 * does not turn into a 40px-wide target at 800%.
 */
import { cropRect, normalizeRect, stepRadius, textBounds } from './render';
import type {
  EditorDoc,
  Point,
  Rect,
  Redact,
  Shape,
  ShapeKind,
  ShapePatch
} from './doc.svelte';

export type ToolId =
  | 'select'
  | 'arrow'
  | 'rect'
  | 'ellipse'
  | 'line'
  | 'pen'
  | 'text'
  | 'highlight'
  | 'redact'
  | 'step'
  | 'crop';

/** Which controls the shared options bar shows for the active tool. */
export type ToolOption = 'color' | 'stroke' | 'fill' | 'textSize' | 'redactMode' | 'redactAmount';

export type RedactMode = Redact['mode'];

/**
 * The live value of every control in the shared options bar (M2 §4.4). One flat
 * record rather than a per-tool bag: the bar keeps a colour and a stroke width
 * across a tool switch, and `optionsForShape` decides which of these fields a
 * given shape is actually allowed to adopt.
 *
 * Keys are exactly the `ToolOption` names, so `toolHasOption(tool, key)`
 * answers "does this control belong on the bar" for any field of this type.
 */
export type ToolOptions = {
  color: string;
  stroke: number;
  fill: boolean;
  textSize: number;
  redactMode: RedactMode;
  redactAmount: number;
};

/** Option overrides a shortcut may carry with it. */
export type ToolPreset = Partial<ToolOptions>;

export interface ToolDef {
  id: ToolId;
  label: string;
  /** Single-key shortcut, lower case. Matches ShareX's region-capture keymap. */
  key: string;
  /**
   * A second key that also selects this tool, applying `altPreset`.
   *
   * ShareX splits into two tools what santi.sharex models as one with a mode:
   * `B` is Blur and `P` is Pixelate. Rather than fork Redact in two, both keys
   * select it and set the mode, so the ShareX muscle memory lands correctly.
   */
  altKey?: string;
  /** Applied when `key` selects this tool. */
  preset?: ToolPreset;
  /** Applied when `altKey` selects this tool. */
  altPreset?: ToolPreset;
  /** CSS cursor for the stage while this tool is active. */
  cursor: string;
  options: readonly ToolOption[];
  /** The shape kind a drag with this tool produces; null for select/crop. */
  creates: ShapeKind | null;
  /** What Shift does, for the toolbar tooltip. */
  shift?: string;
}

/** Toolbar order is the order of this array (M2 §4.4). */
export const TOOLS: readonly ToolDef[] = [
  {
    id: 'select',
    label: 'Select',
    key: 'm',
    cursor: 'default',
    options: [],
    creates: null
  },
  {
    id: 'arrow',
    label: 'Arrow',
    key: 'a',
    cursor: 'crosshair',
    options: ['color', 'stroke'],
    creates: 'arrow',
    shift: 'Constrain to 15°'
  },
  {
    id: 'rect',
    label: 'Rectangle',
    key: 'r',
    cursor: 'crosshair',
    options: ['color', 'stroke', 'fill'],
    creates: 'rect',
    shift: 'Square'
  },
  {
    id: 'ellipse',
    label: 'Ellipse',
    key: 'e',
    cursor: 'crosshair',
    options: ['color', 'stroke', 'fill'],
    creates: 'ellipse',
    shift: 'Circle'
  },
  {
    id: 'line',
    label: 'Line',
    key: 'l',
    cursor: 'crosshair',
    options: ['color', 'stroke'],
    creates: 'line',
    shift: 'Constrain to 15°'
  },
  {
    id: 'pen',
    label: 'Pen',
    key: 'f',
    cursor: 'crosshair',
    options: ['color', 'stroke'],
    creates: 'pen'
  },
  {
    id: 'text',
    label: 'Text',
    key: 't',
    cursor: 'text',
    options: ['color', 'textSize'],
    creates: 'text'
  },
  {
    id: 'highlight',
    label: 'Highlight',
    key: 'h',
    cursor: 'crosshair',
    options: ['color'],
    creates: 'highlight',
    shift: 'Square'
  },
  {
    id: 'redact',
    label: 'Redact',
    key: 'b',
    altKey: 'p',
    preset: { redactMode: 'blur' },
    altPreset: { redactMode: 'pixelate' },
    cursor: 'crosshair',
    options: ['redactMode', 'redactAmount'],
    creates: 'redact',
    shift: 'Square'
  },
  {
    id: 'step',
    label: 'Step',
    key: 'i',
    cursor: 'crosshair',
    options: ['color', 'stroke'],
    creates: 'step'
  },
  {
    id: 'crop',
    label: 'Crop',
    key: 'c',
    cursor: 'crosshair',
    options: [],
    creates: null
  }
];

const BY_ID = new Map<ToolId, ToolDef>(TOOLS.map((t) => [t.id, t]));
/** Both `key` and `altKey` resolve here, each carrying its own preset. */
const BY_KEY = new Map<string, { tool: ToolDef; preset?: ToolPreset }>();
for (const t of TOOLS) {
  BY_KEY.set(t.key, { tool: t, preset: t.preset });
  if (t.altKey) BY_KEY.set(t.altKey, { tool: t, preset: t.altPreset });
}

export function toolById(id: ToolId): ToolDef {
  // Every ToolId is in the table, so this cannot miss.
  return BY_ID.get(id) as ToolDef;
}

/** Resolve a bare `KeyboardEvent.key` to a tool. Modifier filtering is the caller's. */
export function toolForKey(key: string): ToolDef | null {
  return BY_KEY.get(key.toLowerCase())?.tool ?? null;
}

/**
 * The same lookup, plus any option overrides the key implies — `B` selects
 * Redact in blur mode, `P` selects it in pixelate mode, matching ShareX's two
 * separate tools. Callers that can apply options should prefer this.
 */
export function toolHitForKey(key: string): { tool: ToolDef; preset?: ToolPreset } | null {
  return BY_KEY.get(key.toLowerCase()) ?? null;
}

export function toolHasOption(id: ToolId, option: ToolOption): boolean {
  return toolById(id).options.includes(option);
}

/**
 * Which options apply to an already-drawn shape. The Select tool exposes no
 * options of its own; with a shape selected the bar shows that shape's instead,
 * and changing one applies to it (M2 §4.4).
 */
export function optionsForShape(shape: Shape): readonly ToolOption[] {
  return optionsForKind(shape.kind);
}

/**
 * The same table addressed by kind alone — the toolbar is handed the selected
 * shape's `kind`, not the shape. Kept in one place so the bar can never offer a
 * control the renderer ignores: `highlight`, for instance, is a flat fill and
 * has no stroke to set.
 */
export function optionsForKind(kind: ShapeKind): readonly ToolOption[] {
  switch (kind) {
    case 'text':
      return ['color', 'textSize'];
    case 'highlight':
      return ['color'];
    case 'redact':
      return ['redactMode', 'redactAmount'];
    case 'rect':
    case 'ellipse':
      return ['color', 'stroke', 'fill'];
    default:
      return ['color', 'stroke'];
  }
}

/* -------------------------------------------------------------- hit-testing */

/** Grab radius in CSS px; converted to image px through the stage's zoom. */
export const HIT_SLOP = 6;
/** On-screen size of a resize handle, in CSS px. */
export const HANDLE_SIZE = 8;

/**
 * Topmost shape under `point`, or null. Walks in reverse painter's order, so
 * the shape drawn last — the one the user sees on top — wins.
 */
export function hitTest(doc: EditorDoc, point: Point, scale = 1): Shape | null {
  const tol = HIT_SLOP / Math.max(scale, 0.02);
  // A shape scrolled outside the crop is not visible, so it is not clickable.
  const area = cropRect(doc);
  for (let i = doc.shapes.length - 1; i >= 0; i--) {
    const shape = doc.shapes[i];
    if (!rectsOverlap(shapeBounds(shape), area)) continue;
    if (hits(shape, point, tol)) return shape;
  }
  return null;
}

function hits(shape: Shape, p: Point, tol: number): boolean {
  const half = Math.max(1, shape.stroke) / 2;
  switch (shape.kind) {
    case 'arrow':
    case 'line':
      return distToSegment(p, shape.from, shape.to) <= half + tol;
    case 'pen': {
      const pts = shape.points;
      if (pts.length === 1) return distance(p, pts[0]) <= half + tol;
      for (let i = 1; i < pts.length; i++) {
        if (distToSegment(p, pts[i - 1], pts[i]) <= half + tol) return true;
      }
      return false;
    }
    case 'rect':
      return shape.fill
        ? insideRect(p, normalizeRect(shape.rect), tol)
        : nearRectEdge(p, normalizeRect(shape.rect), half + tol);
    case 'ellipse':
      return hitsEllipse(p, normalizeRect(shape.rect), shape.fill, half + tol);
    case 'highlight':
    case 'redact':
      return insideRect(p, normalizeRect(shape.rect), tol);
    case 'text':
      return insideRect(p, textBounds(shape), tol);
    case 'step':
      return distance(p, shape.at) <= stepRadius(shape) + tol;
  }
}

function hitsEllipse(p: Point, r: Rect, fill: boolean, tol: number): boolean {
  const rx = Math.max(r.width / 2, 0.5);
  const ry = Math.max(r.height / 2, 0.5);
  const nx = (p.x - (r.x + rx)) / rx;
  const ny = (p.y - (r.y + ry)) / ry;
  const d = Math.hypot(nx, ny);
  // `d` is in radius-normalised units; scaling by the smaller radius turns the
  // slack back into (approximate) image px, which is close enough for a grab.
  const slack = tol / Math.min(rx, ry);
  return fill ? d <= 1 + slack : Math.abs(d - 1) <= slack;
}

function insideRect(p: Point, r: Rect, tol: number): boolean {
  return (
    p.x >= r.x - tol && p.x <= r.x + r.width + tol && p.y >= r.y - tol && p.y <= r.y + r.height + tol
  );
}

function nearRectEdge(p: Point, r: Rect, tol: number): boolean {
  if (!insideRect(p, r, tol)) return false;
  const inner = p.x > r.x + tol && p.x < r.x + r.width - tol;
  const innerY = p.y > r.y + tol && p.y < r.y + r.height - tol;
  return !(inner && innerY);
}

/* ------------------------------------------------------------------ handles */

export type HandleId = 'nw' | 'n' | 'ne' | 'e' | 'se' | 's' | 'sw' | 'w' | 'from' | 'to';

export interface Handle {
  id: HandleId;
  /** Image px. */
  at: Point;
  cursor: string;
}

const RECT_HANDLES: readonly { id: HandleId; fx: number; fy: number; cursor: string }[] = [
  { id: 'nw', fx: 0, fy: 0, cursor: 'nwse-resize' },
  { id: 'n', fx: 0.5, fy: 0, cursor: 'ns-resize' },
  { id: 'ne', fx: 1, fy: 0, cursor: 'nesw-resize' },
  { id: 'e', fx: 1, fy: 0.5, cursor: 'ew-resize' },
  { id: 'se', fx: 1, fy: 1, cursor: 'nwse-resize' },
  { id: 's', fx: 0.5, fy: 1, cursor: 'ns-resize' },
  { id: 'sw', fx: 0, fy: 1, cursor: 'nesw-resize' },
  { id: 'w', fx: 0, fy: 0.5, cursor: 'ew-resize' }
];

/**
 * Eight resize handles for rect-like shapes, two endpoint handles for
 * arrow/line, none for pen, text and step — those are moved, not resized.
 */
export function handlesFor(shape: Shape): Handle[] {
  if (shape.kind === 'arrow' || shape.kind === 'line') {
    return [
      { id: 'from', at: { ...shape.from }, cursor: 'grab' },
      { id: 'to', at: { ...shape.to }, cursor: 'grab' }
    ];
  }
  if (
    shape.kind === 'rect' ||
    shape.kind === 'ellipse' ||
    shape.kind === 'highlight' ||
    shape.kind === 'redact'
  ) {
    const r = normalizeRect(shape.rect);
    return RECT_HANDLES.map((h) => ({
      id: h.id,
      at: { x: r.x + r.width * h.fx, y: r.y + r.height * h.fy },
      cursor: h.cursor
    }));
  }
  return [];
}

/** The handle under `point`, if any. `scale` keeps the grab area constant on screen. */
export function handleAt(shape: Shape, point: Point, scale = 1): Handle | null {
  const reach = (HANDLE_SIZE / 2 + HIT_SLOP / 2) / Math.max(scale, 0.02);
  for (const handle of handlesFor(shape)) {
    if (Math.abs(point.x - handle.at.x) <= reach && Math.abs(point.y - handle.at.y) <= reach) {
      return handle;
    }
  }
  return null;
}

/** The patch that drags `handle` to `point`. Feed straight into `doc.updateShape()`. */
export function resizePatch(shape: Shape, handle: HandleId, point: Point): ShapePatch {
  if (handle === 'from') return { from: { ...point } };
  if (handle === 'to') return { to: { ...point } };
  if (
    shape.kind !== 'rect' &&
    shape.kind !== 'ellipse' &&
    shape.kind !== 'highlight' &&
    shape.kind !== 'redact'
  ) {
    return {};
  }
  const r = normalizeRect(shape.rect);
  let x0 = r.x;
  let y0 = r.y;
  let x1 = r.x + r.width;
  let y1 = r.y + r.height;
  if (handle.includes('w')) x0 = point.x;
  if (handle.includes('e')) x1 = point.x;
  if (handle.includes('n')) y0 = point.y;
  if (handle.includes('s')) y1 = point.y;
  // Re-normalise: dragging the west handle past the east edge flips the rect
  // rather than collapsing it, which is what every other editor does.
  return { rect: normalizeRect({ x: x0, y: y0, width: x1 - x0, height: y1 - y0 }) };
}

/** The patch that translates a whole shape by (dx, dy) image px. */
export function movePatch(shape: Shape, dx: number, dy: number): ShapePatch {
  switch (shape.kind) {
    case 'arrow':
    case 'line':
      return {
        from: { x: shape.from.x + dx, y: shape.from.y + dy },
        to: { x: shape.to.x + dx, y: shape.to.y + dy }
      };
    case 'rect':
    case 'ellipse':
    case 'highlight':
    case 'redact':
      return { rect: { ...shape.rect, x: shape.rect.x + dx, y: shape.rect.y + dy } };
    case 'pen':
      return { points: shape.points.map((p) => ({ x: p.x + dx, y: p.y + dy })) };
    case 'text':
    case 'step':
      return { at: { x: shape.at.x + dx, y: shape.at.y + dy } };
  }
}

/** Bounding box in image px, stroke included — the box the stage outlines. */
export function shapeBounds(shape: Shape): Rect {
  const pad = Math.max(1, shape.stroke) / 2;
  switch (shape.kind) {
    case 'arrow':
    case 'line':
      return padRect(boxOf([shape.from, shape.to]), shape.kind === 'arrow' ? pad * 4 : pad);
    case 'pen':
      return padRect(boxOf(shape.points), pad);
    case 'rect':
    case 'ellipse':
      return padRect(normalizeRect(shape.rect), shape.fill ? 0 : pad);
    case 'highlight':
    case 'redact':
      return normalizeRect(shape.rect);
    case 'text':
      return textBounds(shape);
    case 'step': {
      const r = stepRadius(shape);
      return { x: shape.at.x - r, y: shape.at.y - r, width: r * 2, height: r * 2 };
    }
  }
}

/* ---------------------------------------------------------- drag primitives */

/** Rect spanned by two points; `square` is the Shift constraint. */
export function rectFromPoints(a: Point, b: Point, square = false): Rect {
  let dx = b.x - a.x;
  let dy = b.y - a.y;
  if (square) {
    const size = Math.max(Math.abs(dx), Math.abs(dy));
    dx = Math.sign(dx || 1) * size;
    dy = Math.sign(dy || 1) * size;
  }
  return normalizeRect({ x: a.x, y: a.y, width: dx, height: dy });
}

/** Snap `to` onto the nearest multiple of `stepDeg` around `from` (Shift on arrow/line). */
export function constrainAngle(from: Point, to: Point, stepDeg = 15): Point {
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  const len = Math.hypot(dx, dy);
  if (len < 0.001) return { ...to };
  const step = (stepDeg * Math.PI) / 180;
  const angle = Math.round(Math.atan2(dy, dx) / step) * step;
  return { x: from.x + Math.cos(angle) * len, y: from.y + Math.sin(angle) * len };
}

/**
 * Drop points closer than `tolerance` to the previous kept one. Pointer events
 * arrive far denser than the stroke needs; the raw list bloats every undo
 * snapshot for a curve that looks identical (M2 §4.4).
 */
export function simplifyPoints(points: Point[], tolerance = 1.5): Point[] {
  if (points.length < 3) return points.map((p) => ({ ...p }));
  const out: Point[] = [{ ...points[0] }];
  for (let i = 1; i < points.length - 1; i++) {
    if (distance(points[i], out[out.length - 1]) >= tolerance) out.push({ ...points[i] });
  }
  // The last point is always kept: it is where the user let go.
  out.push({ ...points[points.length - 1] });
  return out;
}

export function clampToImage(point: Point, doc: EditorDoc): Point {
  return {
    x: Math.min(Math.max(point.x, 0), doc.width),
    y: Math.min(Math.max(point.y, 0), doc.height)
  };
}

/* ------------------------------------------------------------------ maths */

function distance(a: Point, b: Point): number {
  return Math.hypot(a.x - b.x, a.y - b.y);
}

function distToSegment(p: Point, a: Point, b: Point): number {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const lenSq = dx * dx + dy * dy;
  if (lenSq < 0.000001) return distance(p, a);
  let t = ((p.x - a.x) * dx + (p.y - a.y) * dy) / lenSq;
  t = t < 0 ? 0 : t > 1 ? 1 : t;
  return Math.hypot(p.x - (a.x + t * dx), p.y - (a.y + t * dy));
}

function boxOf(points: Point[]): Rect {
  if (points.length === 0) return { x: 0, y: 0, width: 0, height: 0 };
  let minX = points[0].x;
  let minY = points[0].y;
  let maxX = minX;
  let maxY = minY;
  for (const p of points) {
    if (p.x < minX) minX = p.x;
    if (p.y < minY) minY = p.y;
    if (p.x > maxX) maxX = p.x;
    if (p.y > maxY) maxY = p.y;
  }
  return { x: minX, y: minY, width: maxX - minX, height: maxY - minY };
}

function padRect(r: Rect, pad: number): Rect {
  return { x: r.x - pad, y: r.y - pad, width: r.width + pad * 2, height: r.height + pad * 2 };
}

function rectsOverlap(a: Rect, b: Rect): boolean {
  return (
    a.x <= b.x + b.width && a.x + a.width >= b.x && a.y <= b.y + b.height && a.y + a.height >= b.y
  );
}
