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
| **M5** | OCR, scrolling capture, smart eraser | ✅ done |
| **M2.7** | ShareX's own layout for the `sharex` theme | ✅ done |
| **M3** | Upload destinations (Imgur, `.sxcu`, FTP/FTPS) | ✅ done |
| M4 | Screen recording (MP4/GIF) | planned |
| M6 | Workflows — chaining capture → action → destination | planned |

M5 was scoped as "OCR, scrolling capture, workflows" and shipped the first two
plus the smart eraser; workflows did not make it. They are M6 now rather than
being quietly folded into a milestone that is already marked done — the
`sharex` shell's disabled **Workflows** row names that milestone, and a
disabled row pointing at a shipped milestone is a lie the UI would be telling.

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

### M5 — OCR, scrolling capture, smart eraser

**Extract text** reads a capture with `Windows.Media.Ocr`, the recogniser that
ships with the OS. Nothing leaves the machine: no service, no API key, no
network. It is on the Lightbox, on each history card and in the editor's toolbar,
and it shows the full text plus a per-line list, each line individually
copyable.

> If it reports that no OCR language pack is installed, add one under
> **Settings › Time & language › Language & region** — pick a language, open its
> ⋯ menu → *Language options*, and install the **Optical character recognition**
> optional feature.

**Redact** now has three modes rather than two: Blur (`B`), Pixelate (`P`) and
**Smart eraser** (`X`). The eraser replaces the selection with the smoothest
surface that meets the pixels immediately around it — a Laplace solve, not a
blend of the four nearest edges — so it reproduces a gradient exactly instead of
banding. It is *not* content-aware fill: over a photo it leaves a smooth smudge
rather than invented texture.

#### Scrolling capture — and where it does not work

Pick a window, and santi.sharex captures it, sends it a wheel step, captures it
again, and stitches the frames into one tall image. The join is **measured, not
assumed** — a fixed offset would guarantee seams, because three wheel notches is
not a number of pixels and smooth scrolling, momentum and line snapping move
every app by a different amount. If it cannot find a confident overlap it
**stops and keeps what it has** rather than concatenating blindly, and tells you
it stopped early and why.

It works well on ordinary scrolling content: documents, articles, long web
pages, chat logs, code.

It works **poorly, and will usually stop early, on**:

- **Virtualised lists** — anything that recycles rows as you scroll (large
  tables, some file managers, infinite feeds). The content under the viewport is
  regenerated rather than moved, so consecutive frames have no true overlap.
- **Parallax and animated content** — backgrounds that move at a different rate
  than the text, or anything still animating when the frame is grabbed.
- **Fixed headers, toolbars and sidebars** — they repeat identically in every
  frame. The overlap search skips a margin at each edge for exactly this reason,
  but a tall sticky header can still dominate.
- **Anything that does not scroll with the wheel** — a pane that needs a
  scrollbar drag reads as "the content never moved", and you get one frame.

Two more things worth knowing, because they are deliberate:

- **It drives the real mouse pointer.** The wheel is sent with `SendInput` with
  the cursor parked over the window, not posted as `WM_MOUSEWHEEL` to a handle —
  a posted message goes to the wrong child window in Chromium, Electron and WPF
  and is silently ignored. So **moving the mouse ends the run**; that is the
  escape hatch, and reaching for Cancel stops it on the way. The pointer is put
  back where it was afterwards.
- **Whatever was stitched is always kept.** Cancel, a frame budget, a resize, a
  window coming in front — all of them finalize the frames captured so far
  through the same pipeline as any other capture, so the result is named, saved,
  copied and in history.

Tune it under **Settings › Scrolling capture**: settle delay (raise it for apps
with smooth scrolling), wheel notches per step, and a hard frame budget.

### M3 — upload destinations

Three destinations: **Imgur**, **ShareX-compatible custom uploaders** (`.sxcu`),
and **FTP / explicit FTPS**. Set them up under **Destinations** (its own pane in
the `sharex` shell, a section of Settings otherwise).

Read the next two sections before using any of them. M3 is the first milestone
where this app sends your screen off the machine, and both are commitments
rather than notes.

#### Nothing uploads unless you asked

