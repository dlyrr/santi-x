<script lang="ts">
  /**
   * The interactive canvas — ARCHITECTURE-M2 §4.4.
   *
   * Two rules hold this file together:
   *
   *  1. Every shape on screen is drawn by `render()`. The stage never draws a
   *     shape itself, or the canvas and the exported PNG would drift apart.
   *     The only pixels it owns are *chrome* — the selection outline, the resize
   *     handles and the crop scrim — painted AFTER `render()` returns so they
   *     can never reach an export.
   *
   *  2. `toImage()` is the one place CSS px become image px, and `toStage()` is
   *     its strict inverse. Nothing else here does transform maths. Same
   *     discipline that keeps the M1 region overlay pixel-accurate.
   *
   * The keyboard is shared with `EditorRoot`: this component consumes only the
   * keys that belong to a live interaction (Escape step 1, Enter for crop, the
   * nudge arrows, space to pan) and stops those from bubbling. Everything else —
   * tool shortcuts, undo, save, Delete, the rest of the Escape hierarchy —
   * bubbles up to the window handler that owns it.
   */
  import { untrack } from "svelte";
  import { doc } from "$lib/editor/doc.svelte";
  import type {
    EditorDoc,
    Point,
    Rect,
    Shape,
    ShapeInit,
    TextShape
  } from "$lib/editor/doc.svelte";
  import { ANNOTATION_FONT_FAMILY, TEXT_LINE_HEIGHT, render } from "$lib/editor/render";
  import {
    HANDLE_SIZE,
    clampToImage,
    constrainAngle,
    handleAt,
    handlesFor,
    hitTest,
    movePatch,
    rectFromPoints,
    resizePatch,
    shapeBounds,
    simplifyPoints,
    toolById
  } from "$lib/editor/tools";
  import type { HandleId, ToolId, ToolOptions } from "$lib/editor/tools";

  interface Props {
    tool: ToolId;
    opts: ToolOptions;
    /** Display scale, 1 = 100%. Written back on wheel-zoom and on every fit. */
    zoom: number;
    /** True while the view tracks the window size; any manual zoom or pan clears it. */
    fit: boolean;
    /** True while the stage owns the interaction: a live drag or an open caret. */
    busy: boolean;
    /** A one-shot tool (crop) is finished; the parent returns to Select. */
    onToolDone?: () => void;
  }

  let {
    tool,
    opts,
    zoom = $bindable(1),
    fit = $bindable(true),
    busy = $bindable(false),
    onToolDone
  }: Props = $props();

  const MIN_SCALE = 0.1;
  const MAX_SCALE = 8;
  /** CSS px of breathing room left around a fitted image. */
  const FIT_PAD = 28;
  /** CSS px: a drag shorter than this in either axis is a mis-click, not a shape. */
  const MIN_DRAG = 3;
  const NUDGE = 1;
  const NUDGE_FAST = 10;

  type Caret = { id: string | null; at: Point; text: string; size: number; color: string };

  type Draw = { kind: "draw"; id: string; start: Point };
  type Move = { kind: "move"; id: string; start: Point; origin: Shape; started: boolean };
  type Resize = { kind: "resize"; id: string; handle: HandleId; origin: Shape; started: boolean };
  type CropDrag = { kind: "crop"; start: Point };
  type Pan = { kind: "pan"; clientX: number; clientY: number };
  type Drag = (Draw | Move | Resize | CropDrag | Pan) & { pointerId: number };

  type Tokens = {
    accent: string;
    border: string;
    raised: string;
    scrim: string;
    line: string;
    ring: string;
  };

  let hostEl: HTMLDivElement | undefined = $state();
  let canvasEl: HTMLCanvasElement | undefined = $state();
  let caretEl: HTMLTextAreaElement | undefined = $state();

  let vw = $state(0);
  let vh = $state(0);
  let dpr = $state(1);

  let scale = $state(1);
  let panX = $state(0);
  let panY = $state(0);

  let drag = $state<Drag | null>(null);
  let cropDraft = $state<Rect | null>(null);
  let caret = $state<Caret | null>(null);
  let spaceHeld = $state(false);
  let hoverCursor = $state<string | null>(null);
  let tokens = $state<Tokens | null>(null);

  /**
   * The live pen stroke. Deliberately not `$state`: a single flick pushes
   * hundreds of samples, and each one would be a reactive write nothing reads.
   * The doc gets a plain copy on every update, which is what actually renders.
   */
  let penPoints: Point[] = [];

  const clamp = (v: number, lo: number, hi: number) => (v < lo ? lo : v > hi ? hi : v);

  /**
   * The image region on display. Normally the crop — but the crop tool must show
   * the whole image, otherwise an existing crop could only ever be tightened.
   */
  const area = $derived<Rect>(
    tool === "crop" || !doc.crop
      ? { x: 0, y: 0, width: doc.width, height: doc.height }
      : { x: doc.crop.x, y: doc.crop.y, width: doc.crop.width, height: doc.crop.height }
  );

  const selectedShape = $derived(doc.shapes.find((s) => s.id === doc.selected) ?? null);

  const cursor = $derived.by(() => {
    if (drag?.kind === "pan") return "grabbing";
    if (spaceHeld) return "grab";
    if (drag?.kind === "move") return "grabbing";
    if (tool === "select") return hoverCursor ?? "default";
    return toolById(tool).cursor;
  });

  /* ------------------------------------------------------------ coordinates */

  /**
   * THE conversion: client (CSS) px -> image px, through the current pan/zoom.
   * Every pointer position in this file is funnelled through it.
   */
  function toImage(clientX: number, clientY: number): Point {
    const box = canvasEl?.getBoundingClientRect();
    if (!box) return { x: 0, y: 0 };
    return {
      x: area.x + (clientX - box.left - panX) / scale,
      y: area.y + (clientY - box.top - panY) / scale
    };
  }

  /** Its strict inverse: image px -> CSS px inside the canvas box. */
  function toStage(p: Point): Point {
    return { x: panX + (p.x - area.x) * scale, y: panY + (p.y - area.y) * scale };
  }

  /* -------------------------------------------------------- view transform */

  function clampPan(x: number, y: number): void {
    const w = area.width * scale;
    const h = area.height * scale;
    // Content that fits is centred rather than pannable; content that does not
    // may never be dragged past its own edges, so there is no dead space.
    panX = w <= vw ? Math.round((vw - w) / 2) : clamp(x, vw - w, 0);
    panY = h <= vh ? Math.round((vh - h) / 2) : clamp(y, vh - h, 0);
  }

  function applyFit(): void {
    if (!area.width || !area.height || !vw || !vh) return;
    // Never past 1:1 — blowing a small capture up would misrepresent the pixels
    // the user is annotating.
    const next = clamp(
      Math.min((vw - FIT_PAD * 2) / area.width, (vh - FIT_PAD * 2) / area.height),
      MIN_SCALE,
      1
    );
    scale = next;
    zoom = next;
    clampPan(panX, panY);
  }

  /** Zoom about a client point, so Ctrl+wheel leaves the pixel under the cursor put. */
  function zoomAbout(next: number, clientX: number, clientY: number): void {
    const box = canvasEl?.getBoundingClientRect();
    if (!box) return;
    const anchor = toImage(clientX, clientY);
    scale = clamp(next, MIN_SCALE, MAX_SCALE);
    zoom = scale;
    clampPan(
      clientX - box.left - (anchor.x - area.x) * scale,
      clientY - box.top - (anchor.y - area.y) * scale
    );
  }

  function zoomAboutCentre(next: number): void {
    const box = canvasEl?.getBoundingClientRect();
    if (!box) return;
    zoomAbout(next, box.left + box.width / 2, box.top + box.height / 2);
  }

  // Fit is a mode, not an action: it re-runs on resize, on a crop and when the
  // crop tool widens the visible area.
  $effect(() => {
    if (!fit) return;
    void vw;
    void vh;
    void area.width;
    void area.height;
    untrack(applyFit);
  });

  // The footer's zoom buttons write `zoom` straight through; adopt any value
  // that is not already ours, anchored on the middle of the viewport.
  $effect(() => {
    const target = zoom;
    if (fit) return;
    if (Math.abs(target - untrack(() => scale)) < 1e-4) return;
    untrack(() => zoomAboutCentre(target));
  });

  // A changed visible area can strand the image off-screen at a fixed zoom.
  $effect(() => {
    void area.width;
    void area.height;
    if (untrack(() => fit)) return;
    untrack(() => clampPan(panX, panY));
  });

  $effect(() => {
    busy = drag !== null || caret !== null;
  });

  /* -------------------------------------------------------------- painting */

  let frame = 0;

  function schedule(): void {
    if (frame) return;
    frame = requestAnimationFrame(paint);
  }

  /**
   * The document as the stage should draw it. The shape behind an open caret is
   * withheld because the live <textarea> is standing in for it — drawing both
   * would double every glyph.
   */
  function visibleDoc(): EditorDoc {
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

  function paint(): void {
    frame = 0;
    const canvas = canvasEl;
    if (!canvas || vw === 0 || vh === 0) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    const bw = Math.max(1, Math.round(vw * dpr));
    const bh = Math.max(1, Math.round(vh * dpr));
    if (canvas.width !== bw) canvas.width = bw;
    if (canvas.height !== bh) canvas.height = bh;

    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.clearRect(0, 0, bw, bh);
    if (!doc.width || !doc.height) return;

    // From here the context is in CSS px. The devicePixelRatio-sized backing
    // store exists purely so render() rasterises at the display's real
    // resolution — nothing downstream needs to know about it.
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

    ctx.save();
    ctx.translate(panX, panY);
    // render() only ever uses relative transforms, so this pan survives it.
    // `crop: false` keeps the area about to be discarded visible while the crop
    // tool is choosing one.
    render(ctx, visibleDoc(), { scale, crop: tool !== "crop" });
    ctx.restore();

    paintChrome(ctx);
  }

  function paintChrome(ctx: CanvasRenderingContext2D): void {
    const t = tokens;
    if (!t) return;
    const origin = toStage({ x: area.x, y: area.y });
    const w = area.width * scale;
    const h = area.height * scale;

    // A hairline round the image: a white capture on the light theme would
    // otherwise dissolve into the stage backdrop.
    ctx.save();
    ctx.strokeStyle = t.border;
    ctx.lineWidth = 1;
    ctx.strokeRect(origin.x - 0.5, origin.y - 0.5, w + 1, h + 1);
    ctx.restore();

    if (tool === "crop") paintCrop(ctx, t, origin, w, h);
    else if (tool === "select") paintSelection(ctx, t);
  }

  function paintCrop(
    ctx: CanvasRenderingContext2D,
    t: Tokens,
    origin: Point,
    w: number,
    h: number
  ): void {
    ctx.save();
    ctx.fillStyle = t.scrim;
    ctx.beginPath();
    ctx.rect(origin.x, origin.y, w, h);

    if (cropDraft) {
      const a = toStage({ x: cropDraft.x, y: cropDraft.y });
      const b = toStage({
        x: cropDraft.x + cropDraft.width,
        y: cropDraft.y + cropDraft.height
      });
      // Even-odd punches the keeper out of the dim in one fill, which stays
      // crisp at any zoom — four separate rects leave seams.
      ctx.rect(a.x, a.y, b.x - a.x, b.y - a.y);
      ctx.fill("evenodd");

      const x = a.x - 0.5;
      const y = a.y - 0.5;
      const rw = b.x - a.x + 1;
      const rh = b.y - a.y + 1;
      ctx.strokeStyle = t.ring;
      ctx.lineWidth = 3;
      ctx.strokeRect(x, y, rw, rh);
      ctx.strokeStyle = t.line;
      ctx.lineWidth = 1;
      ctx.strokeRect(x, y, rw, rh);
    } else {
      ctx.fill();
    }
    ctx.restore();
  }

  function paintSelection(ctx: CanvasRenderingContext2D, t: Tokens): void {
    const shape = selectedShape;
    if (!shape || drag?.kind === "draw") return;

    const b = shapeBounds(shape);
    const a = toStage({ x: b.x, y: b.y });
    const c = toStage({ x: b.x + b.width, y: b.y + b.height });
    const pad = 3;

    ctx.save();
    ctx.strokeStyle = t.accent;
    ctx.lineWidth = 1;
    ctx.setLineDash([4, 3]);
    ctx.strokeRect(
      a.x - pad - 0.5,
      a.y - pad - 0.5,
      c.x - a.x + pad * 2 + 1,
      c.y - a.y + pad * 2 + 1
    );
    ctx.setLineDash([]);

    // Handles keep a constant on-screen size: a grip that shrinks with the zoom
    // is unusable at 10%.
    for (const handle of handlesFor(shape)) {
      const p = toStage(handle.at);
      ctx.fillStyle = t.raised;
      ctx.fillRect(p.x - HANDLE_SIZE / 2, p.y - HANDLE_SIZE / 2, HANDLE_SIZE, HANDLE_SIZE);
      ctx.strokeStyle = t.accent;
      ctx.strokeRect(
        p.x - HANDLE_SIZE / 2 + 0.5,
        p.y - HANDLE_SIZE / 2 + 0.5,
        HANDLE_SIZE - 1,
        HANDLE_SIZE - 1
      );
    }
    ctx.restore();
  }

  // One repaint per frame, whatever changed. The deep read picks up an in-place
  // edit from the toolbar (a colour swatch restyling the selected shape) that a
  // reference comparison would miss.
  $effect(() => {
    void $state.snapshot(doc.shapes);
    void doc.crop;
    void doc.selected;
    void doc.src;
    void scale;
    void panX;
    void panY;
    void vw;
    void vh;
    void dpr;
    void tool;
    void cropDraft;
    void caret?.id;
    void tokens;
    schedule();
  });

  $effect(() => () => {
    if (frame) cancelAnimationFrame(frame);
    frame = 0;
  });

  /* ---------------------------------------------------------------- tokens */

  /**
   * Canvas chrome cannot read CSS, so the tokens are pulled off the document and
   * re-read when the theme flips. The crop scrim deliberately reuses the M1
   * `--ov-*` group: like the region overlay it paints over captured pixels
   * rather than app chrome, and must read the same in both palettes.
   */
  function readTokens(): void {
    const cs = getComputedStyle(document.documentElement);
    const get = (name: string) => cs.getPropertyValue(name).trim();
    tokens = {
      accent: get("--accent"),
      border: get("--border"),
      raised: get("--bg-raised"),
      scrim: get("--ov-scrim"),
      line: get("--ov-line"),
      ring: get("--ov-line-ring")
    };
  }

  /* -------------------------------------------------------------- lifecycle */

  $effect(() => {
    const host = hostEl;
    readTokens();

    const themeWatch = new MutationObserver(readTokens);
    themeWatch.observe(document.documentElement, { attributeFilter: ["data-theme"] });

    const sizeWatch = new ResizeObserver((entries) => {
      const box = entries[0]?.contentRect;
      if (!box) return;
      vw = box.width;
      vh = box.height;
      // A fitted view re-fits itself; a zoomed one only needs its pan re-clamped.
      if (!fit) clampPan(panX, panY);
    });
    if (host) {
      vw = host.clientWidth;
      vh = host.clientHeight;
      sizeWatch.observe(host);
    }

    dpr = window.devicePixelRatio || 1;
    let dprWatch: MediaQueryList | null = null;
    const onDpr = () => {
      dpr = window.devicePixelRatio || 1;
      armDpr();
    };
    // The only DPI-change signal a webview offers, and it has to be re-armed:
    // the query is pinned to the ratio that was current when it was created.
    function armDpr(): void {
      dprWatch?.removeEventListener("change", onDpr);
      dprWatch = window.matchMedia(`(resolution: ${dpr}dppx)`);
      dprWatch.addEventListener("change", onDpr);
    }
    armDpr();

    host?.focus();

    return () => {
      themeWatch.disconnect();
      sizeWatch.disconnect();
      dprWatch?.removeEventListener("change", onDpr);
    };
  });

  // Entering Crop seeds the draft from the crop already applied, so widening one
  // is a drag rather than a reset-then-redraw. Leaving drops the pending draft.
  let cropActive = false;
  $effect(() => {
    const on = tool === "crop";
    if (on === cropActive) return;
    cropActive = on;
    const existing = doc.crop;
    cropDraft = on && existing ? { ...existing } : null;
  });

  /* ---------------------------------------------------------------- drawing */

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
      // Colour and stroke are carried because `ShapeBase` requires them, but
      // the renderer ignores both for a redaction — including in erase mode,
      // where the fill comes from the surrounding pixels (M5 §2). Passing the
      // live opts keeps the shape uniform with the rest.
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
        return {
          kind: "step",
          color: opts.color,
          stroke: opts.stroke,
          at,
          n: doc.nextStepNumber
        };
      default:
        return null;
    }
  }

  function startDraw(e: PointerEvent, at: Point): void {
    const init = initialShape(at);
    if (!init) return;
    // One gesture, one undo entry: every update() until endDrag() folds into
    // this snapshot (M2 §4.2).
    doc.beginDrag(`add ${init.kind}`);
    const id = doc.addShape(init);
    if (!id) {
      doc.cancelDrag();
      return;
    }
    penPoints = [at];
    drag = { kind: "draw", pointerId: e.pointerId, id, start: at };
    hostEl?.setPointerCapture(e.pointerId);
  }

  function updateDraw(d: Draw, e: PointerEvent): void {
    const p = clampToImage(toImage(e.clientX, e.clientY), doc);
    switch (tool) {
      case "arrow":
      case "line":
        doc.updateShape(d.id, { to: e.shiftKey ? constrainAngle(d.start, p) : p });
        break;
      case "rect":
      case "ellipse":
      case "highlight":
      case "redact":
        doc.updateShape(d.id, { rect: rectFromPoints(d.start, p, e.shiftKey) });
        break;
      case "pen": {
        // The browser batches several samples into one pointermove; taking the
        // coalesced list back is what keeps a fast stroke smooth.
        const batch = typeof e.getCoalescedEvents === "function" ? e.getCoalescedEvents() : [];
        const samples = batch.length > 0 ? batch : [e];
        for (const s of samples) penPoints.push(clampToImage(toImage(s.clientX, s.clientY), doc));
        doc.updateShape(d.id, { points: penPoints.slice() });
        break;
      }
      case "step":
        doc.updateShape(d.id, { at: p });
        break;
    }
  }

  /** Extent of a drag in image px — what decides whether it was a shape or a click. */
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
    if (drawnExtent(shape) * scale < MIN_DRAG) {
      doc.cancelDrag();
      return;
    }
    doc.endDrag();
  }

  /* ---------------------------------------------------------------- select */

  function startSelect(e: PointerEvent, p: Point): void {
    const current = selectedShape;
    const handle = current ? handleAt(current, p, scale) : null;
    if (current && handle) {
      drag = {
        kind: "resize",
        pointerId: e.pointerId,
        id: current.id,
        handle: handle.id,
        // Snapshot: resizePatch must keep working off the pre-drag geometry, or
        // dragging a handle past the opposite edge would flip on every frame.
        origin: $state.snapshot(current) as Shape,
        started: false
      };
      hostEl?.setPointerCapture(e.pointerId);
      return;
    }

    const hit = hitTest(doc, p, scale);
    doc.selected = hit ? hit.id : null;
    if (!hit) return;
    drag = {
      kind: "move",
      pointerId: e.pointerId,
      id: hit.id,
      start: p,
      origin: $state.snapshot(hit) as Shape,
      started: false
    };
    hostEl?.setPointerCapture(e.pointerId);
  }

  function updateHover(e: PointerEvent): void {
    if (tool !== "select" || spaceHeld) {
      hoverCursor = null;
      return;
    }
    const p = toImage(e.clientX, e.clientY);
    const current = selectedShape;
    const handle = current ? handleAt(current, p, scale) : null;
    hoverCursor = handle ? handle.cursor : hitTest(doc, p, scale) ? "move" : null;
  }

  /* ------------------------------------------------------------------ crop */

  function applyCrop(): void {
    const r = cropDraft;
    if (!r || r.width < 1 || r.height < 1) return;
    cropDraft = null;
    // setCrop clamps to the image, rounds to whole pixels and owns its own undo
    // entry, so nothing is committed here.
    doc.setCrop(r);
    onToolDone?.();
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
      // Empty text never becomes a shape (M2 §4.4); blanking an existing one is
      // the only sensible reading of "delete this label".
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

  const caretPos = $derived(caret ? toStage(caret.at) : { x: 0, y: 0 });

  // Focus once, when the caret appears. Deliberately does not read `scale` or
  // the text: re-running would drag the insertion point back to the end.
  $effect(() => {
    const el = caretEl;
    if (!el) return;
    el.focus();
    el.setSelectionRange(el.value.length, el.value.length);
  });

  $effect(() => {
    void caret?.text;
    void scale;
    if (caretEl) fitCaret();
  });

  function onCaretKeyDown(e: KeyboardEvent): void {
    // Esc commits the caret (M2 §4.4) and must not reach the window handler,
    // which would read it as "close the editor".
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      commitCaret();
      hostEl?.focus();
    }
  }

  /* --------------------------------------------------------------- pointer */

  function releaseCapture(pointerId: number): void {
    try {
      hostEl?.releasePointerCapture(pointerId);
    } catch {
      // The capture may already have been dropped with the pointer.
    }
  }

  function onPointerDown(e: PointerEvent): void {
    if (caretEl && e.target === caretEl) return;
    if (!doc.width) return;
    if (caret) commitCaret();

    if (e.button === 1 || (e.button === 0 && spaceHeld)) {
      e.preventDefault();
      // Only leave fit mode when there is something to pan to; a fitted image
      // that already sits centred would otherwise stop tracking the window.
      if (area.width * scale > vw + 1 || area.height * scale > vh + 1) fit = false;
      drag = { kind: "pan", pointerId: e.pointerId, clientX: e.clientX, clientY: e.clientY };
      hostEl?.setPointerCapture(e.pointerId);
      return;
    }
    if (e.button !== 0) return;

    e.preventDefault();
    hostEl?.focus();
    const p = toImage(e.clientX, e.clientY);

    if (tool === "select") {
      startSelect(e, p);
      return;
    }
    if (tool === "crop") {
      const at = clampToImage(p, doc);
      cropDraft = { x: at.x, y: at.y, width: 0, height: 0 };
      drag = { kind: "crop", pointerId: e.pointerId, start: at };
      hostEl?.setPointerCapture(e.pointerId);
      return;
    }
    if (tool === "text") {
      openCaret(clampToImage(p, doc), null);
      return;
    }
    startDraw(e, clampToImage(p, doc));
  }

  function onPointerMove(e: PointerEvent): void {
    const d = drag;
    if (!d) {
      updateHover(e);
      return;
    }
    if (e.pointerId !== d.pointerId) return;

    switch (d.kind) {
      case "pan":
        clampPan(panX + (e.clientX - d.clientX), panY + (e.clientY - d.clientY));
        d.clientX = e.clientX;
        d.clientY = e.clientY;
        break;
      case "draw":
        updateDraw(d, e);
        break;
      case "move": {
        const p = toImage(e.clientX, e.clientY);
        // The snapshot is only opened on real movement, so a plain click-select
        // never leaves an empty undo entry behind.
        if (!d.started) {
          d.started = true;
          doc.beginDrag("move");
        }
        doc.updateShape(d.id, movePatch(d.origin, p.x - d.start.x, p.y - d.start.y));
        break;
      }
      case "resize": {
        const p = clampToImage(toImage(e.clientX, e.clientY), doc);
        if (!d.started) {
          d.started = true;
          doc.beginDrag("resize");
        }
        doc.updateShape(d.id, resizePatch(d.origin, d.handle, p));
        break;
      }
      case "crop": {
        const p = clampToImage(toImage(e.clientX, e.clientY), doc);
        cropDraft = rectFromPoints(d.start, p, e.shiftKey);
        break;
      }
    }
  }

  function onPointerUp(e: PointerEvent): void {
    const d = drag;
    if (!d || e.pointerId !== d.pointerId) return;
    releaseCapture(e.pointerId);
    drag = null;

    switch (d.kind) {
      case "draw":
        finishDraw(d);
        break;
      case "move":
      case "resize":
        if (d.started) doc.endDrag();
        break;
      case "crop":
        // A tap with the crop tool clears the draft rather than leaving a
        // zero-sized one that Enter would silently ignore.
        if (cropDraft && (cropDraft.width < 1 || cropDraft.height < 1)) cropDraft = null;
        break;
      case "pan":
        break;
    }
  }

  /** Escape step 1, and what a lost pointer falls back to. */
  function cancelDrag(): boolean {
    const d = drag;
    if (!d) return false;
    drag = null;
    releaseCapture(d.pointerId);
    if (d.kind === "draw") doc.cancelDrag();
    else if ((d.kind === "move" || d.kind === "resize") && d.started) doc.cancelDrag();
    else if (d.kind === "crop") cropDraft = null;
    return true;
  }

  function onDoubleClick(e: MouseEvent): void {
    if (tool !== "select" || !doc.width) return;
    const hit = hitTest(doc, toImage(e.clientX, e.clientY), scale);
    if (!hit || hit.kind !== "text") return;
    e.preventDefault();
    openCaret(hit.at, hit);
  }

  function onWheel(e: WheelEvent): void {
    if (!doc.width) return;
    e.preventDefault();
    const unit = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? vh : 1;

    if (e.ctrlKey || e.metaKey) {
      fit = false;
      zoomAbout(scale * Math.exp((-e.deltaY * unit) / 420), e.clientX, e.clientY);
      return;
    }
    const dx = e.deltaX * unit;
    const dy = e.deltaY * unit;
    // Shift turns a vertical wheel horizontal, as every scroll surface does.
    if (e.shiftKey) clampPan(panX - dy - dx, panY);
    else clampPan(panX - dx, panY - dy);
  }

  /* -------------------------------------------------------------- keyboard */

  function nudge(dx: number, dy: number): boolean {
    const shape = selectedShape;
    if (!shape) return false;
    doc.updateShape(shape.id, movePatch(shape, dx, dy));
    return true;
  }

  /** Anything that owns its own arrow, space or Enter behaviour. */
  function isFormControl(target: EventTarget | null): boolean {
    const el = target as HTMLElement | null;
    if (!el) return false;
    if (el.isContentEditable) return true;
    const tag = el.tagName;
    return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || tag === "BUTTON";
  }

  /**
   * Bound on the window in the CAPTURE phase, so the stage sees a key before
   * EditorRoot's bubble-phase handler does. That ordering *is* the first step of
   * the Escape hierarchy (M2 §4.4): cancelling a live drag has to win over
   * "reset the tool" and "close the editor". Keys that are not consumed here are
   * left to propagate untouched, and the toolbar's own controls are skipped so a
   * focused slider keeps its arrow keys.
   */
  function onKeyDown(e: KeyboardEvent): void {
    if (isFormControl(e.target)) return;

    if (e.key === "Escape") {
      // Only step 1 lives here. Tool -> selection -> close belong to EditorRoot,
      // so an Escape we do not consume must be allowed to reach it.
      let handled = cancelDrag();
      if (!handled && cropDraft) {
        cropDraft = null;
        handled = true;
      }
      if (handled) {
        e.preventDefault();
        e.stopPropagation();
      }
      return;
    }

    if (e.ctrlKey || e.metaKey || e.altKey) return;

    if (e.key === "Enter" && tool === "crop") {
      e.preventDefault();
      e.stopPropagation();
      applyCrop();
      return;
    }

    if (e.key === " ") {
      e.preventDefault();
      spaceHeld = true;
      return;
    }

    let dx = 0;
    let dy = 0;
    const step = e.shiftKey ? NUDGE_FAST : NUDGE;
    switch (e.key) {
      case "ArrowLeft":
        dx = -step;
        break;
      case "ArrowRight":
        dx = step;
        break;
      case "ArrowUp":
        dy = -step;
        break;
      case "ArrowDown":
        dy = step;
        break;
      default:
        return;
    }
    if (!nudge(dx, dy)) return;
    e.preventDefault();
    e.stopPropagation();
  }

  function onKeyUp(e: KeyboardEvent): void {
    if (e.key === " ") spaceHeld = false;
  }
