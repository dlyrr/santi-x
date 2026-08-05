<script lang="ts">
  import { open as openDirectoryDialog } from '@tauri-apps/plugin-dialog';
  import { getVersion } from '@tauri-apps/api/app';
  import Toggle from '$lib/components/Toggle.svelte';
  import DestinationsView from '$lib/views/DestinationsView.svelte';
  import { PRESETS } from '$lib/editor/ColorPicker.svelte';
  import {
    getHotkeyStatus,
    onCaptureError,
    onHotkeyStatus,
    openSaveDir,
    DESTINATION_LABEL,
    type HotkeyMechanism,
    type HotkeyStatus,
    type Settings,
    type Theme
  } from '$lib/api';
  import {
    SCROLL_DELAY_MS,
    SCROLL_MAX_FRAMES,
    SCROLL_STEP,
    THEMES
  } from '$lib/types';
  import { settings } from '$lib/stores/settings.svelte';
  import { toast } from '$lib/components/Toast.svelte';

  interface Props {
    /**
     * Whether to render the Destinations form inline (M3 §6).
     *
     * On by default, because the default shell's sidebar has only three entries
     * and Settings is the only place destinations can live there. The ShareX
     * shell turns it off: that shell has a real Destinations menu row of its
     * own, and the same form in two panes of one window is a second place to
     * change the same thing rather than a convenience.
     */
    showDestinations?: boolean;
  }

  let { showDestinations = true }: Props = $props();

  type HotkeyKey = 'region' | 'fullscreen' | 'activeWindow';

  // The shipped defaults, and deliberately free ones: Reset has to land on what
  // a fresh install binds, or it hands back a combo the OS already owns.
  const HOTKEY_DEFAULTS: Record<HotkeyKey, string> = {
    region: 'CmdOrCtrl+Shift+1',
    fullscreen: 'CmdOrCtrl+Shift+2',
    activeWindow: 'CmdOrCtrl+Shift+3'
  };

  const HOTKEY_ROWS: { key: HotkeyKey; label: string; help: string }[] = [
    { key: 'region', label: 'Region', help: 'Freeze the desktop and drag out a selection' },
    { key: 'fullscreen', label: 'Fullscreen', help: 'Every monitor composited into one image' },
    { key: 'activeWindow', label: 'Active window', help: 'The window that currently has focus' }
  ];

  // What the chip beside each hotkey says. `none` is the one that matters: a
  // hotkey that silently does nothing is the confusion this display exists to
  // end, so it is loud rather than informative.
  const MECHANISM_META: Record<HotkeyMechanism, { label: string; title: string }> = {
    plugin: {
      label: 'Bound',
      title: 'Registered with Windows the ordinary way (RegisterHotKey).'
    },
    hook: {
      label: 'Bound via hook',
      title:
        'Another app already owns this combination, so santi.sharex claims it with the low-level keyboard hook.'
    },
    none: {
      label: 'Not bound',
      title: 'Nothing happens when you press this combination.'
    }
  };

  // A Record, not a list: adding a theme to the union without a card here is a
  // type error rather than a silently missing option. Order comes from THEMES.
  const THEME_META: Record<Theme, { name: string; blurb: string }> = {
    dark: { name: 'Dark', blurb: 'Near-black surfaces, blue accent' },
    claude: { name: 'Claude', blurb: 'Warm paper, clay accent, Anthropic Sans' },
    'claude-dark': { name: 'Claude Dark', blurb: 'The same warm clay, lights out' },
    sharex: { name: 'ShareX', blurb: 'Cool greys, Windows blue, square corners' }
  };

  const PATTERN_TOKENS = ['kind', 'yyyy', 'MM', 'dd', 'HH', 'mm', 'ss', 'rand'].map(
    (name) => `{${name}}`
  );

  // Annotation colours are content drawn *onto* the capture, not app chrome, so
  // they are literals rather than tokens — the one exception in ARCHITECTURE §6.
  // Taken from the editor's own picker rather than retyped: a default set here
  // that the editor cannot show as selected is a silently confusing setting.
  const EDITOR_COLORS = PRESETS.map((preset) => preset.hex);

  // Sampled per theme to paint the preview cards. The two radii are in here
  // because `sharex` overrides them — a mock with rounded corners would be
  // lying about the theme it is offering.
  const PALETTE_TOKENS = [
    '--bg',
    '--bg-raised',
    '--surface',
    '--border',
    '--text',
    '--text-dim',
    '--accent',
    '--accent-text',
    '--radius',
    '--radius-lg'
  ] as const;

  type Palette = Record<string, string>;

  const s = $derived(settings.current);

  let dirDraft = $state('');
  let patternDraft = $state('');
  let strokeDraft = $state(4);
  // Annotated: the bounds are `as const`, so an inferred draft would be the
  // literal type of its default and refuse every other value on the slider.
  let scrollDelayDraft = $state<number>(SCROLL_DELAY_MS.def);
  let scrollStepDraft = $state<number>(SCROLL_STEP.def);
  let scrollFramesDraft = $state<number>(SCROLL_MAX_FRAMES.def);
  let recording = $state<HotkeyKey | null>(null);
  let hotkeyError = $state('');
  let hotkeyStatus = $state<HotkeyStatus[]>([]);
  let version = $state('');
  let palettes = $state<Record<string, Palette>>({});
  let now = $state(Date.now());

  /**
   * Auto-upload is armed but not yet on: the toggle asked, and this is the
   * panel that spells out what it means. The setting is *not* written until the
   * user clicks through it, so the switch stays visibly off until then.
   */
  let confirmAutoUpload = $state(false);

  // A sample token so the filename preview looks like a real name rather than
  // re-rolling on every keystroke.
  const randSample = Math.random().toString(36).slice(2, 6);

  $effect(() => {
    if (s) dirDraft = s.saveDir;
  });

  $effect(() => {
    if (s) patternDraft = s.filenamePattern;
  });

  $effect(() => {
    if (s) strokeDraft = s.editorDefaultStroke;
  });

  // The `??` is for a `settings.json` written before M5: Rust fills the three in
  // on load, but a slider that binds `undefined` renders at its midpoint and
  // then writes that midpoint back on the first drag.
  $effect(() => {
    if (s) scrollDelayDraft = s.scrollDelayMs ?? SCROLL_DELAY_MS.def;
  });

  $effect(() => {
    if (s) scrollStepDraft = s.scrollStep ?? SCROLL_STEP.def;
  });

  $effect(() => {
    if (s) scrollFramesDraft = s.scrollMaxFrames ?? SCROLL_MAX_FRAMES.def;
  });

  $effect(() => {
    const timer = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(timer);
  });

  $effect(() => {
    getVersion().then((v) => (version = v));
  });

  $effect(() => {
    const sampled: Record<string, Palette> = {};
    for (const id of THEMES) sampled[id] = samplePalette(id);
    palettes = sampled;
  });

  $effect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    onCaptureError((message) => {
      if (/hotkey|shortcut|accelerator|register/i.test(message)) {
        hotkeyError = message;
      }
    }).then((un) => {
      if (cancelled) un();
      else unlisten = un;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  });

  // The snapshot for the first paint, then every rebind. A failure leaves the
  // list empty and the chips absent — better than asserting a binding we could
  // not read.
  $effect(() => {
    let unlisten: (() => void) | undefined;
    let cancelled = false;
    getHotkeyStatus()
      .then((list) => {
        if (!cancelled) hotkeyStatus = list;
      })
      .catch(() => {});
    onHotkeyStatus((list) => (hotkeyStatus = list)).then((un) => {
      if (cancelled) un();
      else unlisten = un;
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  });

  /**
   * The report for a row, or `null` when there is none to trust yet.
   *
   * Rust reports against the accelerator it registered, so between saving a new
   * combo and the rebind landing the old report still describes the old combo.
   * Showing it then would label a freshly typed shortcut with the last one's
   * fate, so a mismatch reads as "not known yet" and the chip is dropped.
   */
  function statusFor(key: HotkeyKey, accelerator: string): HotkeyStatus | null {
    const found = hotkeyStatus.find((entry) => entry.action === key);
    return found && found.accelerator === accelerator.trim() ? found : null;
  }

  /**
   * Reads another theme's token values by stamping it on <html>, forcing a style
   * recalc, and restoring the previous value — all synchronously, so the browser
   * never gets a frame in which it could paint the foreign palette. This is the
   * only way to draw a truthful preview of a theme that is not currently active.
   */
  function samplePalette(theme: Theme): Palette {
    const root = document.documentElement;
    const previous = root.dataset.theme;
    root.dataset.theme = theme;
    const computed = getComputedStyle(root);
    const out: Palette = {};
    for (const token of PALETTE_TOKENS) out[token] = computed.getPropertyValue(token).trim();
    if (previous === undefined) delete root.dataset.theme;
    else root.dataset.theme = previous;
    return out;
  }

  function mockVars(theme: Theme): string {
    const palette = palettes[theme];
    if (!palette) return '';
    // `--bg` becomes `--m-bg`: drop one dash, or the declaration would read
    // `--m--bg` and every mock would silently fall back to the live theme.
    return PALETTE_TOKENS.map((token) => `--m${token.slice(1)}: ${palette[token]}`).join(';');
  }

  /** The stores report failures through `error` instead of rejecting. */
  async function patch(delta: Partial<Settings>) {
    const ok = await settings.update(delta);
    if (!ok) toast.error(settings.error ?? 'Could not save settings');
  }

  async function chooseTheme(theme: Theme) {
    const ok = await settings.setTheme(theme);
    if (!ok) toast.error(settings.error ?? 'Could not save the theme');
  }

  async function browse() {
    try {
      const picked = await openDirectoryDialog({
        directory: true,
        multiple: false,
        defaultPath: s?.saveDir || undefined
      });
      if (typeof picked === 'string' && picked !== '') {
        dirDraft = picked;
        await patch({ saveDir: picked });
      }
    } catch (err) {
      toast.error(String(err));
    }
  }

  async function revealSaveDir() {
    try {
      await openSaveDir();
    } catch (err) {
      toast.error(String(err));
    }
  }

  function commitDir() {
    if (!s) return;
    const next = dirDraft.trim();
    if (next === '' || next === s.saveDir) {
      dirDraft = s.saveDir;
      return;
    }
    void patch({ saveDir: next });
  }

  function commitPattern() {
    if (!s) return;
    const next = patternDraft.trim();
    if (next === '' || next === s.filenamePattern) {
      patternDraft = s.filenamePattern;
      return;
    }
    void patch({ filenamePattern: next });
  }

  /** Swatch equality: `<input type="color">` always reports lowercase hex. */
  function sameColor(a: string, b: string): boolean {
    return a.toLowerCase() === b.toLowerCase();
  }

  /** The slider writes on release only — every patch persists settings.json. */
  function commitStroke() {
    if (!s || strokeDraft === s.editorDefaultStroke) return;
    void patch({ editorDefaultStroke: strokeDraft });
  }

  function commitScrollDelay() {
    if (!s || scrollDelayDraft === s.scrollDelayMs) return;
    void patch({ scrollDelayMs: scrollDelayDraft });
  }

  function commitScrollStep() {
    if (!s || scrollStepDraft === s.scrollStep) return;
    void patch({ scrollStep: scrollStepDraft });
  }

  function commitScrollFrames() {
    if (!s || scrollFramesDraft === s.scrollMaxFrames) return;
    void patch({ scrollMaxFrames: scrollFramesDraft });
  }

  function resolvePattern(pattern: string, kind: string, at: number): string {
    const d = new Date(at);
    const pad = (n: number) => String(n).padStart(2, '0');
    const tokens: Record<string, string> = {
      kind,
      yyyy: String(d.getFullYear()),
      MM: pad(d.getMonth() + 1),
      dd: pad(d.getDate()),
      HH: pad(d.getHours()),
      mm: pad(d.getMinutes()),
      ss: pad(d.getSeconds()),
      rand: randSample
    };
    // Unknown tokens are left verbatim, matching the Rust side.
    return pattern.replace(/\{(\w+)\}/g, (match, name: string) => tokens[name] ?? match) + '.png';
  }

  const filenamePreview = $derived(resolvePattern(patternDraft, 'region', now));

  function displayAccel(accel: string): string[] {
    if (!accel) return ['Not set'];
    return accel.split('+').map((part) => {
      if (part === 'CmdOrCtrl' || part === 'CommandOrControl') return 'Ctrl';
      if (part === 'Super' || part === 'Meta') return 'Win';
      return part;
    });
  }

  function acceleratorKey(code: string, key: string): string | null {
    if (/^Key[A-Z]$/.test(code)) return code.slice(3);
    if (/^Digit[0-9]$/.test(code)) return code.slice(5);
    if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) return code;
    if (/^Numpad[0-9]$/.test(code)) return code;
    const named = new Set([
      'PrintScreen',
      'Space',
      'Enter',
      'Tab',
      'Backspace',
      'Delete',
      'Insert',
      'Home',
      'End',
      'PageUp',
      'PageDown',
      'ArrowUp',
      'ArrowDown',
      'ArrowLeft',
      'ArrowRight',
      'Minus',
      'Equal',
      'BracketLeft',
      'BracketRight',
      'Backslash',
      'Semicolon',
      'Quote',
      'Comma',
      'Period',
      'Slash',
      'Backquote',
      'CapsLock',
      'NumLock',
      'ScrollLock',
      'Pause'
    ]);
    if (named.has(code)) return code;
    if (key === 'PrintScreen') return 'PrintScreen';
    return null;
  }

  function toAccelerator(event: KeyboardEvent): string | null {
    const base = acceleratorKey(event.code, event.key);
    if (!base) return null;
    const parts: string[] = [];
    if (event.ctrlKey) parts.push('CmdOrCtrl');
    if (event.altKey) parts.push('Alt');
    if (event.shiftKey) parts.push('Shift');
    if (event.metaKey) parts.push('Super');
    parts.push(base);
    return parts.join('+');
  }

  function commitHotkey(key: HotkeyKey, accel: string) {
    recording = null;
    hotkeyError = '';
    if (!s || accel === s.hotkeys[key]) return;
    void patch({ hotkeys: { ...s.hotkeys, [key]: accel } });
  }

  function onWindowKeydown(event: KeyboardEvent) {
    const key = recording;
    if (!key) return;
    event.preventDefault();
    event.stopPropagation();
    if (event.key === 'Escape') {
      recording = null;
      return;
    }
    const accel = toAccelerator(event);
    if (accel) commitHotkey(key, accel);
  }

  function onWindowKeyup(event: KeyboardEvent) {
    const key = recording;
    if (!key) return;
    // Windows hands PrintScreen to the webview as a keyup only — the keydown is
    // swallowed by the OS snipping handler — so that combo has to be read here.
    if (event.key !== 'PrintScreen' && event.code !== 'PrintScreen') return;
    event.preventDefault();
    const accel = toAccelerator(event);
    if (accel) commitHotkey(key, accel);
  }

  function resetHotkey(key: HotkeyKey) {
    commitHotkey(key, HOTKEY_DEFAULTS[key]);
  }

  /**
   * The one switch in this app that can send the screen off the machine without
   * anybody asking again (M3 §1), so the toggle does not write it.
   *
   * Turning it *off* is immediate — there is no reason to make stopping hard.
   * Turning it *on* only opens the confirmation below; `acceptAutoUpload` is the
   * only thing that ever writes `true`.
   */
  function requestAutoUpload(next: boolean) {
    if (!next) {
      confirmAutoUpload = false;
      void patch({ autoUpload: false });
      return;
    }
    confirmAutoUpload = true;
  }

  async function acceptAutoUpload() {
    confirmAutoUpload = false;
    await patch({ autoUpload: true });
  }
</script>

<svelte:window onkeydown={onWindowKeydown} onkeyup={onWindowKeyup} />

<div class="page">
  <header class="page-header">
    <h1 class="page-title">Settings</h1>
  </header>

  {#if !s}
    <p class="loading">
      <span>Loading settings…</span>
      {#if settings.error}<span class="err">{settings.error}</span>{/if}
    </p>
  {:else}
  <section class="section" aria-labelledby="sec-appearance">
    <h2 class="section-title" id="sec-appearance">Appearance</h2>
    <p class="section-help">Applied instantly and remembered between launches.</p>

    <div class="themes" role="radiogroup" aria-labelledby="sec-appearance">
      {#each THEMES as id (id)}
        <button
          type="button"
          class="theme"
          class:selected={s.theme === id}
          role="radio"
          aria-checked={s.theme === id}
          onclick={() => chooseTheme(id)}
        >
          <div class="mock" style={mockVars(id)} aria-hidden="true">
            <div class="mock-side">
              <div class="mock-brand"></div>
              <div class="mock-pill active"></div>
              <div class="mock-pill"></div>
              <div class="mock-pill"></div>
            </div>
            <div class="mock-main">
              <div class="mock-head"></div>
              <div class="mock-card"></div>
              <div class="mock-btn"></div>
            </div>
          </div>
          <span class="theme-meta">
            <span class="theme-name">
              {THEME_META[id].name}
              {#if s.theme === id}
                <svg
                  viewBox="0 0 16 16"
                  width="14"
                  height="14"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                  aria-hidden="true"
                >
                  <path d="m3.5 8.5 3 3 6-7" />
                </svg>
              {/if}
            </span>
            <span class="theme-blurb">{THEME_META[id].blurb}</span>
          </span>
        </button>
      {/each}
    </div>
  </section>

  <section class="section" aria-labelledby="sec-startup">
    <h2 class="section-title" id="sec-startup">Startup</h2>

    <div class="row">
      <span class="row-text">
        <span class="row-label" id="lbl-launch-login">Launch at login</span>
        <span class="row-help">
          Start santi.sharex with Windows so the capture hotkeys work from the moment you log in.
          An autostarted launch always goes to the tray, whatever the setting below says.
        </span>
      </span>
      <Toggle
        checked={s.launchAtLogin}
        labelledBy="lbl-launch-login"
        onchange={(v) => patch({ launchAtLogin: v })}
      />
    </div>

    <div class="row">
      <span class="row-text">
        <span class="row-label" id="lbl-start-hidden">Start hidden in the tray</span>
        <span class="row-help">
          Launch with only the tray icon, the way the real ShareX does. The tray icon is how you
          reopen the window — left-click it to show santi.sharex, right-click it for the capture
          menu.
        </span>
      </span>
      <Toggle
        checked={s.startHidden}
        labelledBy="lbl-start-hidden"
        onchange={(v) => patch({ startHidden: v })}
      />
    </div>
  </section>

  <section class="section" aria-labelledby="sec-capture">
    <h2 class="section-title" id="sec-capture">Capture</h2>

    <div class="row">
      <span class="row-text">
        <span class="row-label" id="lbl-save-disk">Save to disk</span>
        <span class="row-help">Write a PNG into the save directory for every capture.</span>
      </span>
      <Toggle
        checked={s.saveToDisk}
        labelledBy="lbl-save-disk"
        onchange={(v) => patch({ saveToDisk: v })}
      />
    </div>

    <div class="row">
      <span class="row-text">
        <span class="row-label" id="lbl-clipboard">Copy to clipboard</span>
        <span class="row-help">Put the image on the clipboard as soon as it is taken.</span>
      </span>
      <Toggle
        checked={s.copyToClipboard}
        labelledBy="lbl-clipboard"
        onchange={(v) => patch({ copyToClipboard: v })}
      />
    </div>

    <!-- "Annotate before saving" used to live here. M2.7 §2 retired
         `annotateInOverlay` as a gate: releasing a region drag is the shutter,
         full stop, and drawing is what a drawing tool does rather than a mode a
         setting unlocks. The field still deserializes for compatibility with
         older `settings.json` files, but nothing reads it — and a switch that
         writes a value no code consults is a control that lies about having an
         effect, which is the same sin as a menu row that looks live and is not. -->

    <div class="row">
      <span class="row-text">
        <span class="row-label" id="lbl-open-editor">Open the editor after each capture</span>
        <span class="row-help">Jump straight into annotating instead of the app shell.</span>
      </span>
      <Toggle
        checked={s.openEditorAfter}
        labelledBy="lbl-open-editor"
        onchange={(v) => patch({ openEditorAfter: v })}
      />
    </div>

    <div class="row">
      <span class="row-text">
        <span class="row-label" id="lbl-capture-preview">Show a preview after capture</span>
        <span class="row-help">
          A small card in the corner of the screen you captured, with the thumbnail and its
          filename. It never takes focus, holds while the pointer is over it, and clicking it
          opens the capture in the editor.
        </span>
      </span>
      <Toggle
        checked={s.showCapturePreview}
        labelledBy="lbl-capture-preview"
        onchange={(v) => patch({ showCapturePreview: v })}
      />
    </div>

    <div class="row">
      <span class="row-text">
        <span class="row-label" id="lbl-open-folder">Open folder after capture</span>
        <span class="row-help">Reveal the new file in Explorer once it is written.</span>
      </span>
      <Toggle
        checked={s.openFolderAfter}
        labelledBy="lbl-open-folder"
        onchange={(v) => patch({ openFolderAfter: v })}
      />
    </div>

    <div class="row">
      <span class="row-text">
        <span class="row-label" id="lbl-capture-cursor">Capture the mouse cursor</span>
        <span class="row-help">
          Draw the cursor into the shot. Windows never puts it in the screen buffer, so
          santi.sharex rasterises the cursor icon and blends it in at the point of capture.
        </span>
      </span>
      <Toggle
        checked={s.captureCursor}
        labelledBy="lbl-capture-cursor"
        onchange={(v) => patch({ captureCursor: v })}
      />
    </div>

    <div class="row">
      <span class="row-text">
        <span class="row-label" id="lbl-hide-window">Hide window on capture</span>
        <span class="row-help">
          Keep santi.sharex itself out of fullscreen, window and monitor shots. Region capture
          ignores this — hiding the window first would freeze a desktop you never saw.
        </span>
      </span>
      <Toggle
        checked={s.hideWindowOnCapture}
        labelledBy="lbl-hide-window"
        onchange={(v) => patch({ hideWindowOnCapture: v })}
      />
    </div>
  </section>

  <section class="section" aria-labelledby="sec-scrolling">
    <h2 class="section-title" id="sec-scrolling">Scrolling capture</h2>
    <p class="section-help">
      How the long-page stitcher drives the window it is capturing. It finds the real overlap
      between consecutive frames rather than trusting the scroll distance, so these tune the
      scrolling, not the joining.
    </p>

    <div class="row">
      <span class="row-text">
        <label class="row-label" for="scroll-delay">Settle delay</label>
        <span class="row-help">
          How long to wait after each wheel step before taking the next shot. Too short and the
          frame catches a smooth scroll still in flight, which the overlap search reads as a bad
          match and stops the run early.
        </span>
      </span>
      <div class="field">
        <input
          id="scroll-delay"
          class="range"
          type="range"
          min={SCROLL_DELAY_MS.min}
          max={SCROLL_DELAY_MS.max}
          step={SCROLL_DELAY_MS.step}
          bind:value={scrollDelayDraft}
          onchange={commitScrollDelay}
        />
        <span class="range-value mono num">{scrollDelayDraft} ms</span>
      </div>
    </div>

    <div class="row">
      <span class="row-text">
        <label class="row-label" for="scroll-step">Scroll step</label>
        <span class="row-help">
          Wheel notches sent per step. Bigger steps finish a long page in fewer frames, but leave
          less overlap for the stitcher to lock onto — and no overlap ends the run.
        </span>
      </span>
      <div class="field">
        <input
          id="scroll-step"
          class="range"
          type="range"
          min={SCROLL_STEP.min}
          max={SCROLL_STEP.max}
          step={SCROLL_STEP.step}
          bind:value={scrollStepDraft}
          onchange={commitScrollStep}
        />
        <span class="range-value mono num">
          {scrollStepDraft}
          {scrollStepDraft === 1 ? 'notch' : 'notches'}
        </span>
      </div>
    </div>

    <div class="row">
      <span class="row-text">
        <label class="row-label" for="scroll-frames">Maximum frames</label>
        <span class="row-help">
          A hard stop, so a page that never stops scrolling cannot run forever. Reaching it keeps
          everything stitched so far and says the capture is the top of the page rather than all
          of it.
        </span>
      </span>
      <div class="field">
        <input
          id="scroll-frames"
          class="range"
          type="range"
          min={SCROLL_MAX_FRAMES.min}
          max={SCROLL_MAX_FRAMES.max}
          step={SCROLL_MAX_FRAMES.step}
          bind:value={scrollFramesDraft}
          onchange={commitScrollFrames}
        />
        <span class="range-value mono num">{scrollFramesDraft} frames</span>
      </div>
    </div>
  </section>

  <section class="section" aria-labelledby="sec-editor">
    <h2 class="section-title" id="sec-editor">Editor</h2>
    <p class="section-help">
      What a new annotation starts as. The editor's toolbar overrides both, per shape.
    </p>

    <div class="row">
      <span class="row-text">
        <span class="row-label" id="lbl-editor-color">Default colour</span>
        <span class="row-help">Used for arrows, shapes, text and step badges.</span>
      </span>
      <div class="field">
        <div class="swatches" role="radiogroup" aria-labelledby="lbl-editor-color">
          {#each EDITOR_COLORS as color (color)}
            <button
              type="button"
              class="swatch"
              class:selected={sameColor(s.editorDefaultColor, color)}
              role="radio"
              aria-checked={sameColor(s.editorDefaultColor, color)}
              aria-label="Use {color}"
              title={color}
              style="--swatch: {color}"
              onclick={() => patch({ editorDefaultColor: color })}
            ></button>
          {/each}
        </div>
        <input
          class="color"
          type="color"
          aria-label="Custom default annotation colour"
          value={s.editorDefaultColor}
          onchange={(e) => patch({ editorDefaultColor: e.currentTarget.value })}
        />
      </div>
    </div>

    <div class="row">
      <span class="row-text">
        <label class="row-label" for="editor-stroke">Default stroke width</label>
        <span class="row-help">In image pixels, so it stays constant however far you zoom.</span>
      </span>
      <div class="field">
        <span
          class="stroke-preview"
          aria-hidden="true"
          style="height: {strokeDraft}px; background: {s.editorDefaultColor}"
        ></span>
        <input
          id="editor-stroke"
          class="range"
          type="range"
          min="1"
          max="24"
          step="1"
          bind:value={strokeDraft}
          onchange={commitStroke}
        />
        <span class="stroke-value mono num">{strokeDraft} px</span>
      </div>
    </div>
  </section>

  <section class="section" aria-labelledby="sec-files">
    <h2 class="section-title" id="sec-files">Files</h2>

    <div class="row stack">
      <span class="row-text">
        <label class="row-label" for="save-dir">Save directory</label>
        <span class="row-help">Where captures are written when saving to disk is on.</span>
      </span>
      <div class="field wide">
        <input
          id="save-dir"
          class="input mono"
          type="text"
          spellcheck="false"
          bind:value={dirDraft}
          onchange={commitDir}
          onblur={commitDir}
        />
        <button type="button" class="btn" onclick={browse}>Browse…</button>
        <button type="button" class="btn" onclick={revealSaveDir}>Open folder</button>
      </div>
    </div>

    <div class="row stack">
      <span class="row-text">
        <label class="row-label" for="filename-pattern">Filename pattern</label>
        <span class="row-help">Tokens are replaced at capture time; anything else is kept.</span>
      </span>
      <div class="field wide">
        <input
          id="filename-pattern"
          class="input mono"
          type="text"
          spellcheck="false"
          aria-describedby="pattern-preview pattern-legend"
          bind:value={patternDraft}
          onchange={commitPattern}
          onblur={commitPattern}
        />
      </div>
      <p class="preview" id="pattern-preview">
        <span class="preview-label">Preview</span>
        <span class="mono num">{filenamePreview}</span>
      </p>
      <p class="legend" id="pattern-legend">
        {#each PATTERN_TOKENS as token (token)}
          <code>{token}</code>
        {/each}
      </p>
    </div>
  </section>

  <section class="section" aria-labelledby="sec-uploading">
    <h2 class="section-title" id="sec-uploading">Uploading</h2>
    <p class="section-help">
      Uploading is opt-in, one capture at a time: the <strong>Upload</strong> action on a capture
      is the only thing that sends it anywhere, unless you change that below.
    </p>

    <div class="row">
      <span class="row-text">
        <span class="row-label" id="lbl-auto-upload">Upload every capture automatically</span>
        <span class="row-help">
          Off, nothing leaves this machine unless you press Upload on a capture. On, every single
          capture is sent to
          {#if s.destination && s.destination !== 'none'}
            <strong>{DESTINATION_LABEL[s.destination]}</strong>
          {:else}
            the destination you configure
          {/if}
          as soon as it is taken.
        </span>
      </span>
      <Toggle
        checked={s.autoUpload === true}
        labelledBy="lbl-auto-upload"
        onchange={requestAutoUpload}
      />
    </div>

    <!-- Not a toggle with a shrug (M3 §1). The switch above stays off until this
         is accepted, and the sentence that matters — hotkey captures taken while
         you are doing something else — is stated rather than implied. -->
    {#if confirmAutoUpload}
      <div class="confirm" role="alertdialog" aria-labelledby="auto-upload-title">
        <p class="confirm-title" id="auto-upload-title">
          Send every capture to
          {s.destination && s.destination !== 'none'
            ? DESTINATION_LABEL[s.destination]
            : 'the configured destination'}?
        </p>
        <p class="confirm-body">
          With this on, <strong>every capture is uploaded the moment it is taken</strong> — including
          the ones you take with a hotkey while you are working in another app, and including
          anything that happens to be on screen at the time. You will not be asked again, per
          capture or otherwise.
        </p>
        <p class="confirm-body">
          Whatever the capture contains — a password manager, a private message, a colleague's
          screen share — goes to
          {#if s.destination && s.destination !== 'none'}
            <strong>{DESTINATION_LABEL[s.destination]}</strong>
          {:else}
            whichever destination is configured
          {/if}
          along with it. Leave this off and press Upload on the captures you actually want to
          share.
        </p>
        {#if !s.destination || s.destination === 'none'}
          <p class="confirm-body">
            No destination is configured yet, so nothing will be sent until you choose one — but
            the moment you do, every capture starts going there.
          </p>
        {/if}
        <div class="confirm-actions">
          <button type="button" class="btn" onclick={() => (confirmAutoUpload = false)}>
            Keep it off
          </button>
          <button type="button" class="btn danger" onclick={() => void acceptAutoUpload()}>
            Upload every capture
          </button>
        </div>
      </div>
    {/if}

    <div class="row">
      <span class="row-text">
        <span class="row-label" id="lbl-copy-url">Copy the link after uploading</span>
        <span class="row-help">
          Put the returned URL on the clipboard as soon as an upload finishes, so it is ready to
          paste. The link is also kept on the capture, where History can copy it again later.
        </span>
      </span>
      <Toggle
        checked={s.copyUrlAfterUpload !== false}
        labelledBy="lbl-copy-url"
        onchange={(v) => patch({ copyUrlAfterUpload: v })}
      />
    </div>
  </section>

  {#if showDestinations}
    <DestinationsView variant="section" />
  {/if}

  <section class="section" aria-labelledby="sec-hotkeys">
    <h2 class="section-title" id="sec-hotkeys">Hotkeys</h2>
    <p class="section-help">
      Click a shortcut and press the combination you want. Escape cancels.
    </p>

    {#if hotkeyError}
      <p class="warning" role="status">
        <svg
          viewBox="0 0 16 16"
          width="16"
          height="16"
          fill="none"
          stroke="currentColor"
          stroke-width="1.5"
          stroke-linecap="round"
          stroke-linejoin="round"
          aria-hidden="true"
        >
          <path d="M8 2.75 14.5 13.5h-13z" />
          <path d="M8 6.75v3" />
          <path d="M8 11.75h.01" />
        </svg>
        <span>{hotkeyError}</span>
      </p>
    {/if}

    {#each HOTKEY_ROWS as row (row.key)}
      {@const status = statusFor(row.key, s.hotkeys[row.key])}
      <div class="row">
        <span class="row-text">
          <span class="label-line">
            <span class="row-label" id="lbl-hk-{row.key}">{row.label}</span>
            {#if status}
              <span
                class="mech"
                class:hook={status.mechanism === 'hook'}
                class:unbound={!status.bound}
                title={MECHANISM_META[status.mechanism].title}
              >
                {MECHANISM_META[status.mechanism].label}
              </span>
            {/if}
          </span>
          <span class="row-help">{row.help}</span>
          {#if status && !status.bound}
            <span class="row-danger">
              {status.error ??
                'Nothing is listening for this shortcut. Pick another combination, or turn on the keyboard hook fallback below.'}
            </span>
          {/if}
        </span>
        <div class="field">
          <button
            type="button"
            class="hotkey"
            class:recording={recording === row.key}
            aria-labelledby="lbl-hk-{row.key}"
            aria-describedby="hk-value-{row.key}"
            onclick={() => (recording = recording === row.key ? null : row.key)}
            onblur={() => {
              if (recording === row.key) recording = null;
            }}
          >
            {#if recording === row.key}
              <span class="listening">Press keys…</span>
            {:else}
              <span class="keys" id="hk-value-{row.key}">
                {#each displayAccel(s.hotkeys[row.key]) as part, i (i)}
                  <kbd>{part}</kbd>
                {/each}
              </span>
            {/if}
          </button>
          <button
            type="button"
            class="btn subtle"
            aria-label="Reset {row.label} shortcut to default"
            onclick={() => resetHotkey(row.key)}
          >
            Reset
          </button>
        </div>
      </div>
    {/each}

    <div class="row">
      <span class="row-text">
        <span class="row-label" id="lbl-hook">Claim hotkeys other apps own</span>
        <span class="row-help">
          Windows gives a hotkey to one app only, so a combination something else already holds —
          <kbd class="inline">Win</kbd><kbd class="inline">Shift</kbd><kbd class="inline">S</kbd>,
          or <kbd class="inline">PrintScreen</kbd> while the real ShareX is running — can be claimed
          only through a low-level keyboard hook. santi.sharex installs one as a fallback, for the combos
          that fail to register normally, and it matches only the combinations you configure here;
          it never records keystrokes. Off, those combos stay unbound.
        </span>
        <span class="row-note">
          Read “How santi.sharex binds hotkeys — and what its keyboard hook does” in the README for its
          exact scope.
        </span>
      </span>
      <Toggle
        checked={s.useLowLevelHotkeys}
        labelledBy="lbl-hook"
        onchange={(v) => patch({ useLowLevelHotkeys: v })}
      />
    </div>
  </section>

  <section class="section" aria-labelledby="sec-about">
    <h2 class="section-title" id="sec-about">About</h2>
    <div class="about">
      <p class="about-line">
        <span class="about-name">santi.sharex</span>
        <span class="mono num">v{version || '—'}</span>
      </p>
      <p class="about-body">
        A ShareX-style screen capture tool for Windows: region, fullscreen and window shots,
        straight to disk and clipboard, with a history you can search.
      </p>
      <p class="about-note">
        Anthropic Sans ships with the Claude themes and is used only while one of them is active.
      </p>
    </div>
  </section>
  {/if}
</div>

<style>
  /* The shell pane has no padding of its own — every view owns its inset. The
     max-width applies to the sections, not to the full-bleed header. */
  .page {
    display: flex;
    flex-direction: column;
    gap: 32px;
    padding: 0 32px 48px;
  }

  .page > .section {
    width: 100%;
    max-width: 760px;
  }

  /* Bled back out to the pane edges so the pinned header and its hairline span
     the full width, the way the capture view's header does. */
  .page-header {
    position: sticky;
    top: 0;
    z-index: 4;
    margin: 0 -32px 0;
    padding: 24px 32px 16px;
    background: var(--bg);
    border-bottom: 1px solid var(--border);
  }

  .loading {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 40px 0;
    color: var(--text-dim);
  }

  .loading .err {
    color: var(--danger);
  }

  .page-title {
    margin: 0;
    font-family: var(--font-display);
    font-size: 20px;
    font-weight: 600;
    line-height: 1.2;
    color: var(--text);
  }

  .section {
    display: flex;
    flex-direction: column;
  }

  .section-title {
    margin: 0 0 4px;
    font-family: var(--font-display);
    font-size: 13px;
    font-weight: 600;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--text-faint);
  }

  .section-help {
    margin: 0 0 12px;
    font-size: 12px;
    color: var(--text-dim);
  }

  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 24px;
    padding: 14px 0;
    border-bottom: 1px solid var(--border);
  }

  .row.stack {
    flex-direction: column;
    align-items: stretch;
    gap: 8px;
  }

  .row:last-child {
    border-bottom: none;
  }

  .row-text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .row-label {
    font-size: 13px;
    color: var(--text);
  }

  .row-help {
    font-size: 12px;
    line-height: 1.6;
    color: var(--text-faint);
  }

  .row-note {
    font-size: 12px;
    line-height: 1.6;
    color: var(--text-dim);
  }

  /* Why a hotkey is doing nothing. Sits under the row's own help text, in the
     same colour as the section-level warning, because an unbound hotkey is a
     failure and not a footnote. */
  .row-danger {
    margin-top: 4px;
    font-size: 12px;
    line-height: 1.5;
    color: var(--danger);
  }

  .label-line {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 8px;
  }

  /* How the hotkey beside it is bound. Quiet for the two working mechanisms —
     it is reassurance, not news — and loud for `none`, which is the whole
     reason this chip exists. */
  .mech {
    flex: none;
    padding: 0 7px;
    font-size: 11px;
    line-height: 18px;
    white-space: nowrap;
    color: var(--text-dim);
    background: var(--bg-inset);
    border: 1px solid var(--border);
    border-radius: 999px;
    cursor: default;
  }

  .mech.hook {
    color: var(--accent);
    background: color-mix(in srgb, var(--accent) 12%, transparent);
    border-color: color-mix(in srgb, var(--accent) 40%, transparent);
  }

  .mech.unbound {
    font-weight: 600;
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 14%, transparent);
    border-color: color-mix(in srgb, var(--danger) 50%, transparent);
  }

  .field {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: none;
  }

  .field.wide {
    flex: 1;
  }

  .input {
    flex: 1;
    min-width: 0;
    height: 34px;
    padding: 0 10px;
    font: inherit;
    font-size: 13px;
    color: var(--text);
    background: var(--bg-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    transition: border-color 140ms ease;
  }

  .input:hover {
    border-color: var(--border-strong);
  }

  .input:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
    border-color: var(--accent);
  }

  .mono {
    font-family: var(--font-mono);
    font-size: 12px;
  }

  .swatches {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  /* `--swatch` is an annotation colour, set inline per button: content painted
     onto the capture, not chrome, so it is a literal rather than a token. */
  .swatch {
    width: 22px;
    height: 22px;
    padding: 0;
    border: 1px solid var(--border-strong);
    border-radius: 50%;
    background: var(--swatch);
    cursor: pointer;
    transition:
      transform 140ms ease,
      box-shadow 140ms ease;
  }

  .swatch:hover {
    transform: scale(1.1);
  }

  .swatch:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  /* Two rings: one in the page background to hold the swatch apart from the
     accent ring, so the selection reads on a swatch of any colour. */
  .swatch.selected {
    box-shadow:
      0 0 0 2px var(--bg),
      0 0 0 4px var(--accent);
  }

  .color {
    width: 34px;
    height: 34px;
    flex: none;
    padding: 2px;
    background: var(--bg-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    cursor: pointer;
    transition: border-color 140ms ease;
  }

  .color:hover {
    border-color: var(--border-strong);
  }

  .color:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .range {
    width: 160px;
    accent-color: var(--accent);
    cursor: pointer;
  }

  .range:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 4px;
  }

  /* Height is bound to the stroke width, so the bar is a 1:1 sample of it. */
  .stroke-preview {
    width: 44px;
    flex: none;
    border-radius: 999px;
  }

  .stroke-value {
    min-width: 44px;
    text-align: right;
    color: var(--text-dim);
  }

  /* Same readout as .stroke-value, wider because the units are words: the
     number must not shuffle sideways as the slider moves. */
  .range-value {
    min-width: 84px;
    text-align: right;
    color: var(--text-dim);
  }

  .num {
    font-variant-numeric: tabular-nums;
  }

  .btn {
    height: 34px;
    padding: 0 12px;
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    font: inherit;
    font-size: 13px;
    color: var(--text);
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    cursor: pointer;
    transition:
      color 140ms ease,
      background-color 140ms ease,
      border-color 140ms ease;
  }

  .btn:hover {
    border-color: var(--border-strong);
    background: var(--bg-inset);
  }

  .btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .btn.subtle {
    color: var(--text-dim);
    background: transparent;
    border-color: transparent;
  }

  .btn.subtle:hover {
    color: var(--text);
    border-color: var(--border);
  }

  .btn.danger {
    color: var(--danger);
    border-color: color-mix(in srgb, var(--danger) 50%, transparent);
  }

  .btn.danger:hover {
    color: var(--accent-text);
    background: var(--danger);
    border-color: var(--danger);
  }

  /* The auto-upload confirmation. Loud on purpose: this is the one control here
     that can put the screen on the network without being asked again, and a
     quiet inline note would read as a footnote to a switch that is already on. */
  .confirm {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin: 4px 0 14px;
    padding: 14px 16px;
    color: var(--text-dim);
    background: color-mix(in srgb, var(--danger) 8%, var(--surface));
    border: 1px solid color-mix(in srgb, var(--danger) 45%, transparent);
    border-radius: var(--radius-lg);
  }

  .confirm-title {
    margin: 0;
    font-family: var(--font-display);
    font-size: 14px;
    font-weight: 600;
    color: var(--danger);
  }

  .confirm-body {
    margin: 0;
    max-width: 62ch;
    font-size: 12px;
    line-height: 1.6;
  }

  .confirm-body strong {
    color: var(--text);
  }

  .confirm-actions {
    display: flex;
    gap: 8px;
    margin-top: 4px;
  }

  .preview {
    margin: 0;
    display: flex;
    align-items: baseline;
    gap: 8px;
    color: var(--text);
  }

  .preview-label {
    font-size: 11px;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--text-faint);
  }

  .legend {
    margin: 0;
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .legend code {
    font-family: var(--font-mono);
    font-size: 11px;
    line-height: 18px;
    padding: 0 6px;
    color: var(--text-dim);
    background: var(--bg-inset);
    border: 1px solid var(--border);
    border-radius: 5px;
  }

  .warning {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    margin: 0 0 12px;
    padding: 10px 12px;
    font-size: 12px;
    line-height: 1.5;
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 10%, transparent);
    border: 1px solid color-mix(in srgb, var(--danger) 40%, transparent);
    border-radius: var(--radius);
  }

  .warning svg {
    flex: none;
    margin-top: 1px;
  }

  .hotkey {
    min-width: 190px;
    height: 34px;
    padding: 0 10px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 4px;
    font: inherit;
    font-size: 13px;
    color: var(--text);
    background: var(--bg-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    cursor: pointer;
    transition:
      border-color 140ms ease,
      background-color 140ms ease;
  }

  .hotkey:hover {
    border-color: var(--border-strong);
  }

  .hotkey:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .hotkey.recording {
    border-color: var(--accent);
    background: color-mix(in srgb, var(--accent) 12%, transparent);
  }

  .listening {
    font-size: 12px;
    color: var(--accent);
  }

  .keys {
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }

  kbd {
    font-family: var(--font);
    font-size: 12px;
    line-height: 20px;
    padding: 0 6px;
    color: var(--text);
    background: var(--surface);
    border: 1px solid var(--border);
    border-bottom-width: 2px;
    border-radius: 5px;
  }

  /* Keys named inside a sentence rather than shown as a binding: smaller, and
     spaced so a run of them does not read as one wide key. */
  kbd.inline {
    margin: 0 1px;
    padding: 0 4px;
    font-size: 11px;
    line-height: 16px;
    color: var(--text-dim);
  }

  /* Four cards. Two rows at the pane's narrow widths, one row once there is
     room — never three across, which would strand the fourth card alone.
     Keyed off the window because the sidebar is a fixed 224px, so pane width
     tracks it exactly. */
  .themes {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 12px;
  }

  @media (min-width: 1040px) {
    .themes {
      grid-template-columns: repeat(4, minmax(0, 1fr));
    }
  }

  .theme {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 8px;
    text-align: left;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    cursor: pointer;
    transition:
      border-color 140ms ease,
      background-color 140ms ease;
  }

  .theme:hover {
    border-color: var(--border-strong);
  }

  .theme:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .theme.selected {
    border-color: var(--accent);
    box-shadow: inset 0 0 0 1px var(--accent);
  }

  /* Corners are scaled from the previewed theme's own radii, not fixed: at this
     size the difference between 14px and 4px is the clearest tell that `sharex`
     is a square-cornered theme. */
  .mock {
    aspect-ratio: 16 / 10;
    display: grid;
    grid-template-columns: 34% 1fr;
    overflow: hidden;
    border-radius: calc(var(--m-radius-lg, 14px) * 0.7);
    background: var(--m-bg, var(--bg));
    border: 1px solid var(--m-border, var(--border));
  }

  .mock-side {
    display: flex;
    flex-direction: column;
    gap: 5px;
    padding: 9px 7px;
    background: var(--m-bg-raised, var(--bg-raised));
    border-right: 1px solid var(--m-border, var(--border));
  }

  .mock-brand {
    height: 6px;
    width: 62%;
    border-radius: calc(var(--m-radius, 8px) * 0.4);
    background: var(--m-text, var(--text));
  }

  .mock-pill {
    height: 5px;
    border-radius: calc(var(--m-radius, 8px) * 0.4);
    background: var(--m-text-dim, var(--text-dim));
    opacity: 0.4;
  }

  .mock-pill.active {
    background: var(--m-accent, var(--accent));
    opacity: 1;
  }

  .mock-main {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 9px;
  }

  .mock-head {
    height: 7px;
    width: 52%;
    border-radius: calc(var(--m-radius, 8px) * 0.4);
    background: var(--m-text, var(--text));
  }

  .mock-card {
    flex: 1;
    border-radius: calc(var(--m-radius, 8px) * 0.7);
    background: var(--m-surface, var(--surface));
    border: 1px solid var(--m-border, var(--border));
  }

  .mock-btn {
    height: 11px;
    width: 42%;
    border-radius: calc(var(--m-radius, 8px) * 0.7);
    background: var(--m-accent, var(--accent));
  }

  .theme-meta {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 0 2px 2px;
  }

  .theme-name {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
    font-weight: 500;
    color: var(--text);
  }

  .theme.selected .theme-name svg {
    color: var(--accent);
  }

  .theme-blurb {
    font-size: 12px;
    color: var(--text-faint);
  }

  .about {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 16px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
  }

  .about-line {
    margin: 0;
    display: flex;
    align-items: baseline;
    gap: 8px;
  }

  .about-name {
    font-family: var(--font-display);
    font-size: 15px;
    font-weight: 600;
    color: var(--text);
  }

  .about-line .mono {
    color: var(--text-faint);
  }

  .about-body,
  .about-note {
    margin: 0;
    font-size: 12px;
    line-height: 1.6;
    color: var(--text-dim);
  }

  .about-note {
    color: var(--text-faint);
  }
</style>
