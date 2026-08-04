# santi.sharex — M2.6: hotkeys that actually bind, ShareX-shaped capture, more themes

Extends `ARCHITECTURE.md` (M1), `ARCHITECTURE-M2.md` (M2) and
`ARCHITECTURE-M2.5.md` (M2.5), all still authoritative. Five changes, all from
using the app against real ShareX.

---

## 1. Hotkeys that bind even when Windows owns them

**The problem.** `RegisterHotKey` — what `tauri-plugin-global-shortcut` uses —
fails for a combo another process already owns. Today all three defaults fail:
`Win+Shift+S` is owned by the Windows shell (Snipping Tool), `PrintScreen` and
`Alt+PrintScreen` by ShareX. The user's report that **Win+Shift+S always worked
in ShareX** is the tell: `RegisterHotKey` cannot do that, so a capture tool that
binds it is not using `RegisterHotKey`.

**The fix.** A low-level keyboard hook (`WH_KEYBOARD_LL`) as a *fallback*, in a
new `src-tauri/src/hotkeys.rs`. A low-level hook sees keys before the shell's
hotkey dispatch, so it can claim a combo the shell owns and suppress the
original by returning 1 from the hook procedure.

### This is keylogger-shaped machinery. Build it so it isn't one.

These constraints are requirements, not suggestions, and the review stage
checks them:

- The hook matches **only** the currently-configured santi.sharex combos. Every other
  key returns `CallNextHookEx` immediately, on the first branch.
- **No keystroke is ever logged, stored, buffered, counted, or sent anywhere.**
  No `Vec<Key>`, no history, no file, no event payload carrying a key. The hook
  compares against the configured set and either fires a capture or passes on.
- The only state it may keep is the live modifier flags needed to match a combo.
- It runs on its own dedicated thread with its own message loop, installed at
  startup and uninstalled on shutdown and on every settings change that
  rebinds.
- The hook procedure does **no** capture work: it posts to the app and returns.
  Blocking inside `WH_KEYBOARD_LL` stalls input system-wide.

Document it plainly in the README under a heading the user will find. A
screenshot tool binding the user's own chosen hotkeys is legitimate; a hidden
keyboard hook is not, and the difference is entirely disclosure and scope.

### Strategy

1. Try `RegisterHotKey` (the existing plugin) first — cheaper, and the OS
   handles matching.
2. Only for combos that fail, install them in the hook.
3. Report per-hotkey which mechanism won, so Settings can show it.

Needs `windows = { version = "0.58", features = [...] }` (or whatever version
resolves) for `SetWindowsHookExW`, `CallNextHookEx`, `UnhookWindowsHookEx`,
`KBDLLHOOKSTRUCT`, `GetAsyncKeyState`, and a message loop.

`Settings` gains:

```rust
pub use_low_level_hotkeys: bool,  // default true, #[serde(default)]
```

Off means "plugin only", i.e. today's behaviour. Surfaced in Settings › Hotkeys
with an honest one-line explanation of what it does and why it's needed.

### Defaults

Change the shipped defaults to combos that are actually free, so a fresh
install works without a trip to Settings:

```
region        Ctrl+Shift+1
fullscreen    Ctrl+Shift+2
active window Ctrl+Shift+3
```

The hook makes `Win+Shift+S` and `PrintScreen` *bindable*, but they must not be
defaults — silently stealing the OS snipping shortcut from a user who didn't
ask is the wrong default. Settings can offer them.

---

## 2. Start to tray

Launching santi.sharex must **not** pop the main window. It starts hidden with only
its tray icon, exactly like ShareX.

```rust
pub start_hidden: bool,  // default true, #[serde(default)]
```

- At startup, when `start_hidden` is true, never show `main`. The window is
  still created (the app shell must exist for the stores and event listeners),
  just not shown.
- The tray icon is the way in: left-click shows and focuses `main`, and the
  existing tray menu keeps working.
- Closing the window still hides rather than quits (M1 behaviour, unchanged).
- `tauri.conf.json`'s `main` window gets `"visible": false` so there is no flash
  of a window at launch before Rust decides — a window shown and then hidden a
  frame later is visible to the user as a flicker.

