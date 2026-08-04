# santi.sharex

A ShareX-style screen capture tool for Windows, rebuilt on Tauri v2 with a UI
that isn't from 2007. Four selectable themes: a modern dark default, a Claude
theme in warm paper and clay set in Anthropic Sans, its after-dark twin, and the
`sharex` theme, a deliberate replica of ShareX's own look for the muscle memory.

> **A note on the name.** This app is called **santi.sharex**. **ShareX** —
> unqualified, capitalised that way — always means the original C#/WinForms
> application by Jaex, and `sharex` in code font means this app's ShareX-replica
> *theme*. Three different things, kept distinct throughout these docs.

ShareX itself is C#/WinForms, so santi.sharex is not a skin over it — it's a
reimplementation of the parts that matter, in Rust + Svelte.

## Status

| Milestone | | |
|---|---|---|
| **M1** | Capture core, history, themes | ✅ done |
| **M2** | Annotation editor | ✅ done |
| **M2.5** | Pre-warmed overlay, window auto-detect, annotate in place | ✅ done |
| **M2.6** | Hotkeys that bind, start to tray, top toolbar, two more themes | ✅ done |
| M3 | Upload destinations (Imgur, `.sxcu`, FTP) | planned |
| M4 | Screen recording (MP4/GIF) | planned |
| M5 | OCR, scrolling capture, workflows | planned |

### M1 — capture

- **Region select** — freezes the desktop, then a crosshair overlay with a pixel
  magnifier, live size readout, arrow-key nudge, and Enter to commit
- **Fullscreen** (all monitors composited), **active window**, **per-monitor**,
  and a picker listing every open window
- **History** — thumbnail gallery, search and filter, lightbox with arrow-key
  navigation, copy / open / reveal / delete
- **Settings** — save directory, filename pattern with live preview,
  click-to-record hotkeys, capture toggles
- **Tray + global hotkeys**, close-to-tray

### M2 — editor

Arrow, rectangle, ellipse, line, pen, text, highlighter, redaction
(blur/pixelate), auto-incrementing step counter, and non-destructive crop.
Undo/redo, zoom 10–800%, pan, select-and-resize. Save in place, save as a new
capture, or copy straight to the clipboard.

## How santi.sharex binds hotkeys — and what its keyboard hook does

Read this. It is the one part of santi.sharex that touches your keyboard
globally, and you should know exactly how far it reaches.

Windows hands a hotkey to whichever process asks for it first, through
`RegisterHotKey`. That is what santi.sharex tries for each of your three hotkeys,
and for a free combo it is the whole story. But it cannot take a combo something
else already owns — `Win+Shift+S` belongs to the Windows snipping shell, and
`PrintScreen` / `Alt+PrintScreen` belong to the original ShareX whenever it is
running. Those simply fail to register. It is also why ShareX can bind
`Win+Shift+S` and a `RegisterHotKey`-only app never can.

So for **only the combos that fail**, santi.sharex installs a low-level keyboard hook
(`WH_KEYBOARD_LL`), which sees keys before the shell's hotkey dispatch. Which
mechanism won for each hotkey is reported by the `get_hotkey_status` command and
on the `hotkeys://status` event (`"plugin"`, `"hook"` or `"none"`), and the whole
fallback is controlled by `useLowLevelHotkeys` in `settings.json` — set it to
`false` for plugin-only, i.e. the pre-M2.6 behaviour, in which case the combos
the OS refuses stay unbound.

Settings › Hotkeys surfaces both: each row carries a chip reading **Bound**,
**Bound via hook** or **Not bound**, and *Claim hotkeys other apps own* is the
`useLowLevelHotkeys` switch.

A hook like this is the same machinery a keylogger uses, so here is its exact
scope, enforced in `src-tauri/src/hotkeys.rs`:

- **No keystroke is logged, stored, buffered, counted or sent anywhere.** There
  is no key history in the code — no buffer, no file, no event carrying a key.
- The hook holds one thing: *your own configured combos*. Every key event is
  compared against that short list and immediately forgotten.
- A key that is not one of your combos is passed straight on to the rest of the
  system on the first branch, untouched.
- It keeps no key state between events. The modifier flags it needs are read
  from the OS on demand, and only after a candidate key already matched.
- It does no capture work inline — it hands the action to a worker thread and
  returns, because blocking inside a low-level hook stalls typing system-wide.
