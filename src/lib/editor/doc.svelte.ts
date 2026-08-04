/**
 * The editor document — M2 §4.1 (model) and §4.2 (undo/redo).
 *
 * Every coordinate stored here is in IMAGE pixels. The stage owns the single
 * CSS-px -> image-px conversion; nothing in this module, in `tools.ts` or in
 * `render.ts` does its own coordinate maths. Same discipline that keeps the M1
 * region overlay correct, and for the same reason.
 */
import type { Rect } from '$lib/types';

/** Re-exported so an editor module only ever imports from `$lib/editor/*`. */
export type { Rect };

export type ShapeId = string;

/** ALWAYS image pixels, never CSS px. */
export type Point = { x: number; y: number };

export type ShapeBase = { id: ShapeId; kind: string; color: string; stroke: number };

export type Arrow = ShapeBase & { kind: 'arrow'; from: Point; to: Point };
export type Line = ShapeBase & { kind: 'line'; from: Point; to: Point };
export type RectShape = ShapeBase & { kind: 'rect'; rect: Rect; fill: boolean };
export type Ellipse = ShapeBase & { kind: 'ellipse'; rect: Rect; fill: boolean };
export type Pen = ShapeBase & { kind: 'pen'; points: Point[] };
export type Highlight = ShapeBase & { kind: 'highlight'; rect: Rect };
/** `at` is the TOP-LEFT of the text block; lines grow downwards. */
export type TextShape = ShapeBase & { kind: 'text'; at: Point; text: string; size: number };
/** `amount` is a blur radius in image px, or a pixelate cell size in image px. */
export type Redact = ShapeBase & {
  kind: 'redact';
  rect: Rect;
  mode: 'blur' | 'pixelate';
  amount: number;
};
/** `at` is the CENTRE of the badge. */
export type Step = ShapeBase & { kind: 'step'; at: Point; n: number };

export type Shape =
  | Arrow
  | Line
  | RectShape
  | Ellipse
  | Pen
  | Highlight
  | TextShape
  | Redact
  | Step;

export type ShapeKind = Shape['kind'];

export interface EditorDoc {
  src: HTMLImageElement | ImageBitmap;
  /** Image pixels — the *uncropped* source size. */
  width: number;
  height: number;
  /** Applied at export time, non-destructive. */
  crop: Rect | null;
  /** Painter's order: index 0 is drawn first, last is drawn on top. */
  shapes: Shape[];
  selected: ShapeId | null;
}

/** A shape as handed to `add()` — the store mints the id. */
type WithoutId<T> = T extends { id: ShapeId } ? Omit<T, 'id'> : never;
export type ShapeInit = WithoutId<Shape>;

/**
 * A patch may touch any field of any shape kind. Built as an intersection of
 * every variant rather than `Partial<Shape>`, which would collapse to the
 * fields all variants share and lose `to`, `rect`, `points`, …
 *
 * `id` and `kind` are dropped from each variant *before* the intersection:
 * intersecting the kind literals gives `never`, and one `never` property
 * reduces the whole intersection to `never`.
 */
type ShapeFields = Omit<Arrow, 'id' | 'kind'> &
  Omit<Line, 'id' | 'kind'> &
  Omit<RectShape, 'id' | 'kind'> &
  Omit<Ellipse, 'id' | 'kind'> &
  Omit<Pen, 'id' | 'kind'> &
  Omit<Highlight, 'id' | 'kind'> &
  Omit<TextShape, 'id' | 'kind'> &
  Omit<Redact, 'id' | 'kind'> &
  Omit<Step, 'id' | 'kind'>;

export type ShapePatch = Partial<ShapeFields>;

/** One undo step. Holds only cloneable state — never `doc.src`. */
interface DocSnapshot {
  label: string;
  /** Identity of the document state this snapshot holds; see `dirty`. */
  stamp: number;
  shapes: Shape[];
  crop: Rect | null;
  selected: ShapeId | null;
}

const HISTORY_LIMIT = 100;

