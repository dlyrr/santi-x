<script lang="ts">
  /**
   * The region overlay — ARCHITECTURE §2 (coordinates), ARCHITECTURE-M2.5 §1–§3.
   *
   * This window is borderless, always-on-top and covers the whole virtual
   * desktop, so the rules that hold it together are safety rules first:
   *
   *  1. It is a STATE MACHINE, not a mount-once script. Since M2.5 the window is
   *     pre-warmed at startup and reused forever, so every capture re-enters
   *     through `overlay://arm` and every exit path — commit, cancel, a failed
   *     decode, an unexpected throw — returns to `idle` and asks Rust to hide.
   *     It never closes the window: closing puts the page load back on the
   *     critical path, which is the thing M2.5 removed.
   *
   *  2. Escape always has somewhere to go (M2.5 §3, extended by M2.7 §3). The
   *     ladder is shape-being-drawn -> back to Region -> clear the selection ->
   *     cancel the capture, and it is live in every phase, including while the
   *     freeze frame is still decoding.
   *
   *  3. One coordinate conversion, `toImage()` / `toCss()`, built on
   *     `freeze.width / window.innerWidth` measured at runtime. `devicePixelRatio`
   *     is deliberately never consulted for geometry: on a mixed-DPI desktop it
   *     describes the monitor the window reports, not the composited image we
   *     were handed. All selection and shape geometry is stored in IMAGE pixels.
   *
   *  4. The freeze frame is decoded through fetch -> blob -> createImageBitmap,
   *     NEVER `new Image()` on an asset URL (M2.5 §0). An asset-protocol origin
   *     taints every canvas it is drawn into and the failure only surfaces later,
   *     at export, as "Tainted canvases may not be exported".
   *
   * Annotation reuses the M2 engine wholesale — `doc.svelte.ts`, `tools.ts`,
   * `render.ts`, `export.ts`. Nothing here draws a shape or re-derives shape
   * geometry itself; `render()` was made pure precisely so this could be its
   * second consumer.
   *
   * M2.7 §2–§3 replaced the phase-gated flow with one rule: THE ACTIVE TOOL
   * DECIDES WHAT A DRAG DOES.
   *
   *  - `REGION_TOOL` (the default the overlay opens in) drags out the selection,
   *    and releasing it COMMITS THE CAPTURE. There is no annotating phase and no
   *    confirm step — releasing the drag is the shutter.
   *  - Any drawing tool drags out that shape, anywhere on the frozen desktop,
   *    with no region required and none involved. Shapes are never clipped: like
   *    the editor's document they live on the full frozen image in image px, and
   *    the crop (if any) is applied once, at export.
   *  - Enter, and the toolbar's Capture button, take the selection when one
   *    exists and otherwise the whole frozen desktop with everything drawn on it.
   *
   * `Settings.annotate_in_overlay` no longer gates any of this. The field still
   * rides along in `ArmPayload` for compatibility; it is deliberately not read.
   */
  import { onMount, tick, untrack } from "svelte";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import {
    assetUrl,
    cancelRegionCapture,
    finishRegionCapture,
    finishRegionCaptureAnnotated,
    onOverlayArm,
    onOverlayDisarm,
    overlayReady,
    type ArmPayload,
    type Rect,
    type UnlistenFn,
    type WindowRect
  } from "$lib/api";
  import { settings } from "$lib/stores/settings.svelte";
  import OverlayToolbar, { OVERLAY_TOOLS, REGION_TOOL } from "$lib/overlay/OverlayToolbar.svelte";
  import { createEditorDoc } from "$lib/editor/doc.svelte";
  import type { EditorDoc, Point, Shape, ShapeInit, TextShape } from "$lib/editor/doc.svelte";
  import { ANNOTATION_FONT_FAMILY, TEXT_LINE_HEIGHT, render } from "$lib/editor/render";
  import { renderToPng } from "$lib/editor/export";
  import {
    HANDLE_SIZE,
    HIT_SLOP,
    constrainAngle,
    handleAt,
    handlesFor,
    hitTest,
    movePatch,
    rectFromPoints,
    resizePatch,
    shapeBounds,
    simplifyPoints,
    toolById,
    toolHitForKey
  } from "$lib/editor/tools";
  import type { HandleId, ToolId, ToolOptions } from "$lib/editor/tools";

  /**
   * idle       nothing loaded, nothing armed, window hidden
   * arming     an arm payload is decoding; Escape already works
   * armed      frame up, waiting for the user — the Region tool selects, a
   *            drawing tool draws, and window auto-detect is live for the former
   * selecting  a region drag is in flight; releasing it commits (M2.7 §2)
   * committing a round trip to Rust is in flight; nothing else may start one
   *
   * M2.5's `annotating` is gone: drawing is no longer a phase the overlay enters
   * after a selection, it is what a drawing tool does in `armed` (M2.7 §3).
   */
  type Phase = "idle" | "arming" | "armed" | "selecting" | "committing";

  /** A window rect already converted from xcap screen coords to image px. */
  type WinHit = { id: number; title: string; appName: string; rect: Rect };

  /** CSS-pixel box, for chrome only. Geometry lives in image px. */
  type CssBox = { left: number; top: number; width: number; height: number };

  type SelHandleId = "nw" | "n" | "ne" | "e" | "se" | "s" | "sw" | "w";

  type Caret = { id: string | null; at: Point; text: string; size: number; color: string };

  type Draw = { kind: "draw"; id: string; start: Point };
  type MoveShape = { kind: "move"; id: string; start: Point; origin: Shape; started: boolean };
  type ResizeShape = {
    kind: "resize";
    id: string;
    handle: HandleId;
    origin: Shape;
    started: boolean;
  };
  type SelDraw = { kind: "sel" };
  type SelResize = { kind: "selresize"; handle: SelHandleId; origin: Rect };
  type Drag = (Draw | MoveShape | ResizeShape | SelDraw | SelResize) & { pointerId: number };

  const LOUPE = 132; // px, the zoomed viewport
  const LOUPE_COORD_H = 20; // px, the coordinate strip under it
  /** Magnification relative to what is already on screen, wheel-adjustable (M2.9 §1). */
  const LOUPE_ZOOM_MIN = 2;
  const LOUPE_ZOOM_MAX = 20;
  const LOUPE_ZOOM_DEFAULT = 8;
  /**
   * `deltaY` that makes one notch, per `WheelEvent.deltaMode` — do NOT assume
   * 100px per notch (M2.9 §1). Chromium reports pixels (mode 0), a notched wheel
   * as ~100 of them and a trackpad as a stream of small ones that the
   * accumulator adds up; Firefox reports lines (mode 1), 3 per notch; mode 2 is
   * pages, one per notch.
   */
  const WHEEL_PIXELS = 100;
  const WHEEL_LINES = 3;
  const WHEEL_PAGES = 1;
  /** Debounce on the settings write: one sweep is dozens of steps, not dozens of saves. */
  const ZOOM_SAVE_MS = 500;
  const MIN_PX = 2; // anything smaller than this in image px is a cancel
  /** CSS px of pointer travel below which a press is a click, not a drag (M2.5 §2). */
  const CLICK_SLOP = 4;

  /**
   * `REGION_TOOL` is this overlay's default and its neutral state (M2.7 §3): a
   * drag makes the selection and commits it, a press on an existing shape moves
   * that shape, and the selection edges are grabbable. Every other tool draws.
   * The bar owns which tools exist (it drops Crop from the M2 table); the key
   * map is built from those same rows so the two can never disagree.
   */
  const OVERLAY_TOOL_IDS = new Set<ToolId>([
    // `M` puts the drawing tool down and returns to Region. Included explicitly
    // as well as via the bar, so it keeps working whatever the bar shows.
    REGION_TOOL,
    ...OVERLAY_TOOLS.map((spec) => spec.id)
  ]);

  const DEFAULT_OPTIONS: ToolOptions = {
    color: "#f2555a",
    stroke: 4,
    fill: false,
    textSize: 28,
    redactMode: "blur",
    redactAmount: 12
  };

  /** The overlay is a second consumer of the M2 engine; it owns its own document
   *  so that re-arming is a total reset with nothing shared to leak across. */
  const doc = createEditorDoc();

  let root: HTMLDivElement | undefined = $state();
  let canvasEl: HTMLCanvasElement | undefined = $state();
  let loupeEl: HTMLCanvasElement | undefined = $state();
  let caretEl: HTMLTextAreaElement | undefined = $state();

  let phase = $state<Phase>("idle");
  let freeze = $state<{ width: number; height: number } | null>(null);

  let viewW = $state(0);
  let viewH = $state(0);

  let cx = $state(0);
  let cy = $state(0);
  let hasPointer = $state(false);

  let sel = $state<Rect | null>(null);
  let windows = $state<WinHit[]>([]);
  let keyboardAdjust = $state(false);
  /** Live loupe magnification. Seeded from `Settings.loupeZoom` on every arm. */
  let loupeZoom = $state(LOUPE_ZOOM_DEFAULT);

  /**
   * Presentation-only mirror of `candidate.rect` (M2.9 §2). It is retained after
   * the candidate goes away so the ring can fade out in place, which is the ONLY
   * reason it exists: the rect a click commits is read from `candidate` itself,
   * so what this is mid-glide never reaches `finishSelection()`.
   */
  let hiliteRect = $state<Rect | null>(null);
  let hiliteOn = $state(false);

  // The overlay opens in Region, so "drag and it's captured" costs no keystroke.
  let tool = $state<ToolId>(REGION_TOOL);
  let opts = $state<ToolOptions>({ ...DEFAULT_OPTIONS });
  let caret = $state<Caret | null>(null);
  let drag = $state<Drag | null>(null);
  let hoverCursor = $state<string | null>(null);

  let badgeW = $state(0);
  let badgeH = $state(0);
  /**
   * The pointer is over the top bar. Since M2.6 §3 the bar is up from `armed`,
   * so it sits on top of desktop the user may want to select: while it is under
   * the pointer the crosshair, the loupe and the window highlight all stand
   * down, and the bar's own handlers keep a press there from opening a
   * selection.
   */
  let overBar = $state(false);

  /* Non-reactive working state: read inside handlers, never rendered. */
  let bitmap: ImageBitmap | null = null;
  /** The arm we are serving. A superseded arm must never paint a stale frame. */
  let armId = 0;
  /** True from the moment an exit is requested until we are back at idle. */
  let closing = false;
  /** Image px: the pinned corner of the selection drag and its moving corner. */
  let selAnchor: Point | null = null;
  let selCur: Point | null = null;
  /** CSS px of the pointerdown that opened the current gesture. */
  let downAt: { x: number; y: number } | null = null;
  let moved = false;
  /** The live pen stroke. Not $state: one flick pushes hundreds of samples. */
  let penPoints: Point[] = [];
  /** Sub-notch wheel travel, so a trackpad's small deltas still add up to a step. */
  let wheelAccum = 0;
  let zoomSaveTimer = 0;

  const clamp = (v: number, lo: number, hi: number) => (v < lo ? lo : v > hi ? hi : v);

  const live = $derived(phase === "armed" || phase === "selecting");

  /* --------------------------------------------------------------- geometry */

  /**
   * THE conversion. `scale` is image px per CSS px, fixed by the contract as
   * `freeze.width / window.innerWidth` and measured at runtime from the image we
   * were actually handed (ARCHITECTURE §2).
   */
  const ppx = $derived(freeze && viewW ? freeze.width / viewW : 1);
  /** Its inverse: CSS px per image px — the display scale the M2 helpers take. */
  const cssScale = $derived(1 / ppx);

  function toImage(clientX: number, clientY: number): Point {
    return { x: clientX * ppx, y: clientY * ppx };
  }

  function toCss(rect: Rect): CssBox {
    return {
      left: rect.x * cssScale,
      top: rect.y * cssScale,
      width: rect.width * cssScale,
      height: rect.height * cssScale
    };
  }

  /** Whole-pixel image rect spanned by two image-px points, clamped to the frame. */
  function snapRect(a: Point, b: Point): Rect {
    const f = freeze;
    const w = f?.width ?? 0;
    const h = f?.height ?? 0;
    const x0 = clamp(Math.floor(Math.min(a.x, b.x)), 0, w);
    const y0 = clamp(Math.floor(Math.min(a.y, b.y)), 0, h);
    const x1 = clamp(Math.ceil(Math.max(a.x, b.x)), x0, w);
    const y1 = clamp(Math.ceil(Math.max(a.y, b.y)), y0, h);
    return { x: x0, y: y0, width: x1 - x0, height: y1 - y0 };
  }

  function unionRect(a: Rect, b: Rect): Rect {
    const x0 = Math.min(a.x, b.x);
    const y0 = Math.min(a.y, b.y);
    const x1 = Math.max(a.x + a.width, b.x + b.width);
    const y1 = Math.max(a.y + a.height, b.y + b.height);
    return { x: x0, y: y0, width: x1 - x0, height: y1 - y0 };
  }

  function intersects(a: Rect, b: Rect): boolean {
    return (
      a.x < b.x + b.width && a.x + a.width > b.x && a.y < b.y + b.height && a.y + a.height > b.y
    );
  }

  /**
   * Drawing is NOT confined to the selection (M2.7 §3): a tool draws anywhere on
   * the frozen desktop, with no region required, so shapes live on the full
   * image exactly as they do in the editor's document. The only clamp left is
   * the image itself, because nothing outside it can ever be rendered.
   */
  function clampDraw(p: Point): Point {
    const f = freeze;
    if (!f) return p;
    return { x: clamp(p.x, 0, f.width), y: clamp(p.y, 0, f.height) };
  }

  const selBox = $derived<CssBox | null>(sel ? toCss(sel) : null);

  /* -------------------------------------------------------- window detection */

  const cursorPx = $derived.by<Point | null>(() => {
    const f = freeze;
    if (!f) return null;
    return {
      x: clamp(Math.floor(cx * ppx), 0, f.width - 1),
      y: clamp(Math.floor(cy * ppx), 0, f.height - 1)
    };
  });

  /**
   * The topmost window under the cursor (M2.5 §2). Only a candidate while no
   * selection exists and nothing is being dragged: a real drag overrides to
   * freehand and the highlight goes with it. The list arrives sorted
   * topmost-first and was converted to image px once, on arm.
   *
   * M2.7 §3 adds the tool test: auto-detect belongs to Region alone, so hovering
   * cannot light a window up while the user is drawing an arrow across one.
   */
  const candidate = $derived.by<WinHit | null>(() => {
    if (tool !== REGION_TOOL) return null;
    if (phase !== "armed" || !hasPointer || overBar || sel || windows.length === 0) return null;
    const p = cursorPx;
    if (!p) return null;
    for (const w of windows) {
      const r = w.rect;
      if (p.x >= r.x && p.x < r.x + r.width && p.y >= r.y && p.y < r.y + r.height) return w;
    }
    return null;
  });

  /** The undimmed area: the selection, or the window about to be captured. */
  const hole = $derived<Rect | null>(sel ?? candidate?.rect ?? null);
  const holeBox = $derived<CssBox | null>(hole ? toCss(hole) : null);

  /**
   * The highlight glides between windows instead of snapping (M2.9 §2).
   *
   * Only while the hole IS a candidate and no gesture is in flight: a region
   * drag must track the pointer exactly, and an animated selection edge would be
   * a lie about where the crop is going to land. It is also purely presentation
   * — `finishSelection()` takes `candidate.rect`, not what is on screen — so a
   * click mid-glide still captures the window's true bounds.
   */
  const glide = $derived(sel === null && candidate !== null && drag === null);

  /**
   * Feed the ring. Splitting it from `candidate` is what buys the fade-out: the
   * geometry is kept after the pointer leaves every window, so the ring dims in
   * place rather than vanishing with the element.
   */
  $effect(() => {
    const rect = candidate?.rect ?? null;
    const dragging = drag !== null;
    untrack(() => {
      if (rect) {
        hiliteRect = { ...rect };
        hiliteOn = true;
        return;
      }
      hiliteOn = false;
      // A drag has taken over: drop the ring outright rather than leave it
      // fading over the selection the user is now dragging.
      if (dragging) hiliteRect = null;
    });
  });

  const hiliteBox = $derived<CssBox | null>(hiliteRect ? toCss(hiliteRect) : null);

  /* ------------------------------------------------------------ selection UI */

  const SEL_HANDLES: readonly { id: SelHandleId; fx: number; fy: number; cursor: string }[] = [
    { id: "nw", fx: 0, fy: 0, cursor: "nwse-resize" },
    { id: "n", fx: 0.5, fy: 0, cursor: "ns-resize" },
    { id: "ne", fx: 1, fy: 0, cursor: "nesw-resize" },
    { id: "e", fx: 1, fy: 0.5, cursor: "ew-resize" },
    { id: "se", fx: 1, fy: 1, cursor: "nwse-resize" },
    { id: "s", fx: 0.5, fy: 1, cursor: "ns-resize" },
    { id: "sw", fx: 0, fy: 1, cursor: "nesw-resize" },
    { id: "w", fx: 0, fy: 0.5, cursor: "ew-resize" }
  ];

  /** Grab zone for a selection edge, in image px — constant on screen. */
  const selReach = $derived((HANDLE_SIZE / 2 + HIT_SLOP) * ppx);

  /**
   * A selection that is still on screen rather than already captured is the
   * keyboard-adjust case (`nudgeSelection`), where Enter is the shutter — so its
   * edges stay grabbable, but only for the tool that owns the region.
   */
  function selHandleAt(p: Point): { id: SelHandleId; cursor: string } | null {
    const r = sel;
    if (!r || tool !== REGION_TOOL) return null;
    for (const h of SEL_HANDLES) {
      const hx = r.x + r.width * h.fx;
      const hy = r.y + r.height * h.fy;
      if (Math.abs(p.x - hx) <= selReach && Math.abs(p.y - hy) <= selReach) return h;
    }
    return null;
  }

  function resizeSel(origin: Rect, handle: SelHandleId, p: Point): Rect {
    let x0 = origin.x;
    let y0 = origin.y;
    let x1 = origin.x + origin.width;
    let y1 = origin.y + origin.height;
    if (handle.includes("w")) x0 = p.x;
    if (handle.includes("e")) x1 = p.x;
    if (handle.includes("n")) y0 = p.y;
    if (handle.includes("s")) y1 = p.y;
    return snapRect({ x: x0, y: y0 }, { x: x1, y: y1 });
  }

  const selectedShape = $derived(doc.shapes.find((s) => s.id === doc.selected) ?? null);

  const selectedBox = $derived<CssBox | null>(
    live && tool === REGION_TOOL && selectedShape && drag?.kind !== "draw"
      ? toCss(shapeBounds(selectedShape))
      : null
  );

  const shapeHandles = $derived(
    selectedShape && selectedBox
      ? handlesFor(selectedShape).map((h) => ({
          id: h.id,
          left: h.at.x * cssScale,
          top: h.at.y * cssScale
        }))
      : []
  );

  /* The crosshair, the loupe and the window highlight are all region-finding
     aids. With a drawing tool active there is no region to find, so they stand
     down and leave the frozen desktop to the drawing. */
  const showGuides = $derived(
    live && tool === REGION_TOOL && hasPointer && !overBar && !sel && !candidate
  );
  const showLoupe = $derived(
    live && hasPointer && !overBar && !keyboardAdjust && !caret && tool === REGION_TOOL
  );
  const showBadge = $derived(live && sel !== null);

  const cursor = $derived.by(() => {
    if (!live) return "default";
    if (drag?.kind === "move") return "grabbing";
    if (tool !== REGION_TOOL) return toolById(tool).cursor;
    // Region is a crosshair unless the pointer is over something grabbable — a
    // selection edge, a shape, one of its handles.
    return hoverCursor ?? "crosshair";
  });

  const badgePos = $derived.by(() => {
    const box = holeBox;
    if (!box) return { left: 0, top: 0 };
    const gap = 8;
    let top = box.top - badgeH - gap;
    if (top < gap) top = box.top + box.height + gap;
    if (top + badgeH + gap > viewH) top = Math.max(gap, box.top + gap);
    const left = clamp(box.left, gap, Math.max(gap, viewW - badgeW - gap));
    return { left, top };
  });

  /**
   * What a commit would take, or null if there is nothing worth taking.
   *
   * The selection when one exists; otherwise the whole frozen desktop, but only
   * once something has been drawn on it (M2.7 §3) — that is the "draw a few
   * arrows and capture the lot" path. With neither a region nor a shape there is
   * nothing to distinguish this from a fullscreen capture the user did not ask
   * for, so Enter and the Capture button stay inert instead of firing a commit
   * that would only cancel.
   */
  function commitTarget(): Rect | null {
    const r = sel;
    if (r && r.width >= MIN_PX && r.height >= MIN_PX) return r;
    const f = freeze;
    if (!f || doc.shapes.length === 0) return null;
    return { x: 0, y: 0, width: f.width, height: f.height };
  }

  /**
   * The toolbar is pinned to the top edge and no longer flips or follows the
   * selection (M2.6 §3), so it is up before a region exists — and the Capture
   * button has to say so.
   */
  const canCommit = $derived.by(() => commitTarget() !== null);

  const caretPos = $derived(
    caret ? { left: caret.at.x * cssScale, top: caret.at.y * cssScale } : { left: 0, top: 0 }
  );

  /* -------------------------------------------------------------- lifecycle */

  function looksConverted(s: string): boolean {
    // convertFileSrc output is a real URL (http://asset.localhost/…);
    // a Windows path (C:\…) never contains "://".
    return /^[a-z][a-z0-9+.-]*:\/\//i.test(s);
  }

  /**
   * Decoded through fetch + `createImageBitmap`, NOT `new Image()` — see the
   * header and M2.5 §0. `freeze.png` is rewritten at the same path on every
   * capture, so the URL carries the arm id as a version or the webview would
   * happily hand back the *previous* desktop out of its cache.
   */
  async function decodeFreeze(src: string, version: number): Promise<ImageBitmap> {
    const base = looksConverted(src) ? src : assetUrl(src);
    // The asset protocol resolves the file from the URL *path* only, so the
    // extra query is invisible to Rust and to the scope check.
    const url = `${base}${base.includes("?") ? "&" : "?"}v=${version}`;
    const res = await fetch(url);
    if (!res.ok) throw new Error(`The freeze frame could not be read (${res.status}).`);
    return createImageBitmap(await res.blob());
  }

  /** Everything a previous capture could have left behind. */
  function reset(): void {
    drag = null;
    caret = null;
    // Every capture starts in Region: the tool a previous one ended on must not
    // decide what the next drag does.
    tool = REGION_TOOL;
    sel = null;
    windows = [];
    keyboardAdjust = false;
    // Unmounted, not just hidden: a ring left over from the last capture would
    // glide in from the window it was on then.
    hiliteRect = null;
    hiliteOn = false;
    hasPointer = false;
    overBar = false;
    hoverCursor = null;
    selAnchor = null;
    selCur = null;
    downAt = null;
    moved = false;
    penPoints = [];
    optionDrag = false;
    paintedSel = null;
    if (dirtyFrame) {
      cancelAnimationFrame(dirtyFrame);
      dirtyFrame = 0;
    }
    dirtyRegion = null;
    dirtyAll = false;
    pending = null;
    if (moveFrame) {
      cancelAnimationFrame(moveFrame);
      moveFrame = 0;
    }
  }

  function toIdle(): void {
    reset();
    phase = "idle";
    freeze = null;
    bitmap?.close();
    bitmap = null;
    closing = false;
    // Retiring the arm id is what orphans a decode still in flight. Escape
    // during `arming` cancels the capture while `decodeFreeze` is still
    // running, and that promise goes on to check `armId === id` before it
    // installs its bitmap: leaving the id set would let a capture the user has
    // already abandoned come back as `armed`, holding a virtual-desktop-sized
    // ImageBitmap and acknowledging an arm Rust has long since dropped.
    armId = 0;
  }

  /**
   * Rust shows the window only after `overlay_ready`, so everything up to that
   * call is invisible — which is exactly the budget for making the first painted
   * frame complete (M2.5 §1).
   */
  async function arm(payload: ArmPayload): Promise<void> {
    const id = payload.armId;

    /*
     * Serving an arm must be idempotent. Rust re-emits the *same* payload every
     * 100 ms until `overlay_ready` lands (overlay.rs `await_ready`), because on
     * the cold fallback path the first emit can beat this listener into
     * existence. A freeze frame that takes longer than one poll to decode — a
     * multi-monitor 4K desktop is not a rare case — is therefore delivered
     * twice, and taking it twice would restart the decode, strand the first
     * bitmap unclosed, and, if the second pass lands after Rust has already
     * shown the window, reload the document and yank the phase back to `armed`
     * underneath a user who is mid-selection.
     *
     * So: a repeat of the id we are already on is either ignored (still
     * decoding — the pass in flight will acknowledge for both) or simply
     * re-acknowledged, which is also the recovery if our first answer was lost.
     */
    if (id !== 0 && id === armId && phase !== "idle") {
      if (phase !== "arming" && phase !== "committing" && !closing) {
        try {
          await overlayReady(id);
        } catch {
          /* the next poll asks again */
        }
      }
      return;
    }

    armId = id;
    closing = false;
    reset();
    // The magnification the user last settled on (M2.9 §1). Skipped while a save
    // is still pending, because until it lands `settings.current` is the value
    // they just scrolled away from and re-reading it would undo their choice.
    if (!zoomSaveTimer) loupeZoom = clampZoom(settings.current?.loupeZoom ?? LOUPE_ZOOM_DEFAULT);
    bitmap?.close();
    bitmap = null;
    phase = "arming";

    let image: ImageBitmap;
    try {
      image = await decodeFreeze(payload.src, id);
    } catch {
      // A frame we cannot decode means nothing can be selected. Cancelling is
      // the only safe answer: sitting here would leave a black sheet over the
      // user's desktop with no way out.
      if (armId === id) await cancel();
      return;
    }

    if (armId !== id) {
      image.close();
      return;
    }

    bitmap = image;
    freeze = { width: payload.width, height: payload.height };
    // `payload.annotate` is deliberately not read: M2.7 §2 retired
    // `annotate_in_overlay` as a gate. Releasing a region drag captures, full
    // stop; drawing is what a drawing tool does, not a mode a setting unlocks.
    // Converted ONCE, here — never per frame (M2.5 §2).
    windows = payload.windows.map((w: WindowRect) => ({
      id: w.id,
      title: w.title,
      appName: w.appName,
      rect: {
        x: w.x - payload.originX,
        y: w.y - payload.originY,
        width: w.width,
        height: w.height
      }
    }));
    doc.load(image, payload.width, payload.height);
    phase = "armed";

    // Let Svelte mount the canvas, then paint synchronously: the round trip below
    // is what buys a complete first frame, so it must not be spent on a rAF.
    await tick();
    paint(true);

    if (armId !== id) return;
    try {
      await overlayReady(id);
    } catch {
      if (armId === id) await cancel();
      return;
    }
    root?.focus();
  }

  /** Last resort only. The window is pre-warmed and reused: never close it. */
  async function hideSelf(): Promise<void> {
    try {
      await getCurrentWindow().hide();
    } catch {
      /* nothing left to try */
    }
  }

  /**
   * Get this window off the screen, whatever Rust thinks is going on.
   *
   * Used by the Escape ladder from `idle`, where there is nothing to cancel and
   * therefore nothing the normal path would do — but where being visible is the
   * worst case of all, because `idle` paints nothing and the window is
   * borderless, undecorated and always-on-top. `cancel_region_capture` first
   * because it also drops any freeze frame Rust is still holding and brings the
   * main window back; a bare hide if even that fails.
   */
  async function abandon(): Promise<void> {
    try {
      await cancelRegionCapture();
    } catch {
      await hideSelf();
    }
  }

  async function cancel(): Promise<void> {
    if (closing || phase === "idle") return;
    // The next hotkey press can land while this round trip is still open. It
    // re-arms us with a fresh id, and tearing that down afterwards would blank a
    // capture the user is already looking at.
    const id = armId;
    closing = true;
    phase = "committing";
    try {
      await cancelRegionCapture();
    } catch {
      await hideSelf();
    }
    if (armId === id) toIdle();
  }

  /** Shapes that survive the crop — the only ones worth an annotated round trip. */
  function shapesInside(rect: Rect): boolean {
    return doc.shapes.some((s) => intersects(shapeBounds(s), rect));
  }

  /**
   * Take the capture. Called on release of a region drag (M2.7 §2), on a window
   * click, from Enter, and from the toolbar's Capture button — the four are the
   * same action and must not drift apart.
   *
   * Two paths, and picking the right one matters (M2.5 §3): with nothing drawn
   * inside the rect Rust crops the already-decoded image natively, and a 4K
   * capture never touches the IPC. Only real annotations pay for base64.
   */
  async function commit(): Promise<void> {
    if (closing || !live) return;
    // Before the target is resolved: a caret with text in it becomes a shape,
    // and that shape is the difference between "nothing to capture" and a
    // whole-desktop commit.
    if (caret) commitCaret();

    const rect = commitTarget();
    if (!rect) {
      void cancel();
      return;
    }

    const annotated = shapesInside(rect);
    const id = armId;
    closing = true;
    phase = "committing";
    try {
      if (annotated) {
        // The crop is only applied here so it never lands on the undo stack: the
        // crop-aware export does the cropping (M2.5 §3).
        doc.setCrop(rect);
        await finishRegionCaptureAnnotated(renderToPng(doc));
      } else {
        await finishRegionCapture(rect);
      }
    } catch {
      try {
        await cancelRegionCapture();
      } catch {
        await hideSelf();
      }
    }
    // Same guard as `cancel()`: a capture armed while this was in flight owns
    // the window now.
    if (armId === id) toIdle();
  }

  /**
   * Rust hid the window on a path we did not drive — a second hotkey press
   * superseding this arm, or a capture that failed after arming. Release the
   * freeze frame and go back to idle rather than sitting armed, invisible, on a
   * virtual-desktop-sized bitmap until the next capture.
   *
   * A commit or cancel already in flight owns its own teardown and ends at
   * `toIdle()` regardless, so this must not pull the ground out from under it.
   * `arming` is excluded for the same reason from the other side: the window is
   * not on screen yet and the decode in flight would go on to acknowledge an arm
   * we had just torn down. Nothing is lost by skipping it — a superseded arm
   * always arrives as a fresh `overlay://arm`, which resets everything anyway.
   */
  function disarm(): void {
    if (closing || phase === "idle" || phase === "arming") return;
    toIdle();
  }

  onMount(() => {
    const stops: UnlistenFn[] = [];
    void onOverlayArm((payload) => {
      void arm(payload);
    }).then((stop) => stops.push(stop));
    void onOverlayDisarm(disarm).then((stop) => stops.push(stop));

    // Off the critical path: the window is pre-warmed at startup, so the tool
    // defaults are resolved long before the first hotkey press.
    void settings.load();

    return () => {
      for (const stop of stops) stop();
      if (dirtyFrame) cancelAnimationFrame(dirtyFrame);
      if (moveFrame) cancelAnimationFrame(moveFrame);
      if (zoomSaveTimer) clearTimeout(zoomSaveTimer);
      bitmap?.close();
    };
  });

  // Seed the annotation defaults once. A later `settings://changed` must not
  // stomp on a colour the user has since picked in the overlay toolbar.
  let seeded = false;
  $effect(() => {
    const current = settings.current;
    if (!current || seeded) return;
    seeded = true;
    opts.color = current.editorDefaultColor;
    opts.stroke = current.editorDefaultStroke;
  });

  /* --------------------------------------------------------------- painting */

  /** Accumulated dirty region in image px; `dirtyAll` wins over it. */
  let dirtyRegion: Rect | null = null;
  let dirtyAll = false;
  let dirtyFrame = 0;
  /** The region shapes were last painted into — repaint covers old *and* new. */
  let paintedSel: Rect | null = null;

  function invalidate(region: Rect | null): void {
    if (!region) dirtyAll = true;
    else dirtyRegion = dirtyRegion ? unionRect(dirtyRegion, region) : region;
    if (!dirtyFrame) dirtyFrame = requestAnimationFrame(() => paint(false));
  }

  /**
   * Repaints the freeze frame and the shapes through the M2 `render()` — the
   * same function the export runs, so the canvas cannot drift from the file.
   * The backing store is the freeze frame's own size, so this is 1:1: no
   * resampling, and shape coordinates are canvas coordinates.
   */
  function paint(full: boolean): void {
    // Safe on an already-fired handle, and it keeps a synchronous full paint
    // from leaving a scheduled partial one behind.
    if (dirtyFrame) cancelAnimationFrame(dirtyFrame);
    dirtyFrame = 0;
    const canvas = canvasEl;
    const f = freeze;
    if (!canvas || !f || !bitmap || phase === "idle") return;

    const region = full || dirtyAll ? { x: 0, y: 0, width: f.width, height: f.height } : dirtyRegion;
    dirtyAll = false;
    dirtyRegion = null;
    if (!region || region.width < 1 || region.height < 1) return;

    if (canvas.width !== f.width) canvas.width = f.width;
    if (canvas.height !== f.height) canvas.height = f.height;

    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.save();
    ctx.beginPath();
    ctx.rect(region.x, region.y, region.width, region.height);
    ctx.clip();
    ctx.clearRect(region.x, region.y, region.width, region.height);
    // `crop: false`: this canvas is the whole desktop. The selection is a hole in
    // the DOM scrim above it, and the crop is applied at export time instead.
    render(ctx, docFor(), { scale: 1, crop: false });
    ctx.restore();
  }

  /**
   * The document as the overlay should draw it: the shape behind an open caret is
   * withheld because the live <textarea> is standing in for it.
   */
  function docFor(): EditorDoc {
    const id = caret?.id;
    if (!id) return doc;
    return {
      src: doc.src,
      width: doc.width,
      height: doc.height,
      crop: doc.crop,
      shapes: doc.shapes.filter((s) => s.id !== id),
      selected: doc.selected
    };
  }

  // One repaint per frame, whatever changed. The deep read picks up an in-place
  // edit (a colour swatch restyling the selected shape) a reference compare would
  // miss; the union with the previous selection covers a shrinking resize, which
  // would otherwise strand shape pixels outside the new hole.
  $effect(() => {
    void $state.snapshot(doc.shapes);
    void caret?.id;
    const region = sel;
    untrack(() => {
      if (phase === "idle" || phase === "arming") return;
      invalidate(region && paintedSel ? unionRect(region, paintedSel) : null);
      paintedSel = region ? { ...region } : null;
    });
  });

  /* --------------------------------------------------------------- magnifier */

  /**
   * Clamp to 2x-20x at BOTH ends (M2.9 §1).
   *
   * The finite test has to come before the clamp rather than ride on `||`:
   * `Math.round(0) || LOUPE_ZOOM_DEFAULT` is 8, so a sweep that landed exactly
   * on zero — two notches down from 2x, which one flick of a trackpad does —
   * snapped the loupe to 8x instead of holding the floor. Only a value that is
   * not a number at all falls back to the default.
   */
  const clampZoom = (z: number) =>
    Number.isFinite(z) ? clamp(Math.round(z), LOUPE_ZOOM_MIN, LOUPE_ZOOM_MAX) : LOUPE_ZOOM_DEFAULT;

  /**
   * Persist through the normal settings path (M2.9 §1) — no bespoke command —
   * but not on every step of a sweep: `save_settings` writes the file and
   * re-registers the hotkeys (ARCHITECTURE §4), and one flick of the wheel is a
   * dozen steps.
   */
  function scheduleZoomSave(z: number): void {
    if (zoomSaveTimer) clearTimeout(zoomSaveTimer);
    zoomSaveTimer = window.setTimeout(() => {
      zoomSaveTimer = 0;
      const current = settings.current;
      if (!current || current.loupeZoom === z) return;
      // A failed write only costs the persistence; the live value stands.
      void settings.update({ loupeZoom: z });
    }, ZOOM_SAVE_MS);
  }

  function setLoupeZoom(next: number): void {
    const z = clampZoom(next);
    if (z === loupeZoom) return;
    loupeZoom = z;
    scheduleZoomSave(z);
  }

  /** Wheel travel expressed in notches, whatever units the device reports in. */
  function wheelNotches(e: WheelEvent): number {
    const unit = e.deltaMode === 1 ? WHEEL_LINES : e.deltaMode === 2 ? WHEEL_PAGES : WHEEL_PIXELS;
    return e.deltaY / unit;
  }

  function onWheel(e: WheelEvent): void {
    // Consumed unconditionally: this window is a fixed, non-scrolling surface
    // over the user's desktop, and a scroll that reached the document would show
    // up as a seam (M2.9 §1).
    e.preventDefault();
    // Only live while the loupe is, so it cannot fight a future zoom or pan
    // gesture belonging to a drawing tool.
    if (!showLoupe) return;

    const notches = wheelNotches(e);
    if (!Number.isFinite(notches) || notches === 0) return;
    // A reversal answers on the first event instead of first unwinding whatever
    // the previous direction left in the accumulator.
    if (Math.sign(notches) !== Math.sign(wheelAccum)) wheelAccum = 0;
    wheelAccum += notches;
    const steps = Math.trunc(wheelAccum);
    if (steps === 0) return;
    wheelAccum -= steps;
    // Scrolling up magnifies, as it does everywhere else.
    setLoupeZoom(loupeZoom - steps);
  }

  const loupePos = $derived.by(() => {
    const gap = 22;
    const boxH = LOUPE + LOUPE_COORD_H;
    let left = cx + gap;
    let top = cy + gap;
    if (left + LOUPE > viewW - 8) left = cx - gap - LOUPE;
    if (top + boxH > viewH - 8) top = cy - gap - boxH;
    const cell = loupeZoom * cssScale; // one image pixel, in loupe CSS px
    const ix = cursorPx?.x ?? 0;
    const iy = cursorPx?.y ?? 0;
    return {
      left: clamp(left, 8, Math.max(8, viewW - LOUPE - 8)),
      top: clamp(top, 8, Math.max(8, viewH - boxH - 8)),
      // Snap the highlight to the real pixel boundary rather than the cursor,
      // otherwise the "current pixel" straddles two of them at odd scales.
      cellLeft: LOUPE / 2 + (ix * cssScale - cx) * loupeZoom,
      cellTop: LOUPE / 2 + (iy * cssScale - cy) * loupeZoom,
      cell
    };
  });

  /**
   * Drawn from the decoded bitmap rather than a second `<img>` of the same asset:
   * one decode, no 30k-px-wide raster asked of the compositor, and no second
   * chance to load a tainted origin.
   */
  function paintLoupe(): void {
    const canvas = loupeEl;
    const f = freeze;
    if (!canvas || !f || !bitmap) return;
    // devicePixelRatio is display resolution, not geometry — the one place it is
    // legitimate here, exactly as the M2 stage uses it.
    const dpr = window.devicePixelRatio || 1;
    const bw = Math.max(1, Math.round(LOUPE * dpr));
    if (canvas.width !== bw) canvas.width = bw;
    if (canvas.height !== bw) canvas.height = bw;

    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    // Image px covered by the loupe: it magnifies `loupeZoom` times whatever is
    // already on screen, so the span follows the runtime scale.
    const span = (LOUPE * ppx) / loupeZoom;
    const sx = cx * ppx - span / 2;
    const sy = cy * ppx - span / 2;

    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, bw, bw);
    ctx.imageSmoothingEnabled = false;
    // Source coords may run off the frame near an edge; the browser clips the
    // draw and leaves the rest transparent, which reads as "nothing there".
    ctx.drawImage(bitmap, sx, sy, span, span, 0, 0, bw, bw);
  }

  // Reads `loupeEl` so the very first frame after the loupe mounts is drawn, and
  // the cursor so it tracks. Both are already coalesced into one rAF upstream,
  // so this runs at most once per animation frame.
  $effect(() => {
    void cx;
    void cy;
    void ppx;
    void loupeZoom;
    if (loupeEl && bitmap) paintLoupe();
  });

  /* ---------------------------------------------------------------- pointer */

  let pending: { x: number; y: number; shift: boolean } | null = null;
  let moveFrame = 0;

  function flushMove(): void {
    moveFrame = 0;
    const p = pending;
    pending = null;
    if (!p || !live) return;

    cx = p.x;
    cy = p.y;
    hasPointer = true;

    const image = toImage(p.x, p.y);
    const d = drag;
    if (!d) {
      updateHover(image);
      return;
    }

    switch (d.kind) {
      case "sel":
        selCur = image;
        // Below the slop the press is still a window click, and a selection here
        // would silently clear the candidate the release is about to take.
        if (moved && selAnchor) sel = snapRect(selAnchor, image);
        break;
      case "selresize":
        sel = resizeSel(d.origin, d.handle, image);
        break;
      case "draw":
        updateDraw(d, image, p.shift);
        break;
      case "move": {
        if (!d.started) {
          d.started = true;
          doc.beginDrag("move");
        }
        doc.updateShape(d.id, movePatch(d.origin, image.x - d.start.x, image.y - d.start.y));
        break;
      }
      case "resize": {
        if (!d.started) {
          d.started = true;
          doc.beginDrag("resize");
        }
        doc.updateShape(d.id, resizePatch(d.origin, d.handle, clampDraw(image)));
        break;
      }
    }
  }

  function updateHover(p: Point): void {
    // Only Region grabs things; a drawing tool draws over whatever is under it.
    if (!live || tool !== REGION_TOOL) {
      hoverCursor = null;
      return;
    }
    const handle = selHandleAt(p);
    if (handle) {
      hoverCursor = handle.cursor;
      return;
    }
    const shape = selectedShape;
    const shapeHandle = shape ? handleAt(shape, p, cssScale) : null;
    hoverCursor = shapeHandle ? shapeHandle.cursor : hitTest(doc, p, cssScale) ? "move" : null;
  }

  function initialShape(at: Point): ShapeInit | null {
    const rect: Rect = { x: at.x, y: at.y, width: 0, height: 0 };
    switch (tool) {
      case "arrow":
        return { kind: "arrow", color: opts.color, stroke: opts.stroke, from: at, to: at };
      case "line":
        return { kind: "line", color: opts.color, stroke: opts.stroke, from: at, to: at };
      case "rect":
        return { kind: "rect", color: opts.color, stroke: opts.stroke, rect, fill: opts.fill };
      case "ellipse":
        return { kind: "ellipse", color: opts.color, stroke: opts.stroke, rect, fill: opts.fill };
      case "pen":
        return { kind: "pen", color: opts.color, stroke: opts.stroke, points: [at] };
      case "highlight":
        return { kind: "highlight", color: opts.color, stroke: opts.stroke, rect };
      case "redact":
        return {
          kind: "redact",
          color: opts.color,
          stroke: opts.stroke,
          rect,
          mode: opts.redactMode,
          amount: opts.redactAmount
        };
      case "step":
        return { kind: "step", color: opts.color, stroke: opts.stroke, at, n: doc.nextStepNumber };
      default:
        return null;
    }
  }

  function startDraw(e: PointerEvent, at: Point): void {
    const init = initialShape(at);
    if (!init) return;
    // One gesture, one undo entry (M2 §4.2).
    doc.beginDrag(`add ${init.kind}`);
    const id = doc.addShape(init);
    if (!id) {
      doc.cancelDrag();
      return;
    }
    doc.select(null);
    penPoints = [at];
    drag = { kind: "draw", pointerId: e.pointerId, id, start: at };
  }

  function updateDraw(d: Draw, image: Point, shift: boolean): void {
    const p = clampDraw(image);
    switch (tool) {
      case "arrow":
      case "line":
        doc.updateShape(d.id, { to: shift ? constrainAngle(d.start, p) : p });
        break;
      case "rect":
      case "ellipse":
      case "highlight":
      case "redact":
        doc.updateShape(d.id, { rect: rectFromPoints(d.start, p, shift) });
        break;
      case "pen":
        doc.updateShape(d.id, { points: penPoints.slice() });
        break;
      case "step":
        doc.updateShape(d.id, { at: p });
        break;
    }
  }

  function drawnExtent(shape: Shape): number {
    switch (shape.kind) {
      case "arrow":
      case "line":
        return Math.hypot(shape.to.x - shape.from.x, shape.to.y - shape.from.y);
      case "rect":
      case "ellipse":
      case "highlight":
      case "redact":
        return Math.max(Math.abs(shape.rect.width), Math.abs(shape.rect.height));
      default:
        // Pen and step are placed, not spanned: a tap is a legitimate result.
        return Number.POSITIVE_INFINITY;
    }
  }

  function finishDraw(d: Draw): void {
    const shape = doc.shapes.find((s) => s.id === d.id);
    if (!shape) {
      doc.endDrag();
      return;
    }
    if (shape.kind === "pen") {
      doc.updateShape(d.id, { points: simplifyPoints(penPoints) });
      doc.endDrag();
      return;
    }
    if (drawnExtent(shape) * cssScale < CLICK_SLOP) {
      doc.cancelDrag();
      return;
    }
    doc.endDrag();
  }

  /**
   * Is there something already drawn under the pointer? Region still edits what
   * exists — a press on a shape (or on a handle of the selected one) moves it
   * rather than opening a selection, which is the M2.5 Select behaviour kept
   * alive now that Region has taken over the neutral tool's slot. With an empty
   * document this is a no-op scan of an empty array, so the common path — drag a
   * region on a bare desktop — is untouched.
   */
  function shapeUnder(p: Point): boolean {
    const current = selectedShape;
    if (current && handleAt(current, p, cssScale)) return true;
    return hitTest(doc, p, cssScale) !== null;
  }

  function startSelectShape(e: PointerEvent, p: Point): void {
    const current = selectedShape;
    const handle = current ? handleAt(current, p, cssScale) : null;
    if (current && handle) {
      drag = {
        kind: "resize",
        pointerId: e.pointerId,
        id: current.id,
        handle: handle.id,
        // Snapshot: resizePatch works off the pre-drag geometry, or dragging a
        // handle past the opposite edge would flip on every frame.
        origin: $state.snapshot(current) as Shape,
        started: false
      };
      return;
    }
    const hit = hitTest(doc, p, cssScale);
    doc.select(hit ? hit.id : null);
    if (!hit) return;
    drag = {
      kind: "move",
      pointerId: e.pointerId,
      id: hit.id,
      start: p,
      origin: $state.snapshot(hit) as Shape,
      started: false
    };
  }

  /** The toolbar and the text caret own their own pointer events. */
  function isChrome(target: EventTarget | null): boolean {
    return target instanceof Element && target.closest("[data-chrome]") !== null;
  }

  function onPointerDown(e: PointerEvent): void {
    if (!live || isChrome(e.target)) return;
    if (e.button === 2) {
      e.preventDefault();
      void cancel();
      return;
    }
    if (e.button !== 0) return;
    e.preventDefault();

    if (caret) commitCaret();
    root?.focus();
    keyboardAdjust = false;
    cx = e.clientX;
    cy = e.clientY;
    hasPointer = true;
    downAt = { x: e.clientX, y: e.clientY };
    moved = false;

    const p = toImage(e.clientX, e.clientY);

    // THE MODEL (M2.7 §3): the active tool decides what this drag does.
    if (tool !== REGION_TOOL) {
      if (tool === "text") {
        openCaret(clampDraw(p), null);
        return;
      }
      startDraw(e, clampDraw(p));
    } else {
      const handle = selHandleAt(p);
      if (handle && sel) {
        drag = { kind: "selresize", pointerId: e.pointerId, handle: handle.id, origin: { ...sel } };
      } else if (shapeUnder(p)) {
        startSelectShape(e, p);
      } else {
        // A press alone is not yet a selection: leaving `sel` null keeps the
        // window candidate highlighted until the pointer actually moves
        // (M2.5 §2), and clearing a selection left over from a keyboard adjust
        // is what brings that candidate back.
        doc.select(null);
        sel = null;
        selAnchor = p;
        selCur = p;
        drag = { kind: "sel", pointerId: e.pointerId };
      }
    }

    if (drag) {
      try {
        root?.setPointerCapture(e.pointerId);
      } catch {
        /* a pointer that vanished mid-press cannot be captured */
      }
    }
  }

  function onPointerMove(e: PointerEvent): void {
    if (!live) return;
    if (downAt && !moved) {
      moved =
        Math.abs(e.clientX - downAt.x) >= CLICK_SLOP || Math.abs(e.clientY - downAt.y) >= CLICK_SLOP;
      if (moved && drag?.kind === "sel") phase = "selecting";
    }
    // The pen samples synchronously: the browser batches several positions into
    // one pointermove and dropping them is what makes a fast stroke polygonal.
    if (drag?.kind === "draw" && tool === "pen") {
      const batch = typeof e.getCoalescedEvents === "function" ? e.getCoalescedEvents() : [];
      for (const s of batch.length > 0 ? batch : [e]) {
        penPoints.push(clampDraw(toImage(s.clientX, s.clientY)));
      }
    }
    pending = { x: e.clientX, y: e.clientY, shift: e.shiftKey };
    if (!moveFrame) moveFrame = requestAnimationFrame(flushMove);
  }

  function releaseCapture(pointerId: number): void {
    try {
      root?.releasePointerCapture(pointerId);
    } catch {
      /* the capture may already have gone with the pointer */
    }
  }

  function onPointerUp(e: PointerEvent): void {
    const d = drag;
    if (!live || !d || e.pointerId !== d.pointerId) return;
    if (moveFrame) {
      cancelAnimationFrame(moveFrame);
      moveFrame = 0;
    }
    pending = { x: e.clientX, y: e.clientY, shift: e.shiftKey };
    flushMove();

    releaseCapture(e.pointerId);
    drag = null;
    downAt = null;
    // `selecting` describes a region drag in flight and nothing else. Leaving it
    // set after the release is not cosmetic: `candidate` is gated on `armed`, so
    // a gesture that ended without committing — a degenerate drag, or a nudge
    // the user is still adjusting — would silently kill window auto-detect for
    // the rest of the capture. `commit()` below accepts `armed` just as happily.
    if (phase === "selecting") phase = "armed";

    switch (d.kind) {
      case "sel":
        finishSelection();
        break;
      case "selresize":
        // The only way a selection is still on screen to be resized is the
        // keyboard-adjust path, where Enter is explicitly the shutter — so a
        // release here does not fire one.
        break;
      case "draw":
        finishDraw(d);
        break;
      case "move":
      case "resize":
        if (d.started) doc.endDrag();
        break;
    }
  }

  /**
   * Release the drag, get the capture (M2.7 §2). A click that never moved takes
   * the window under the cursor instead (M2.5 §2); either way the shutter fires
   * here, with no annotating phase and no confirmation in between.
   */
  function finishSelection(): void {
    if (!moved && !keyboardAdjust) {
      const hit = candidate;
      selAnchor = null;
      selCur = null;
      if (!hit) {
        dropSelection();
        return;
      }
      sel = { ...hit.rect };
      void commit();
      return;
    }
    if (keyboardAdjust) return; // Enter commits; the user is still nudging.
    const rect = sel;
    if (!rect || rect.width < MIN_PX || rect.height < MIN_PX) {
      dropSelection();
      return;
    }
    void commit();
  }

  /**
   * A region gesture that produced nothing — a click on bare desktop with no
   * window under it, or a drag too small to be a rect.
   *
   * On an empty document that is M1's cancel-click and stays one. With shapes
   * drawn it must not be: since M2.7 §3 the user can spend a minute annotating
   * before choosing a region, and throwing all of it away on a stray click is
   * not something Undo can undo. The gesture is simply dropped instead — Escape
   * and right-click still cancel.
   */
  function dropSelection(): void {
    sel = null;
    selAnchor = null;
    selCur = null;
    if (doc.shapes.length === 0) void cancel();
  }

  function onPointerCancel(e: PointerEvent): void {
    const d = drag;
    if (!d || e.pointerId !== d.pointerId) return;
    cancelDrag();
  }

  /* ------------------------------------------------------------ text caret */

  function openCaret(at: Point, existing: TextShape | null): void {
    caret = {
      id: existing?.id ?? null,
      at: { x: at.x, y: at.y },
      text: existing?.text ?? "",
      size: existing?.size ?? opts.textSize,
      color: existing?.color ?? opts.color
    };
  }

  function commitCaret(): void {
    const c = caret;
    if (!c) return;
    // Cleared first: unmounting the textarea fires blur, which lands back here.
    caret = null;
    const text = c.text.replace(/\s+$/, "");
    if (!text) {
      if (c.id) doc.removeShape(c.id);
      return;
    }
    if (c.id) doc.updateShape(c.id, { text });
    else {
      doc.addShape({
        kind: "text",
        color: c.color,
        stroke: opts.stroke,
        at: c.at,
        text,
        size: c.size
      });
    }
  }

  /** Grow the overlay to its content; a textarea will not size itself. */
  function fitCaret(): void {
    const el = caretEl;
    if (!el) return;
    el.style.width = "0px";
    el.style.width = `${el.scrollWidth + 2}px`;
    el.style.height = "0px";
    el.style.height = `${el.scrollHeight}px`;
  }

  // Focus once, when the caret appears. Deliberately does not read the text:
  // re-running would drag the insertion point back to the end.
  $effect(() => {
    const el = caretEl;
    if (!el) return;
    el.focus();
    el.setSelectionRange(el.value.length, el.value.length);
  });

  $effect(() => {
    void caret?.text;
    void cssScale;
    if (caretEl) fitCaret();
  });

  function onCaretKeyDown(e: KeyboardEvent): void {
    // Esc commits the caret and must not reach the window handler, which would
    // read it as a step down the cancel ladder.
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      commitCaret();
      root?.focus();
    }
  }

  /* --------------------------------------------------------------- toolbar */

  /** Drops undefined keys so a patch never writes a field back as blank. */
  function prune<T extends object>(patch: T): Partial<T> {
    return Object.fromEntries(Object.entries(patch).filter(([, v]) => v !== undefined)) as Partial<T>;
  }

  /**
   * True between a slider's pointerdown and its release. The gesture is not
   * opened on the document here — only the first restyle it actually produces
   * opens one, so pressing a slider without moving it neither dirties the
   * document nor leaves an empty undo entry behind.
   */
  let optionDrag = false;

  /**
   * A slider fires `oninput` on every pixel of a drag, and with a shape selected
   * every one of those is a document mutation — one sweep of the width control
   * would push ~20 undo entries and flush the 100-entry stack. Fold the whole
   * sweep into one entry, exactly as a pointer drag folds (M2 §4.2).
   */
  function optionGesture(active: boolean): void {
    if (active) {
      optionDrag = true;
      return;
    }
    optionDrag = false;
    doc.endDrag();
  }

  function applyOptions(patch: Partial<ToolOptions>): void {
    opts = { ...opts, ...patch };
    const id = doc.selected;
    if (id === null) return;
    const shape = doc.shapes.find((s) => s.id === id);
    if (!shape) return;
    if (optionDrag && !doc.dragging) doc.beginDrag("restyle");
    switch (shape.kind) {
      case "rect":
      case "ellipse":
        doc.updateShape(id, prune({ color: patch.color, stroke: patch.stroke, fill: patch.fill }));
        break;
      case "text":
        doc.updateShape(id, prune({ color: patch.color, size: patch.textSize }));
        break;
      case "redact":
        doc.updateShape(id, prune({ mode: patch.redactMode, amount: patch.redactAmount }));
        break;
      default:
        doc.updateShape(id, prune({ color: patch.color, stroke: patch.stroke }));
    }
  }

  /**
   * Switching tool switches what a drag does (M2.7 §3): a drawing tool takes the
   * overlay out of committing-on-release, and Region puts it back.
   */
  function setTool(next: ToolId): void {
    if (caret) commitCaret();
    tool = next;
    if (next !== REGION_TOOL) doc.select(null);
    hoverCursor = null;
  }

  /* -------------------------------------------------------------- keyboard */

  /**
   * Text entry swallows every shortcut — including the on-canvas text caret.
   *
   * "Text entry" is [`ownsEscape`]'s set, not "any `<input>`". The only other
   * `<input>`s this window has are the toolbar's sliders and its colour well,
   * and those consume no letter key: treating them as typing meant that after
   * one click on the stroke slider `A`, `V`, Ctrl+Z and Delete all went dead
   * until the user found somewhere else to click. The bar keeps the keys a
   * focused widget genuinely owns (Enter, Space, the arrows) to itself via its
   * own `keydown` guard, which is the right place for that distinction.
   */
  function isTyping(target: EventTarget | null): boolean {
    return ownsEscape(target);
  }

  /** `<input>` types that are real text entry, where Escape belongs to the field. */
  const TEXT_INPUT_TYPES = new Set([
    "text",
    "search",
    "url",
    "tel",
    "email",
    "password",
    "number"
  ]);

  /**
   * Whether the focused element is allowed to keep an Escape to itself.
   *
   * Deliberately much narrower than [`isTyping`]. Since M2.6 §3 the toolbar is up
   * from `armed`, and its stroke / text-size / redaction sliders and the colour
   * well are `<input>`s that take focus on click — `isTyping` calls all of those
   * "typing", so gating Escape on it meant one click on a slider silently
   * disarmed the only way out of a borderless, always-on-top, full-desktop
   * window. Only genuine text entry (the on-canvas caret) answers its own
   * Escape; a range or colour control never does.
   */
  function ownsEscape(target: EventTarget | null): boolean {
    const el = target as HTMLElement | null;
    if (!el) return false;
    if (el.isContentEditable) return true;
    if (el.tagName === "TEXTAREA") return true;
    if (el.tagName !== "INPUT") return false;
    return TEXT_INPUT_TYPES.has(((el as HTMLInputElement).type || "text").toLowerCase());
  }

  /** Escape step 1: unwind whatever gesture is in flight. */
  function cancelDrag(): boolean {
    const d = drag;
    if (!d) return false;
    drag = null;
    downAt = null;
    releaseCapture(d.pointerId);
    if (d.kind === "draw") doc.cancelDrag();
    else if ((d.kind === "move" || d.kind === "resize") && d.started) doc.cancelDrag();
    else if (d.kind === "selresize") sel = { ...d.origin };
    else if (d.kind === "sel") {
      selAnchor = null;
      selCur = null;
      sel = null;
      phase = "armed";
    }
    return true;
  }

  /** Escape step 4: drop the selection and go back to choosing one. */
  function backToSelecting(): void {
    sel = null;
    selAnchor = null;
    selCur = null;
    doc.select(null);
    keyboardAdjust = false;
    phase = "armed";
  }

  /**
   * The Escape ladder (M2.5 §3, re-rung by M2.7 §3), in order:
   *
   *   1. cancel the shape (or region drag) currently being drawn
   *   2. close the text caret
   *   3. put the drawing tool down and return to Region
   *   4. clear the selection / deselect the shape
   *   5. cancel the capture
   *
   * This is load-bearing. The window is borderless, always-on-top and the size
   * of the virtual desktop, so any state without a way out is a hard lockout —
   * every state the tool model adds needs its own rung, and the bottom rung
   * always reaches `cancel()`.
   */
  function onEscape(): void {
    // Escape must take this window off the screen from EVERY state, including
    // the two that have no capture to cancel. Nothing here can reach the user
    // unless the window is visible and focused, so an Escape arriving in one of
    // those states *is* the report that it is stuck.
    if (phase === "idle") {
      // Nothing armed and nothing painted: the only way to be on screen here is
      // a show that raced a cancel, which is a full-screen sheet of webview
      // background with no way out but the task manager.
      void abandon();
      return;
    }
    if (phase === "committing") {
      // The round trip owns the teardown and ends at `toIdle()` either way, so
      // this does not touch the phase — but it can be slow (a 4K annotated PNG
      // crossing the IPC), and hiding early never cancels the capture in
      // flight. Rust hides the window on that path anyway; this only makes it
      // happen when the user asks.
      void hideSelf();
      return;
    }
    if (cancelDrag()) return;
    if (caret) {
      // Committing, not discarding: M2 §4.4 makes Escape the text tool's commit
      // and empty text its own discard, and `onCaretKeyDown` already does that
      // when focus is in the textarea. This rung is the same key arriving from
      // somewhere else — focus parked on a toolbar slider, say — and it must not
      // mean the opposite thing.
      commitCaret();
      return;
    }
    if (tool !== REGION_TOOL) {
      // Not "no tool" any more: the way out of a drawing tool is the tool that
      // takes the shot, so one Escape restores commit-on-release.
      setTool(REGION_TOOL);
      return;
    }
    if (sel || doc.selected !== null) {
      backToSelecting();
      return;
    }
    void cancel();
  }

  function nudgeSelection(dx: number, dy: number): void {
    if (!selAnchor || !selCur) return;
    const f = freeze;
    if (!f) return;
    keyboardAdjust = true;
    selCur = {
      x: clamp(selCur.x + dx, 0, f.width),
      y: clamp(selCur.y + dy, 0, f.height)
    };
    sel = snapRect(selAnchor, selCur);
  }

  function onKeyDown(e: KeyboardEvent): void {
    // Escape stays live in every phase, including while the frame is decoding
    // and after a failed arm — it is the only way out of a full-screen sheet.
    if (e.key === "Escape") {
      if (ownsEscape(e.target)) return; // the caret answers its own Escape
      e.preventDefault();
      onEscape();
      return;
    }
    if (!live || isTyping(e.target)) return;

    const mod = e.ctrlKey || e.metaKey;
    const key = e.key.toLowerCase();

    if (mod) {
      if (key === "z") {
        e.preventDefault();
        if (e.shiftKey) doc.redo();
        else doc.undo();
      } else if (key === "y") {
        e.preventDefault();
        doc.redo();
      }
      return;
    }
    if (e.altKey) return;

    if (e.key === "Enter") {
      // A focused button would otherwise fire its own click as well.
      if (e.target instanceof HTMLElement && e.target.tagName === "BUTTON") return;
      e.preventDefault();
      // The selection when there is one, otherwise the whole frozen desktop with
      // everything drawn on it (M2.7 §3). `commit()` resolves which; with
      // neither, `canCommit` is false and it would only cancel.
      if (canCommit) void commit();
      return;
    }

    if (e.key === "Delete" || e.key === "Backspace") {
      if (doc.selected === null) return;
      e.preventDefault();
      doc.deleteSelected();
      return;
    }

    if (e.key.startsWith("Arrow")) {
      const step = e.shiftKey ? 10 : 1;
      const dx = e.key === "ArrowLeft" ? -step : e.key === "ArrowRight" ? step : 0;
      const dy = e.key === "ArrowUp" ? -step : e.key === "ArrowDown" ? step : 0;
      // A selected shape owns the arrows; otherwise they fine-tune the region.
      const shape = selectedShape;
      if (shape) {
        e.preventDefault();
        doc.updateShape(shape.id, movePatch(shape, dx, dy));
        return;
      }
      e.preventDefault();
      nudgeSelection(dx, dy);
      return;
    }

    // Tool shortcuts, M2 §4.4, restricted to the set this overlay offers, plus
    // `V` for Region. Checked last so a modifier combination can never land on
    // one. Live in every phase the bar is (M2.6 §3), because since M2.7 §3 the
    // tool is what decides whether the next drag selects or draws.
    // `B`/`P` both land on Redact and carry the mode with them, the way ShareX's
    // separate Blur and Pixelate tools do.
    const hit = toolHitForKey(key);
    if (hit && OVERLAY_TOOL_IDS.has(hit.tool.id)) {
      e.preventDefault();
      setTool(hit.tool.id);
      if (hit.preset) applyOptions(hit.preset);
    }
  }
