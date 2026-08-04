import { errorMessage, getSettings, onSettingsChanged, saveSettings } from '$lib/api';
import type { UnlistenFn } from '$lib/api';
import { DEFAULT_THEME, THEME_STORAGE_KEY, isTheme, type Settings, type Theme } from '$lib/types';

/**
 * Stamp the theme on `<html>` and refresh the pre-paint cache.
 *
 * Rust owns the durable value; localStorage exists only so the inline script in
 * `app.html` can pick the palette before first paint (ARCHITECTURE §6). It is
 * therefore rewritten on every load and every change, never read back here.
 */
function applyTheme(theme: Theme): void {
  if (typeof document === 'undefined') return;
  document.documentElement.dataset.theme = theme;
  try {
    localStorage.setItem(THEME_STORAGE_KEY, theme);
  } catch {
    // Private-mode / quota failures only cost us the pre-paint hint.
  }
}

class SettingsStore {
  current = $state<Settings | null>(null);
  loading = $state(false);
  error = $state<string | null>(null);

  #inFlight: Promise<void> | null = null;
  #unlisten: UnlistenFn | null = null;

  get ready(): boolean {
    return this.current !== null;
  }

  get theme(): Theme {
    return this.current?.theme ?? DEFAULT_THEME;
  }

  /** Idempotent: concurrent or repeated calls share one round trip. */
  load(): Promise<void> {
    if (this.#inFlight) return this.#inFlight;
    this.#inFlight = this.#load().finally(() => {
      this.#inFlight = null;
    });
    return this.#inFlight;
  }

  async #load(): Promise<void> {
    this.loading = true;
    this.error = null;
    try {
      this.#adopt(await getSettings());
      await this.#subscribe();
    } catch (e) {
      this.error = errorMessage(e);
    } finally {
      this.loading = false;
    }
  }

  /**
   * Applies `patch` immediately so the UI never lags a click, then persists.
   * On failure the previous snapshot is restored and `error` is set.
   * Returns whether the write landed.
   */
  async update(patch: Partial<Settings>): Promise<boolean> {
    const previous = this.current;
    if (!previous) {
      this.error = 'Settings are not loaded yet';
      return false;
    }

    // hotkeys is merged one level deeper so callers can patch a single binding.
    const next: Settings = {
      ...previous,
      ...patch,
      hotkeys: { ...previous.hotkeys, ...patch.hotkeys }
    };
    this.#adopt(next);
    this.error = null;

    try {
      this.#adopt(await saveSettings(next));
      return true;
    } catch (e) {
      this.#adopt(previous);
      this.error = errorMessage(e);
      return false;
    }
  }

  /**
   * The palette flips before the round trip so the switch feels instant; if the
   * write fails, `update()`'s rollback re-stamps the old theme.
   */
  async setTheme(theme: Theme): Promise<boolean> {
    applyTheme(theme);
    return this.update({ theme });
  }

  async #subscribe(): Promise<void> {
    if (this.#unlisten) return;
    // Reserve the slot before awaiting so two overlapping calls cannot both
    // register a listener.
    this.#unlisten = () => {};
    try {
      this.#unlisten = await onSettingsChanged((settings) => this.#adopt(settings));
    } catch (e) {
      this.#unlisten = null;
      this.error = errorMessage(e);
    }
  }

  /**
   * The one choke point every `Settings` passes through — the initial load, the
   * echo from `save_settings`, and the `settings://changed` event.
   *
   * Rust types `theme` as a plain `String` and does not validate it, so a
   * `settings.json` hand-edited or written by a future build can carry a name
   * this build has no `app.css` block for. `Settings.theme` is declared as
   * `Theme`, which would make that value a lie, and stamping it on `<html>`
   * leaves every token unresolved — a completely untokenised window. M2.6 §5:
   * an unknown theme falls back to `dark`. Coerced here rather than only at the
   * point of stamping, so the settings picker and any later `update()` see the
   * same value the page is actually wearing.
   */
  #adopt(settings: Settings): void {
    const theme: Theme = isTheme(settings.theme) ? settings.theme : DEFAULT_THEME;
    this.current = theme === settings.theme ? settings : { ...settings, theme };
    applyTheme(theme);
  }

  dispose(): void {
    this.#unlisten?.();
    this.#unlisten = null;
  }
}

export const settings = new SettingsStore();
