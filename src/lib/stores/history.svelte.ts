import {
  clearHistory,
  deleteCapture,
  errorMessage,
  getHistory,
  onCaptureNew,
  onCaptureUpdated
} from '$lib/api';
import type { UnlistenFn } from '$lib/api';
import type { CaptureRecord } from '$lib/types';

class HistoryStore {
  items = $state<CaptureRecord[]>([]);
  loading = $state(false);
  error = $state<string | null>(null);

  total = $derived(this.items.length);
  totalBytes = $derived(this.items.reduce((sum, r) => sum + r.sizeBytes, 0));

  #inFlight: Promise<void> | null = null;
  #unlisten: UnlistenFn | null = null;
  #unlistenUpdated: UnlistenFn | null = null;

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
      this.items = await getHistory();
      await this.#subscribe();
    } catch (e) {
      this.error = errorMessage(e);
    } finally {
      this.loading = false;
    }
  }

  byId(id: string): CaptureRecord | undefined {
    return this.items.find((r) => r.id === id);
  }

  /** Rust returns the surviving history; adopt it rather than splicing locally. */
  async remove(id: string, deleteFile: boolean): Promise<boolean> {
    this.error = null;
    try {
      this.items = await deleteCapture(id, deleteFile);
      return true;
    } catch (e) {
      this.error = errorMessage(e);
      return false;
    }
  }

  async clear(deleteFiles: boolean): Promise<boolean> {
    this.error = null;
    try {
      this.items = await clearHistory(deleteFiles);
      return true;
    } catch (e) {
      this.error = errorMessage(e);
      return false;
    }
  }

  async #subscribe(): Promise<void> {
    // Claim the slot before awaiting: two overlapping load()s would otherwise
    // each register a listener and every capture would be unshifted twice.
    if (!this.#unlisten) {
      this.#unlisten = () => {};
      try {
        this.#unlisten = await onCaptureNew((record) => this.#prepend(record));
      } catch (e) {
        this.#unlisten = null;
        this.error = errorMessage(e);
      }
    }

    // Guarded separately so a failure on one channel still leaves the other
    // retryable on the next load().
    if (this.#unlistenUpdated) return;
    this.#unlistenUpdated = () => {};
    try {
      this.#unlistenUpdated = await onCaptureUpdated((record) => this.#replace(record));
    } catch (e) {
      this.#unlistenUpdated = null;
      this.error = errorMessage(e);
    }
  }

  #prepend(record: CaptureRecord): void {
    // A load() that resolves after the event fired can carry the same record.
    this.items = [record, ...this.items.filter((r) => r.id !== record.id)];
  }

  /**
   * An edit saved over its original keeps its place in the list — prepending
   * would leave the gallery showing the same capture twice.
   */
  #replace(record: CaptureRecord): void {
    const at = this.items.findIndex((r) => r.id === record.id);
    if (at === -1) return;
    this.items = this.items.map((r, i) => (i === at ? record : r));
  }

  dispose(): void {
    this.#unlisten?.();
    this.#unlisten = null;
    this.#unlistenUpdated?.();
    this.#unlistenUpdated = null;
  }
}

export const history = new HistoryStore();