**Uploading is opt-in, per capture.** The **Upload** action on a capture — on a
history card, in the lightbox, or on the post-capture preview — is the only
thing that sends it anywhere. There is no "upload everything" default:
`autoUpload` is `false` on a fresh install *and* on an existing `settings.json`
that predates the field, and `destination` is `none` until you pick one, so a
default install has nowhere to send a capture even if the switch were on.

The one switch that changes this lives in **Settings › Uploading**, behind a
confirmation that states what it means: every capture, including the ones taken
by hotkey while you are working in another app, goes to the configured
destination the moment it is taken, and you are not asked again.

A screen capture tool that uploads without being asked is a data-exfiltration
tool with a friendly icon, so if any of the above is ever ambiguous in this app,
treat it as a bug and report it.

santi.sharex makes **no other outbound connection of any kind** — no telemetry,
no update check, no analytics, no crash reporting. The only network traffic it
can produce is an upload you asked for, to the destination you configured. There
is exactly one URL compiled into the binary (`https://api.imgur.com/3/image`),
and it is only reached by an Imgur upload.

#### Credentials

FTP passwords and API keys go into **Windows Credential Manager**, keyed
`santi.sharex/<destination>/<field>` — never into `settings.json`, never into a
log line, never into an error message. `settings.json` holds only non-secret
configuration: host, port, user name, remote directory, which destination is
active.

The Settings page can **write** a credential and **remove** one, and can tell you
*whether* one is set. It cannot show you one, because nothing can read one back:
the reader is scoped to the upload modules, so no command the UI can call is able
to reach it. That is enforced by the type system rather than by care.

You can see what is stored — names only — with
`rundll32 keymgr.dll,KRShowKeyMgr`.

#### Imgur

santi.sharex deliberately ships **no Imgur Client ID**. This repository is
public, and a bundled ID would be scraped, abused and rate-limited into
uselessness for everyone using it. Register your own at
<https://api.imgur.com/oauth2/addclient>, choose **“Anonymous usage without user
authorization”**, and paste the Client ID into Destinations › Imgur; the Client
secret is not used. Uploads are anonymous, and Imgur's rate limits are reported
as *when the quota comes back* rather than as "upload failed".

#### Custom uploaders — the exact `.sxcu` subset

Import a ShareX `.sxcu` and it is used as-is. **Supported, in full:**

- `RequestMethod` — `POST` or `PUT` (`RequestType` accepted as the older name)
- `RequestURL` — `http://` or `https://` only
- `Headers` and `Parameters` (`Arguments` accepted as the older name for
  `Parameters`); both are string maps
- `Body` — `MultipartFormData` or `Binary` (`RequestBodyType` as the older name)
- `FileFormName`
- `URL` / `ThumbnailURL` / `DeletionURL` response templates
- `$json:path.to.field$` (including `[0]` array indexes) and `$response$` inside
  those templates
- `{filename}` substitution in header and parameter *values*
- `DestinationType` of `ImageUploader` or `FileUploader`

**Not supported, and refused at import time with the offending field named:**
OAuth flows (`OAuth2`), `RegexList` and `$regex:…$`, any `Body` /
`RequestBodyType` outside the two above, any other `$…$` placeholder, and
destination types that are not image or file uploaders (`TextUploader`,
`URLShortener`, `URLSharingService`). A file that needs one of those is rejected
when you import it, not accepted and then mysteriously broken on a real capture.

Any header whose name looks like a credential — anything containing `auth`,
`key`, `token`, `secret`, `password`, `credential` or `cookie` — has its value
moved to Windows Credential Manager at import. Only the header *name* stays in
`settings.json`.

#### FTP / FTPS

Plain FTP and **explicit FTPS** (`AUTH TLS`), with rustls and the same trust
anchors HTTPS uses. There is no "accept any certificate" switch, on purpose.

**SFTP is not implemented.** Despite the name it is not a variant of FTP or
FTPS — it is a subsystem of SSH, and carrying an SSH stack for it is not
something this app does. The picker says so rather than offering it and failing.

**Plain FTP sends your password, and the file, in clear text.** That warning is
on screen next to the switch that turns FTPS off, which is where the choice is
actually made. Configure host, port, user name, remote directory, passive mode,
and an optional public URL prefix so the copied link points at the web host
rather than at an `ftp://` path.