/**
 * `structuredClone` throws `DataCloneError` on a runes proxy, and live shapes
 * are proxied. `$state.snapshot` is the same structured-clone algorithm with
 * the proxies peeled off first, and is a plain deep clone for plain input, so
 * it serves both directions: state -> stack and stack -> state.
 */
function cloneShapes(shapes: Shape[]): Shape[] {
  return $state.snapshot(shapes) as Shape[];
}

function cloneRect(rect: Rect | null): Rect | null {
  return rect ? { x: rect.x, y: rect.y, width: rect.width, height: rect.height } : null;
}

const clamp = (v: number, lo: number, hi: number) => (v < lo ? lo : v > hi ? hi : v);

/**
 * Stand-in for `EditorDoc.src` while nothing is loaded. The contract types `src`
 * as non-null (M2 §4.1) precisely so the store can be handed straight to
 * `render()` and `renderToPng()` with no null dance at every call site. An
 * `<img>` with no source reports `naturalWidth === 0`, which is exactly the
 * "has not decoded yet" case the renderer already skips.
 *
 * Built lazily: this module is imported before there is a DOM, and the getter
 * that needs it only ever runs while painting.
 */
let blankSrc: HTMLImageElement | null = null;

function blankSource(): HTMLImageElement {
  blankSrc ??=
    typeof Image === 'undefined'
      ? ({ complete: false, naturalWidth: 0, naturalHeight: 0 } as unknown as HTMLImageElement)
      : new Image();
  return blankSrc;
}

/**
 * The document store. Its public surface deliberately *is* `EditorDoc` — `src`,
 * `width`, `height`, `crop`, `shapes` and `selected` all read exactly as the
 * plain shape does — so the store itself can be passed to `render()`,
 * `hitTest()` and `renderToPng()` without ever unwrapping it. The `implements`
 * clause is what keeps that promise from rotting.
 */
export class EditorDocStore implements EditorDoc {
  #doc = $state<EditorDoc | null>(null);
  #past = $state<DocSnapshot[]>([]);
  #future = $state<DocSnapshot[]>([]);

  /** Identity of the current state, compared against `#savedStamp` for `dirty`. */
  #stamp = $state(0);
  #savedStamp = $state(0);
  /** Monotonic stamp source: a fresh state never collides with a saved one. */
  #seq = 0;
  #ids = 0;
  /** True between `beginDrag()` and `endDrag()`/`cancelDrag()`. */
  #dragging = $state(false);