Because the app now launches invisibly, the tray icon is the *only* affordance.
Its tooltip must say "santi.sharex" and the menu must include Show.

---

## 3. The capture toolbar goes to the top

M2.5 anchored the annotation toolbar to the selection. The user wants ShareX's
arrangement: **a single horizontal bar pinned to the top of the overlay**,
present as soon as the overlay arms — before a selection exists — so the tool
is chosen first and the region drawn second, ShareX-style.

Changes to `OverlayToolbar.svelte` and `RegionOverlay.svelte`:

- The bar is fixed to the top edge, horizontally centred, with a small gap. It
  no longer flips or follows the selection.
- It is visible in `armed` and `selecting`, not only `annotating` — picking a
  tool before dragging is the point.
- It must not eat the region under it: while the pointer is over the bar, the
  crosshair and window-highlight are suppressed, and a drag started on the bar
  does not begin a selection.
- Dense and icon-only, like the reference: ~28px square buttons, a tight row,
  dividers between groups, tooltips carrying the shortcut. Groups in order:
  selection tools, shapes, drawing, text/step, redaction, then colour/stroke,
  then Undo/Redo, then Cancel and Capture.
- Keyboard focus must stay with the overlay: `preventDefault` on the bar's
  `pointerdown` so key handling is uninterrupted.

Everything else about M2.5's overlay — arm/ready, window auto-detect, the
Escape ladder, the native-crop fast path when nothing was drawn — is unchanged
and must not regress.

---

## 4. Two new themes

`Settings.theme` widens from `"dark" | "claude"` to:

```
"dark" | "claude" | "claude-dark" | "sharex"
```

Update the Rust default/validation, the TS `Theme` union, the pre-paint script
in `app.html`, and the Settings theme picker — which becomes four preview
cards, each a miniature of the real UI in that theme's own colours.

### `claude-dark`

The Claude theme after dark. Warm, flat, same clay accent — not the dark theme
with different numbers.

```
--bg #262624   --bg-raised #30302e  --bg-inset #1f1e1d  --surface #30302e
--border #3e3e3a  --border-strong #4e4e48
--text #faf9f5  --text-dim #b4b2a7  --text-faint #8a887e
--accent #c96442  --accent-hover #d97757  --accent-text #ffffff
--danger #e0796a  --success #7fae8c
--blur none      (flat, like its light twin)
--font/-display  Anthropic Sans
```

### `sharex`

A deliberate replica of ShareX's own chrome, for people who want the muscle
memory. Segoe UI, near-square corners, tight density, cool grays, Windows blue.

```
--bg #1c1c1c   --bg-raised #2b2b2b  --bg-inset #171717  --surface #2b2b2b
--border #3f3f3f  --border-strong #565656
--text #dcdcdc  --text-dim #9a9a9a  --text-faint #6f6f6f
--accent #0078d7  --accent-hover #1f8ae0  --accent-text #ffffff
--danger #d13438  --success #4ec36a
--radius 3px   --radius-lg 4px      (ShareX is not a rounded app)
--blur none
--font/-display  "Segoe UI", "Segoe UI Variable", system-ui
```

**`--radius` and `--radius-lg` must move out of the shared block and be defined
per theme**, since `sharex` overrides them. Every token still has to be defined
in all four blocks — a token present in three is a bug.

Order in `app.css`, last in the file: `dark`, `claude`, `claude-dark`,
`sharex`.

The theme picker's four cards must each paint themselves in their own palette
using literal values, so the choice is visible before committing — that is the
documented exception to the no-hard-coded-colour rule, alongside the annotation
palette.

---

## 5. What must not regress

- M1 capture, history, tray, settings. M2 editor. M2.5 overlay behaviour,
  including the Escape ladder and the native-crop fast path.
- `cargo check`, `pnpm check`, `pnpm build` stay clean.
- A settings file written by any earlier milestone still loads: every new field
  carries `#[serde(default)]`, and an unknown `theme` string falls back to
  `dark` rather than rendering an untokenised page.
