/**
 * Writing *text* to the clipboard from the webview.
 *
 * Images do not come through here — `copy_capture` and `copy_edit` write those
 * from Rust, where the format negotiation and the retry-while-another-app-owns-it
 * loop live (M2.7 §1). Text has no such need and no JS half installed for
 * `tauri-plugin-clipboard-manager`, so it goes through the webview.
 *
 * The `execCommand` fallback is not padding. Tauri serves the app from
 * `tauri.localhost`, which WebView2 treats as a trustworthy origin, so the async
 * Clipboard API is present — but it rejects when the document does not have
 * focus, and that is the failure mode that actually shows up: OCR text copied
 * out of a panel the user reached from the always-on-top capture preview, or a
 * link copied from a card in a window that just lost focus to something else.
 * A "Copy link" that throws in exactly the moment it is most useful is a broken
 * action, so the synchronous path is the second attempt rather than an excuse.
 *
 * Throws only when *both* paths refuse, which is the one case the caller has to
 * tell the user about.
 */
export async function writeClipboardText(value: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(value);
    return;
  } catch {
    // Fall through to the synchronous path.
  }

  const area = document.createElement('textarea');
  area.value = value;
  area.setAttribute('readonly', '');
  area.style.position = 'fixed';
  area.style.top = '-1000px';
  area.style.opacity = '0';
  document.body.appendChild(area);
  area.select();
  const ok = document.execCommand('copy');
  area.remove();
  if (!ok) throw new Error('The webview refused to write to the clipboard.');
}