  /**
   * Derived rather than a counter, so undoing a step badge frees its number
   * again and a reloaded document starts at 1 without extra bookkeeping.
   */
  #nextStep = $derived.by(() => {
    const shapes = this.#doc?.shapes;
    if (!shapes) return 1;
    let max = 0;
    for (const s of shapes) if (s.kind === 'step' && s.n > max) max = s.n;
    return max + 1;
  });

  get doc(): EditorDoc | null {
    return this.#doc;
  }

  get ready(): boolean {
    return this.#doc !== null;
  }

  get src(): HTMLImageElement | ImageBitmap {
    return this.#doc?.src ?? blankSource();
  }

  get width(): number {
    return this.#doc?.width ?? 0;
  }

  get height(): number {
    return this.#doc?.height ?? 0;
  }

  get crop(): Rect | null {
    return this.#doc?.crop ?? null;
  }

  /** The exported area: the crop when set, the whole image otherwise. */
  get bounds(): Rect {
    const doc = this.#doc;
    if (!doc) return { x: 0, y: 0, width: 0, height: 0 };
    return doc.crop
      ? { x: doc.crop.x, y: doc.crop.y, width: doc.crop.width, height: doc.crop.height }
      : { x: 0, y: 0, width: doc.width, height: doc.height };
  }

  get shapes(): Shape[] {
    return this.#doc?.shapes ?? [];
  }

  get selected(): ShapeId | null {
    return this.#doc?.selected ?? null;
  }

  /** Assignable so the store reads as `EditorDoc`; routed through `select()`
   *  so an id that is no longer in the document can never stick. */
  set selected(id: ShapeId | null) {
    this.select(id);
  }

  get selectedShape(): Shape | null {
    const doc = this.#doc;
    if (!doc || !doc.selected) return null;
    return doc.shapes.find((s) => s.id === doc.selected) ?? null;
  }

  get dirty(): boolean {
    return this.#stamp !== this.#savedStamp;
  }

  get canUndo(): boolean {
    return this.#past.length > 0;
  }

  get canRedo(): boolean {
    return this.#future.length > 0;
  }

  get undoLabel(): string | null {
    return this.#past.at(-1)?.label ?? null;
  }

  get redoLabel(): string | null {
    return this.#future.at(-1)?.label ?? null;
  }

  get dragging(): boolean {
    return this.#dragging;
  }

  get nextStepNumber(): number {
    return this.#nextStep;
  }

  /** Replaces the document wholesale: shapes, crop, history and dirty all reset. */
  load(src: HTMLImageElement | ImageBitmap, width: number, height: number): void {
    this.#doc = {
      src,
      width: Math.max(1, Math.round(width)),
      height: Math.max(1, Math.round(height)),
      crop: null,
      shapes: [],
      selected: null
    };
    this.#past = [];
    this.#future = [];
    this.#seq = 0;
    this.#stamp = 0;
    this.#savedStamp = 0;
    this.#ids = 0;
    this.#dragging = false;
  }

  /** Call after a successful save: the current state becomes the clean one. */
  markClean(): void {
    this.#savedStamp = this.#stamp;
  }

  /* ------------------------------------------------------------- mutations */

  /**
   * Appends a shape on top of the stack and returns its id, or `null` if no
   * document is loaded. Selection is left alone — the caller decides whether a
   * freshly drawn shape should also be selected.
   */
  addShape(init: ShapeInit): ShapeId | null {
    const doc = this.#doc;
    if (!doc) return null;
    this.commit(`add ${init.kind}`);
    const id = `s${++this.#ids}`;
    doc.shapes.push({ ...init, id } as Shape);
    return id;
  }

  updateShape(id: ShapeId, patch: ShapePatch): boolean {
    const doc = this.#doc;
    if (!doc) return false;
    const i = doc.shapes.findIndex((s) => s.id === id);
    if (i < 0) return false;
    this.commit('edit');
    // Replace rather than mutate in place: one assignment is one reactive
    // notification, and a partially applied patch can never be observed.
    // `ShapePatch` omits id and kind, so neither can be rewritten from here.
    doc.shapes[i] = { ...doc.shapes[i], ...patch } as Shape;
    return true;
  }

  removeShape(id: ShapeId): boolean {
    const doc = this.#doc;
    if (!doc) return false;
    const i = doc.shapes.findIndex((s) => s.id === id);
    if (i < 0) return false;
    this.commit('delete');
    doc.shapes.splice(i, 1);
    if (doc.selected === id) doc.selected = null;
    return true;
  }

  deleteSelected(): boolean {
    const id = this.selected;
    return id ? this.removeShape(id) : false;
  }

  /** Selection is view state: it is never a mutation and never marks dirty. */
  select(id: ShapeId | null): void {
    const doc = this.#doc;
    if (!doc) return;
    doc.selected = id && doc.shapes.some((s) => s.id === id) ? id : null;
  }

  bringToFront(id: ShapeId): boolean {
    return this.#reorder(id, 'front');
  }

  sendToBack(id: ShapeId): boolean {
    return this.#reorder(id, 'back');
  }

  #reorder(id: ShapeId, where: 'front' | 'back'): boolean {
    const doc = this.#doc;
    if (!doc) return false;
    const i = doc.shapes.findIndex((s) => s.id === id);
    if (i < 0) return false;
    if (where === 'front' && i === doc.shapes.length - 1) return false;
    if (where === 'back' && i === 0) return false;
    this.commit(where === 'front' ? 'bring to front' : 'send to back');
    const [shape] = doc.shapes.splice(i, 1);
    if (where === 'front') doc.shapes.push(shape);
    else doc.shapes.unshift(shape);
    return true;
  }

  /** `rect` is clamped to the image and rounded to whole pixels; `null` clears. */
  setCrop(rect: Rect | null): void {
    const doc = this.#doc;
    if (!doc) return;
    const next = rect ? this.#clampCrop(rect) : null;
    if (!next && !doc.crop) return;
    this.commit(next ? 'crop' : 'reset crop');
    doc.crop = next;
  }

  #clampCrop(rect: Rect): Rect | null {
    const doc = this.#doc;
    if (!doc) return null;
    const x0 = clamp(Math.round(Math.min(rect.x, rect.x + rect.width)), 0, doc.width);
    const y0 = clamp(Math.round(Math.min(rect.y, rect.y + rect.height)), 0, doc.height);
    const x1 = clamp(Math.round(Math.max(rect.x, rect.x + rect.width)), x0, doc.width);
    const y1 = clamp(Math.round(Math.max(rect.y, rect.y + rect.height)), y0, doc.height);
    if (x1 - x0 < 1 || y1 - y0 < 1) return null;
    return { x: x0, y: y0, width: x1 - x0, height: y1 - y0 };
  }

  /* ---------------------------------------------------------- undo history */

  /**
   * Snapshots the current state BEFORE the mutation the caller is about to
   * make. During a drag (see `beginDrag`) it only advances the stamp, so the
   * whole gesture stays one undo entry.
   */
  commit(label: string): void {
    if (!this.#doc) return;
    if (!this.#dragging) {
      this.#past.push(this.#snapshot(label));
      if (this.#past.length > HISTORY_LIMIT) this.#past.shift();
      this.#future = [];
    }
    this.#stamp = ++this.#seq;
  }

  /**
   * Opens a live gesture: one snapshot now, then every `add`/`update` until
   * `endDrag()` folds into it. Dragging an arrow is one undo entry, not one
   * per pointermove (M2 §4.2).
   */
  beginDrag(label: string): void {
    if (!this.#doc || this.#dragging) return;
    this.commit(label);
    this.#dragging = true;
  }

  endDrag(): void {
    this.#dragging = false;
  }

  /** Aborts the gesture and restores the state captured by `beginDrag()`. */
  cancelDrag(): void {
    if (!this.#dragging) return;
    this.#dragging = false;
    const snapshot = this.#past.pop();
    if (snapshot) this.#restore(snapshot);
  }

  undo(): boolean {
    if (this.#dragging) this.cancelDrag();
    const snapshot = this.#past.pop();
    if (!snapshot) return false;
    this.#future.push(this.#snapshot(snapshot.label));
    if (this.#future.length > HISTORY_LIMIT) this.#future.shift();
    this.#restore(snapshot);
    return true;
  }

  redo(): boolean {
    if (this.#dragging) this.cancelDrag();
    const snapshot = this.#future.pop();
    if (!snapshot) return false;
    this.#past.push(this.#snapshot(snapshot.label));
    if (this.#past.length > HISTORY_LIMIT) this.#past.shift();
    this.#restore(snapshot);
    return true;
  }

  #snapshot(label: string): DocSnapshot {
    const doc = this.#doc;
    return {
      label,
      stamp: this.#stamp,
      shapes: doc ? cloneShapes(doc.shapes) : [],
      crop: cloneRect(doc?.crop ?? null),
      selected: doc?.selected ?? null
    };
  }

  /**
   * Clones on the way out too: the stack entry must stay independent of the
   * live document, or the next edit would rewrite history in place.
   */
  #restore(snapshot: DocSnapshot): void {
    const doc = this.#doc;
    if (!doc) return;
    doc.shapes = cloneShapes(snapshot.shapes);
    doc.crop = cloneRect(snapshot.crop);
    doc.selected =
      snapshot.selected && doc.shapes.some((s) => s.id === snapshot.selected)
        ? snapshot.selected
        : null;
    this.#stamp = snapshot.stamp;
  }
}

export function createEditorDoc(): EditorDocStore {
  return new EditorDocStore();
}

/**
 * The editor window edits one document at a time (M2 §1), so a singleton is
 * enough; `createEditorDoc()` stays exported for anything that needs its own.
 * `EditorRoot` and `Stage` both import this and must see the same instance —
 * the stage reads the document the shell loaded.
 */
export const doc = createEditorDoc();