- It runs on its own thread, is installed at startup, and is uninstalled on
  shutdown and before every rebind.

A screenshot tool binding hotkeys you chose is legitimate. A hidden keyboard hook
is not, and the difference is disclosure and scope — hence this section.

### Default hotkeys

| | |
|---|---|
| Region | `Ctrl+Shift+1` |
| Fullscreen | `Ctrl+Shift+2` |
| Active window | `Ctrl+Shift+3` |

Deliberately boring, deliberately free: a fresh install works without a trip to
Settings. The hook makes `Win+Shift+S` and `PrintScreen` *bindable* if you want
them, but silently taking the OS snipping shortcut from someone who never asked
would be the wrong default.

## Starting to tray

santi.sharex launches hidden, with only its tray icon — as ShareX does. Left-click the
tray icon to open the window, right-click for the capture menu and Quit. Closing
the window hides it again rather than quitting. Turn *Start hidden in the tray*
off under Settings › Startup (or set `startHidden` to `false` in `settings.json`)
if you want the window at launch.

If the tray icon cannot be created, santi.sharex shows its window instead and closing
that window quits — an app with no window *and* no tray would be reachable only
through Task Manager.

## Running it

```sh
pnpm install
pnpm tauri dev
```

Do not remove `pnpm-workspace.yaml`. Without it pnpm walks up, finds the one in
the home directory, and installs `node_modules` outside the project.

## Architecture

`docs/ARCHITECTURE.md` (M1), `docs/ARCHITECTURE-M2.md` (M2),
`docs/ARCHITECTURE-M2.5.md` and `docs/ARCHITECTURE-M2.6.md` are the binding
contracts — exact command names, type shapes, event names, and design tokens.
Read them before changing anything that crosses the Rust/TypeScript seam.

Two things in there are load-bearing and easy to "fix" back into bugs:

- **Extra windows are query params, not routes** (`?w=overlay`, `?w=editor`).
  `adapter-static` never emits `/overlay.html`, and the `index.html?…` form
  404s under `tauri dev` because SvelteKit's dev middleware has no root
  `index.html`. The bare `?w=…` form is the one that works in both.
- **All geometry is stored in image pixels.** The region overlay derives its
  scale at runtime from `freeze.width / window.innerWidth` rather than trusting
  `devicePixelRatio`, and the editor converts pointer coordinates in exactly one
  helper. This is what keeps captures pixel-accurate across DPI settings.

## Known rough edges

Untested at runtime, in rough order of likelihood:

1. **A hotkey may still not fire** — if `RegisterHotKey` refuses it *and* the
   low-level hook is switched off (or cannot install), the combo stays unbound.
   Registration failures surface as a toast; Settings › Hotkeys shows which
   mechanism owns each one.
2. **Text nudges on commit** — the inline `<textarea>` and canvas
   `textBaseline: 'top'` disagree by a font-dependent fraction of an em.
3. **Large-capture performance** — the editor deep-clones shapes per repaint, and
   Copy/Save pushes a base64 PNG (15–25 MB on a 4K capture) through the WebView2
   IPC in one string.
4. **Mixed-DPI multi-monitor** — crop math is DPI-safe by construction, but
   overlay *coverage* of the virtual desktop is not guaranteed.

## Fonts — why the Claude themes look different here

The `claude` and `claude-dark` themes are designed for **Anthropic Sans**, which
is Anthropic's proprietary brand typeface. It is deliberately **not** committed
to this repository — shipping it here would be redistributing it — so
`static/fonts/` is empty in a fresh clone and `.gitignore` keeps it that way.

Nothing breaks without it. Every `--font` stack falls back through
`Segoe UI Variable Text` → `Segoe UI` → `system-ui`, so the Claude themes simply
render in the system face. The other two themes never used it.

To use the real thing locally, drop these eight files into `static/fonts/`:

```
AnthropicSans-Display-Medium-Static.otf    AnthropicSans-Text-Regular-Static.otf
AnthropicSans-Display-Semibold-Static.otf  AnthropicSans-Text-Medium-Static.otf
AnthropicSans-Display-Bold-Static.otf      AnthropicSans-Text-Semibold-Static.otf
                                           AnthropicSans-Text-Bold-Static.otf
                                           AnthropicSans-Text-RegularItalic-Static.otf
```

They are picked up on the next build. Check the licence before distributing any
build that includes them.