</script>

<svelte:window
  onkeydowncapture={onKeyDown}
  onkeyup={onKeyUp}
  onblur={() => (spaceHeld = false)}
/>

<!-- tabindex="-1": the stage is a drawing surface, not a tab stop. It is focused
     on mount and on every pointerdown, which is all the keyboard handlers need
     and is what the M1 region overlay does for the same reason. -->
<div
  class="stage"
  bind:this={hostEl}
  role="application"
  aria-label="Annotation canvas"
  tabindex="-1"
  style:cursor
  onpointerdown={onPointerDown}
  onpointermove={onPointerMove}
  onpointerup={onPointerUp}
  onpointercancel={() => cancelDrag()}
  onpointerleave={() => {
    if (!drag) hoverCursor = null;
  }}
  ondblclick={onDoubleClick}
  onwheel={onWheel}
  oncontextmenu={(e: MouseEvent) => e.preventDefault()}
>
  <canvas class="surface" bind:this={canvasEl}></canvas>

  {#if caret}
    <!-- A real focused textarea rather than a synthetic caret, so IME
         composition, selection and the system clipboard all behave. It is
         positioned and scaled to sit exactly where render() will draw the
         committed TextShape, and shares the renderer's font so the preview is
         not a different typeface from the result. -->
    <textarea
      class="caret"
      bind:this={caretEl}
      bind:value={caret.text}
      spellcheck="false"
      autocomplete="off"
      aria-label="Annotation text"
      style:left="{caretPos.x}px"
      style:top="{caretPos.y}px"
      style:font="600 {caret.size * scale}px {ANNOTATION_FONT_FAMILY}"
      style:line-height={TEXT_LINE_HEIGHT}
      style:color={caret.color}
      onkeydown={onCaretKeyDown}
      onblur={commitCaret}
    ></textarea>
  {/if}
</div>

<style>
  .stage {
    position: relative;
    width: 100%;
    height: 100%;
    overflow: hidden;
    outline: none;
    user-select: none;
    -webkit-user-select: none;
    /* Without this the webview claims pointer sequences for panning gestures
       and a drag stops halfway through. */
    touch-action: none;
  }

  .surface {
    display: block;
    width: 100%;
    height: 100%;
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
</style>
