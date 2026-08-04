/**
 * Full-resolution export — M2 §4.5.
 *
 * The canvas is built at the CROPPED size at 1:1 image resolution and runs the
 * same `render()` the stage does. Never export from the on-screen canvas: it is
 * scaled to fit the window and would ship a blurry, wrongly sized image.
 */
import { cropRect, render } from './render';
import type { EditorDoc, Rect } from './doc.svelte';

/** Pixel size of the file `renderToPng()` would produce. */
export function exportSize(doc: EditorDoc): { width: number; height: number } {
  const area: Rect = cropRect(doc);
  return {
    width: Math.max(1, Math.round(area.width)),
    height: Math.max(1, Math.round(area.height))
  };
}

/** The flattened document as an offscreen canvas, at 1:1. */
export function renderToCanvas(doc: EditorDoc): HTMLCanvasElement {
  const { width, height } = exportSize(doc);
  const canvas = document.createElement('canvas');
  canvas.width = width;
  canvas.height = height;
  const ctx = canvas.getContext('2d');
  if (!ctx) throw new Error('Could not obtain a 2D context for the export');
  // `render` translates by the crop origin itself, so the canvas starts at 0,0.
  render(ctx, doc, { scale: 1 });
  return canvas;
}

export function renderToDataUrl(doc: EditorDoc): string {
  return renderToCanvas(doc).toDataURL('image/png');
}

/**
 * Base64 PNG with the `data:image/png;base64,` prefix stripped — the exact
 * form `save_edit` and `copy_edit` expect (M2 §3).
 */
export function renderToPng(doc: EditorDoc): string {
  const url = renderToDataUrl(doc);
  const comma = url.indexOf(',');
  return comma < 0 ? url : url.slice(comma + 1);
}