</script>

<svelte:window bind:innerWidth={viewW} bind:innerHeight={viewH} onkeydown={onKeyDown} />

<div
  class="overlay"
  class:idle={phase === "idle"}
  bind:this={root}
  role="application"
  aria-label="Region selection"
  tabindex="-1"
  style:cursor
  onpointerdown={onPointerDown}
  onpointermove={onPointerMove}
  onpointerup={onPointerUp}
  onpointercancel={onPointerCancel}
  onwheel={onWheel}
  onpointerenter={() => {
    if (live) hasPointer = true;
  }}
  onpointerleave={() => {
    if (!drag) hasPointer = false;
  }}
  oncontextmenu={(e: MouseEvent) => {
    e.preventDefault();
    void cancel();
  }}
>
  {#if phase !== "idle"}
    <!-- The frozen desktop AND the annotations, through the M2 render(). One
         canvas, sized to the freeze frame, so this is a 1:1 blit. -->
    <canvas class="frame" bind:this={canvasEl}></canvas>
  {/if}

  {#if holeBox && holeBox.width > 0 && holeBox.height > 0}
    <!-- Four rects rather than an inverted box-shadow: crisp at any DPI and it
         does not force a full-desktop repaint on every pointermove.
         `glide` travels with the ring below so the undimmed hole and its outline
         move as one box (M2.9 §2); it is off for anything the pointer is
         actually dragging. -->
    <div class="scrim" class:glide style="left:0;top:0;width:100%;height:{holeBox.top}px"></div>
    <div
      class="scrim"
      class:glide
      style="left:0;top:{holeBox.top + holeBox.height}px;width:100%;bottom:0"
    ></div>
    <div
      class="scrim"
      class:glide
      style="left:0;top:{holeBox.top}px;width:{holeBox.left}px;height:{holeBox.height}px"
    ></div>
    <div
      class="scrim"
      class:glide
      style="left:{holeBox.left + holeBox.width}px;top:{holeBox.top}px;right:0;height:{holeBox.height}px"
    ></div>

    {#if sel}
      <div
        class="marquee"
        style="left:{holeBox.left}px;top:{holeBox.top}px;width:{holeBox.width}px;height:{holeBox.height}px"
      >
        <span class="tick tl"></span>
        <span class="tick tr"></span>
        <span class="tick br"></span>
        <span class="tick bl"></span>
      </div>
    {/if}

    <!-- Grips on a selection that is still on screen rather than already
         captured: the keyboard-adjust case, and never mid-drag. -->
    {#if selBox && tool === REGION_TOOL && !drag}
      {#each SEL_HANDLES as h (h.id)}
        <span
          class="grip"
          style="left:{selBox.left + selBox.width * h.fx}px;top:{selBox.top +
            selBox.height * h.fy}px"
        ></span>
      {/each}
    {/if}
  {:else if phase !== "idle"}
    <div class="scrim full"></div>
  {/if}

  {#if hiliteBox}
    <!-- The window auto-detect ring (M2.5 §2), drawn from the retained rect
         rather than from `candidate` so it can fade out in place after the
         pointer has left every window. It stays mounted between windows, which
         is what lets the glide read as one box moving rather than two boxes
         crossfading; the first candidate of a capture mounts it, and
         @starting-style fades that first appearance in. -->
    <div
      class="hilite"
      class:on={hiliteOn}
      class:glide
      style="left:{hiliteBox.left}px;top:{hiliteBox.top}px;width:{hiliteBox.width}px;height:{hiliteBox.height}px"
    ></div>
  {/if}

  {#if selectedBox}
    <div
      class="shape-sel"
      style="left:{selectedBox.left}px;top:{selectedBox.top}px;width:{selectedBox.width}px;height:{selectedBox.height}px"
    ></div>
    {#each shapeHandles as h (h.id)}
      <span class="shape-grip" style="left:{h.left}px;top:{h.top}px"></span>
    {/each}
  {/if}

  {#if showGuides}
    <div class="guide v" style="left:{cx}px"></div>
    <div class="guide h" style="top:{cy}px"></div>
  {/if}

  {#if showBadge || candidate}
    <div
      class="badge"
      bind:clientWidth={badgeW}
      bind:clientHeight={badgeH}
      style="left:{badgePos.left}px;top:{badgePos.top}px"
    >
      {#if candidate}
        <span class="title">{candidate.title || candidate.appName}</span>
        <span class="sep"></span>
        <span class="dims">{candidate.rect.width} &times; {candidate.rect.height}</span>
      {:else if sel}
        <span class="dims">{sel.width} &times; {sel.height}</span>
        <span class="sep"></span>
        <span class="origin">{sel.x}, {sel.y}</span>
      {/if}
    </div>
  {/if}

  {#if showLoupe}
    <div class="loupe" style="left:{loupePos.left}px;top:{loupePos.top}px">
      <div class="loupe-view">
        <canvas class="loupe-img" bind:this={loupeEl}></canvas>
        <div class="loupe-line v"></div>
        <div class="loupe-line h"></div>
        <div
          class="loupe-cell"
          style="left:{loupePos.cellLeft}px;top:{loupePos.cellTop}px;width:{loupePos.cell}px;height:{loupePos.cell}px"
        ></div>
      </div>
      <div class="loupe-coord">
        <span>{cursorPx ? `${cursorPx.x}, ${cursorPx.y}` : ""}</span>
        <!-- The wheel is the only way to change this, so the factor has to be on
             screen for the control to be discoverable at all (M2.9 §1). -->
        <span class="loupe-zoom">{loupeZoom}&times;</span>
      </div>
    </div>
  {/if}

  {#if caret}
    <!-- A real focused textarea rather than a synthetic caret, so IME
         composition, selection and the system clipboard all behave. It sits
         exactly where render() will draw the committed TextShape and shares the
         renderer's font, so the preview is not a different typeface. -->
    <textarea
      class="caret"
      data-chrome
      bind:this={caretEl}
      bind:value={caret.text}
      spellcheck="false"
      autocomplete="off"
      aria-label="Annotation text"
      style:left="{caretPos.left}px"
      style:top="{caretPos.top}px"
      style:font="600 {caret.size * cssScale}px {ANNOTATION_FONT_FAMILY}"
      style:line-height={TEXT_LINE_HEIGHT}
      style:color={caret.color}
      onkeydown={onCaretKeyDown}
      onblur={commitCaret}
    ></textarea>
  {/if}

  {#if live}
    <!-- Pinned to the top edge from the moment the overlay arms (M2.6 §3), so a
         tool is picked first and the region dragged second. `onHover` is the
         suppression signal for the chrome underneath: it rides the bar's own
         pointerenter/leave, which do not bubble and are not among the events
         the bar swallows. While a drag holds the pointer capture they never
         fire at all, which is right — dragging a selection across the bar must
         not blink the crosshair off. -->
    <div class="bar" data-chrome>
      <OverlayToolbar
        {tool}
        {opts}
        canUndo={doc.canUndo}
        canRedo={doc.canRedo}
        {canCommit}
        onTool={setTool}
        onOptions={applyOptions}
        onOptionsGesture={optionGesture}
        onUndo={() => doc.undo()}
        onRedo={() => doc.redo()}
        onCommit={() => void commit()}
        onCancel={() => void cancel()}
        onHover={(over) => (overBar = over)}
      />
    </div>
  {/if}

</div>

<style>
  /* The overlay window is borderless and covers the virtual desktop; any
     scrollbar or margin here shows up as a seam over the user's screen.
     Gated on the `overlay-window` class that app.html stamps pre-paint: this
     component's CSS ships in the same bundle the main window loads, so an
     ungated `html, body` rule would strip the app shell's background too. */
  :global(html.overlay-window),
  :global(html.overlay-window body) {
    position: fixed;
    inset: 0;
    margin: 0;
    padding: 0;
    overflow: hidden;
    background: transparent;
  }

  /* The --ov-* palette lives in app.css and is identical in both themes: this
     component paints over the frozen desktop, not inside the app chrome, so it
     must read the same whichever palette the app is wearing. */
  .overlay {
    position: fixed;
    inset: 0;
    overflow: hidden;
    outline: none;
    user-select: none;
    -webkit-user-select: none;
    /* Without this the webview claims pointer sequences for panning gestures
       and a drag stops halfway through. */
    touch-action: none;
    font-family: var(--font, system-ui, sans-serif);
    color: var(--ov-text);
  }

  /* Idle is a genuinely empty window: the pre-warmed overlay sits here between
     captures and must not paint a single pixel if it is ever shown by mistake. */
  .overlay.idle {
    pointer-events: none;
  }

  .frame {
    position: absolute;
    left: 0;
    top: 0;
    width: 100%;
    height: 100%;
    pointer-events: none;
  }

  .scrim {
    position: absolute;
    background: var(--ov-scrim);
    pointer-events: none;
  }

  .scrim.full {
    inset: 0;
  }

  .marquee {
    position: absolute;
    box-shadow:
      0 0 0 1px var(--ov-line),
      0 0 0 2px var(--ov-line-ring);
    pointer-events: none;
  }

  /* A window under the cursor reads as a suggestion, not a committed selection.
     It is its own element rather than a modifier on .marquee so that it can
     outlive the candidate for the length of its fade-out (M2.9 §2). */
  .hilite {
    position: absolute;
    box-shadow:
      0 0 0 2px var(--accent),
      0 0 0 3px var(--ov-line-ring);
    opacity: 0;
    transition: opacity 110ms ease-out;
    pointer-events: none;
  }

  .hilite.on {
    opacity: 1;
  }

  /* The first candidate of a capture mounts this element, and a transition does
     not run on the frame an element appears. Engines without @starting-style
     simply show it immediately, which is the pre-M2.9 behaviour. */
  @starting-style {
    .hilite.on {
      opacity: 0;
    }
  }

  /* ~110ms ease-out on the geometry itself, so the highlight reads as the box
     moving to the next window rather than one box replacing another. Class-gated
     because the same properties carry the selection rect during a drag, which
     must track the pointer exactly. */
  .scrim.glide {
    transition:
      left 110ms ease-out,
      top 110ms ease-out,
      width 110ms ease-out,
      height 110ms ease-out;
  }

  .hilite.glide {
    transition:
      opacity 110ms ease-out,
      left 110ms ease-out,
      top 110ms ease-out,
      width 110ms ease-out,
      height 110ms ease-out;
  }

  /* Decoration, and decoration is exactly what this query exists to switch off
     (M2.9 §2): no transition at all, in either direction. */
  @media (prefers-reduced-motion: reduce) {
    .hilite,
    .scrim.glide,
    .hilite.glide {
      transition: none;
    }
  }

  .tick {
    position: absolute;
    width: 8px;
    height: 8px;
    border: 2px solid var(--ov-line);
    filter: drop-shadow(0 0 1px var(--ov-line-ring));
  }

  .tick.tl {
    left: -1px;
    top: -1px;
    border-right: none;
    border-bottom: none;
  }
  .tick.tr {
    right: -1px;
    top: -1px;
    border-left: none;
    border-bottom: none;
  }
  .tick.br {
    right: -1px;
    bottom: -1px;
    border-left: none;
    border-top: none;
  }
  .tick.bl {
    left: -1px;
    bottom: -1px;
    border-right: none;
    border-top: none;
  }

  /* Affordance only: the grab zones are hit-tested in image px, so these are
     never allowed to eat a pointer event. */
  .grip {
    position: absolute;
    width: 8px;
    height: 8px;
    margin: -4px 0 0 -4px;
    border: 1px solid var(--ov-line-ring);
    background: var(--ov-line);
    pointer-events: none;
  }

  .shape-sel {
    position: absolute;
    margin: -3px 0 0 -3px;
    padding: 3px;
    box-sizing: content-box;
    border: 1px dashed var(--accent);
    pointer-events: none;
  }

  .shape-grip {
    position: absolute;
    width: 8px;
    height: 8px;
    margin: -4px 0 0 -4px;
    border: 1px solid var(--accent);
    background: var(--ov-panel);
    pointer-events: none;
  }

  .guide {
    position: absolute;
    background: var(--ov-guide);
    pointer-events: none;
  }

  .guide.v {
    top: 0;
    bottom: 0;
    width: 1px;
  }

  .guide.h {
    left: 0;
    right: 0;
    height: 1px;
  }

  .badge {
    position: absolute;
    display: flex;
    align-items: center;
    gap: 8px;
    max-width: 420px;
    padding: 6px 10px;
    border-radius: var(--radius, 8px);
    background: var(--ov-panel);
    border: 1px solid var(--ov-panel-border);
    box-shadow: var(--ov-panel-shadow);
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 12px;
    line-height: 1;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    pointer-events: none;
  }

  .dims {
    font-size: 13px;
    font-weight: 600;
  }

  .title {
    max-width: 280px;
    overflow: hidden;
    text-overflow: ellipsis;
    font-family: var(--font, system-ui, sans-serif);
    font-size: 12px;
    font-weight: 500;
  }

  .sep {
    width: 1px;
    height: 12px;
    background: var(--ov-panel-border);
  }

  .origin {
    color: var(--ov-text-dim);
  }

  .loupe {
    position: absolute;
    width: 132px;
    border-radius: var(--radius-lg, 14px);
    border: 1px solid var(--ov-panel-border);
    background: var(--ov-panel);
    box-shadow: var(--ov-panel-shadow);
    overflow: hidden;
    pointer-events: none;
  }

  .loupe-view {
    position: relative;
    width: 132px;
    height: 132px;
    overflow: hidden;
  }

  .loupe-img {
    position: absolute;
    inset: 0;
    width: 132px;
    height: 132px;
    image-rendering: pixelated;
  }

  .loupe-line {
    position: absolute;
    background: var(--ov-hairline);
  }

  .loupe-line.v {
    left: 50%;
    top: 0;
    bottom: 0;
    width: 1px;
  }

  .loupe-line.h {
    top: 50%;
    left: 0;
    right: 0;
    height: 1px;
  }

  .loupe-cell {
    position: absolute;
    box-shadow:
      0 0 0 1px var(--ov-line),
      0 0 0 2px var(--ov-line-ring);
  }

  .loupe-coord {
    height: 20px;
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    padding: 0 6px;
    border-top: 1px solid var(--ov-panel-border);
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    color: var(--ov-text-dim);
    white-space: nowrap;
  }

  /* Sits with the coordinates rather than over the magnified pixels: this strip
     is the loupe's readout line, and the zoomed image is what is being read. */
  .loupe-zoom {
    padding-left: 6px;
    border-left: 1px solid var(--ov-panel-border);
    color: var(--ov-text);
    font-size: 10px;
    font-weight: 600;
  }

  /* Transparent and chromeless: the glyphs must read as if they were already
     part of the image, over whatever the canvas is showing underneath. */
  .caret {
    position: absolute;
    margin: 0;
    padding: 0;
    border: 0;
    min-width: 1px;
    background: transparent;
    white-space: pre;
    overflow: hidden;
    resize: none;
    outline: 1px dashed color-mix(in srgb, var(--accent) 70%, transparent);
    outline-offset: 2px;
    caret-color: var(--accent);
  }

  /* Fixed to the top edge, horizontally centred, small gap — ShareX's
     arrangement (M2.6 §3). It no longer flips or follows the selection, so its
     measured size is nobody's business but its own. */
  .bar {
    position: absolute;
    top: 10px;
    left: 50%;
    display: flex;
    transform: translateX(-50%);
    cursor: default;
  }

</style>