#### What "Test connection" checks

It differs per destination, and the sentence it gives you says which — read it
rather than treating a green tick as "verified":

- **FTP** genuinely connects, signs in and looks at the remote directory. It
  creates nothing; a missing directory is reported, and the first upload makes it.
- **Imgur** checks a Client ID is saved and can be sent in a header. It does not
  ask Imgur — the only request Imgur offers uploads a picture, and a test must
  not leave one behind.
- **Custom uploader** re-validates the imported file and that every credential
  header has a value stored, and does not contact the server, for the same reason.

#### Cancelling, and what a cancel means

Uploads run off the UI thread and **Cancel really aborts the transfer** — the
payload reader fails mid-chunk, which ends the HTTP body or the FTP data
connection on the wire rather than quietly discarding a finished result. A
part-written FTP file is removed. The one thing a cancel cannot do is un-send a
transfer that had already completed when you pressed it; that is reported as the
upload it was, with its link, rather than as a cancellation that leaves an
orphan on the server.

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

`docs/ARCHITECTURE.md` (M1), `docs/ARCHITECTURE-M2.md` (M2), the
`docs/ARCHITECTURE-M2.x.md` series, `docs/ARCHITECTURE-M3.md` and
`docs/ARCHITECTURE-M5.md` are the binding contracts — exact command names, type
shapes, event names, and design tokens. Read them before changing anything that
crosses the Rust/TypeScript seam.

Two things in there are load-bearing and easy to "fix" back into bugs:

- **Extra windows are query params, not routes** (`?w=overlay`, `?w=editor`).
  `adapter-static` never emits `/overlay.html`, and the `index.html?…` form
  404s under `tauri dev` because SvelteKit's dev middleware has no root
  `index.html`. The bare `?w=…` form is the one that works in both.
- **All geometry is stored in image pixels.** The region overlay derives its
  scale at runtime from `freeze.width / window.innerWidth` rather than trusting
  `devicePixelRatio`, and the editor converts pointer coordinates in exactly one
  helper. This is what keeps captures pixel-accurate across DPI settings.
- **`upload::secrets::get` is `pub(in crate::upload)`, and must stay that way.**
  It is the only reader of a stored credential, and its scope is what makes "no
  command can return a secret" a compile-time property instead of a review item.
  Widening it to `pub(crate)` would silently reopen the whole of M3 §2.
- **`reqwest` and `suppaftp` are `default-features = false` with rustls.** The
  defaults are native-tls, which means OpenSSL/Schannel and a C toolchain this
  machine does not have. `cargo tree` must stay clear of `openssl-sys`,
  `native-tls`, `libssh2-sys` and anything built with CMake.

## Known rough edges

Untested at runtime, in rough order of likelihood:

1. **Uploading has never met a real server from this machine.** The Imgur
   request, the `.sxcu` request and the FTP session are all unit-tested around
   the network but not through it. The most likely first failure is an FTPS
   handshake or a passive-mode data connection, both of which report the
   server's own error rather than a generic one — start with **Test connection**
   on FTP, which is the one test that genuinely connects.
2. **Scrolling capture is the least reliable feature here** — see the list of
   what it cannot do above. It is built to stop and say so rather than produce a
   garbled image, so the common failure is a short capture with an explanation,
   not a wrong one. Content that repeats vertically with a regular period is the
   case it reads worst: a scroll of exactly one period is indistinguishable from
   not scrolling, so it may report reaching the bottom early.
3. **A hotkey may still not fire** — if `RegisterHotKey` refuses it *and* the
   low-level hook is switched off (or cannot install), the combo stays unbound.
   Registration failures surface as a toast; Settings › Hotkeys shows which
   mechanism owns each one.
4. **Text nudges on commit** — the inline `<textarea>` and canvas
   `textBaseline: 'top'` disagree by a font-dependent fraction of an em.
5. **Large-capture performance** — the editor deep-clones shapes per repaint, and
   Copy/Save pushes a base64 PNG (15–25 MB on a 4K capture) through the WebView2
   IPC in one string.
6. **Mixed-DPI multi-monitor** — crop math is DPI-safe by construction, but
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
