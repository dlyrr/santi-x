# santi.sharex — M3: upload destinations

Extends everything through M5. Three destinations — Imgur, ShareX-compatible
custom uploaders, and FTP/FTPS — plus the plumbing they share.

This is the first milestone where santi.sharex sends the user's screen off the
machine. That changes the stakes: the failure modes here are privacy failures,
not rendering glitches. Sections 1 and 2 are not preamble.

---

## 1. Nothing uploads unless the user asked

**Uploading is opt-in, per capture, always.** There is no "upload everything"
default and there never will be one by accident.

- `Settings.auto_upload` defaults to **false**, with a named serde default so an
  existing `settings.json` cannot silently acquire it as `true`.
- With it off — the default — the only way anything leaves the machine is the
  explicit **Upload** action on a capture.
- Turning it on gets a real confirmation that says plainly what it means: every
  capture, including ones taken by hotkey while you are doing something else,
  will be sent to the configured destination. Not a toggle with a shrug.
- No destination is configured out of the box, so even auto-upload has nothing
  to send until the user sets one up.

A screen capture tool that uploads without being asked is a data-exfiltration
tool with a friendly icon. Treat any ambiguity here as a bug.

## 2. Credentials

FTP passwords and API keys **never touch `settings.json`**, never appear in a
log line, and never cross the IPC back to the frontend.

- Secrets go in **Windows Credential Manager** (`CredWriteW`/`CredReadW` from the
  `windows` crate, already a dependency), keyed `santi.sharex/<destination-id>`.
  That ties them to the Windows account and keeps them out of the repo, out of
  backups of the config directory, and out of anything a user might paste into
  an issue.
- `settings.json` stores only non-secret configuration: host, port, path, user
  name, which destination is active.
- The frontend may **write** a secret and **clear** it, and may ask *whether one
  is set*. It may never read one back. A settings page that can display the
  password is a settings page that leaks it to anyone shoulder-surfing.
- Error messages must never interpolate a secret. An FTP failure that echoes the
  password into a toast is the exact bug this section exists to prevent.

## 3. No embedded Imgur client ID

ShareX ships its own Imgur client ID. santi.sharex will not, because this repo is
public: an embedded ID would be extracted, abused, and rate-limited into
uselessness for everyone within a week.

The user supplies their own. Settings › Destinations › Imgur must therefore
carry a short, correct setup path — register an application at
`https://api.imgur.com/oauth2/addclient`, choose "Anonymous usage without user
authorization", paste the Client ID — rather than a bare empty field that looks
broken. Store the ID as a secret (§2); it is not a password, but there is no
reason to leave it lying in a config file either.

Uploads are anonymous: `Authorization: Client-ID <id>`, `POST https://api.imgur.com/3/image`,
multipart with the image. The response's `data.link` is the URL. Handle Imgur's
rate-limit responses (429, and the `X-RateLimit-*` headers) with a message that
says when to try again, not "upload failed".

---

## 4. Custom uploaders (`.sxcu`)

ShareX's own custom-uploader files, so an existing configuration works here.

**Support this subset, and document exactly this list in the README:**

- `RequestMethod` (`POST`, `PUT`), `RequestURL`
- `Headers`, `Parameters` (both string maps; `Arguments` accepted as an alias,
  since older files use it)
- `Body`: `MultipartFormData` and `Binary`
- `FileFormName`
- `URL` / `ThumbnailURL` / `DeletionURL` response templates
- Response parsing for `$json:path.to.field$` and `$response$`
- `{filename}` substitution in parameters and headers

**Explicitly not supported**, and it must say so when it meets one rather than
failing obscurely: OAuth flows, `RequestBodyType` values outside the two above,
`RegexList`/`$regex:…$`, and destination types other than image uploaders.
Importing a file that needs one of those is rejected at import time with a
message naming the field — not accepted and then mysteriously broken at upload.

An imported `.sxcu` is stored as configuration; any field that looks like a
secret (anything under `Headers` named like an authorization or key header) goes
to the credential store per §2.

## 5. FTP / FTPS

- `suppaftp` with **rustls**, not native-tls. This machine has no C toolchain —
  an OpenSSL or libssh2 dependency will not build here, which is a real
  constraint discovered the hard way on an earlier project, not a preference.
- Plain FTP and explicit FTPS (`AUTH TLS`). **SFTP is not implemented** — it
  needs an SSH stack, and half-implementing it would be worse than saying so.
  The destination picker says SFTP is not supported rather than offering it and
  failing.
- Config: host, port, username, remote directory, passive mode, and an optional
  URL prefix so the copied link points at the public host rather than the FTP
  path. Password is a secret (§2).
- Plain FTP sends the password in clear text. The UI must say that where the
  user chooses it, not bury it in docs.

---

## 6. Shared plumbing

### Rust

New `src-tauri/src/upload/` — `mod.rs` (the trait, dispatch, progress), plus
`imgur.rs`, `custom.rs`, `ftp.rs`, and `secrets.rs` for §2.

```rust
pub struct UploadResult { pub url: String, pub deletion_url: Option<String>, pub thumbnail_url: Option<String> }
pub trait Destination { fn upload(&self, ctx: &UploadCtx, bytes: &[u8], name: &str) -> Result<UploadResult, String>; }
```

`reqwest` with `rustls-tls` and **default-features off** — same no-C-toolchain
constraint.

Commands: `upload_capture(id)`, `cancel_upload(id)`, `test_destination(kind)`,
`set_destination_secret(kind, field, value)`, `clear_destination_secret(kind, field)`,
`destination_status()` (which secrets are set — booleans, never values).

Events: `upload://progress` `{ id, sent, total }`, `upload://done`
`{ id, url }`, `upload://error` `{ id, message }`.

- Uploads run off the UI thread and are **cancellable**. A 20 MB capture on slow
  upstream must not wedge the app.
- On success the URL goes to the clipboard when `Settings.copy_url_after_upload`
  is true (**default true** — it is the point of uploading), and the
  `CaptureRecord` gains `url` so History can show and re-copy it.
- `CaptureRecord` gains `url: Option<String>` and `deletion_url: Option<String>`,
  both `#[serde(default)]` so existing `history.json` still loads. The user has
  82 real records; losing them to a schema change is not acceptable.

### Frontend

- **Destinations** pane in the ShareX shell (currently a disabled `M3` row — it
  becomes real) and a Destinations section in Settings.
- Per-destination config forms; password/key fields are write-only with a
  "Configured" indicator, per §2.
- **Test connection** per destination, reporting the real error.
- An **Upload** action on `CaptureCard` and in `Lightbox`, with progress and
  cancel. Uploaded records show a **Copy link** action.
- The capture preview (M2.9 §3) gains an upload affordance when a destination is
  configured — that is where the user is already looking after a capture.

---

## 7. What must not regress

Everything through M5 and the rename: overlay Escape ladder, arm/ready
handshake, commit-on-release, the native-crop fast path, cursor capture, the
preview hiding before a grab, the keyboard hook's scope, the four themes and the
ShareX shell's no-faked-features rule — the Destinations row must become genuinely
live, not merely enabled.

`cargo check`, `cargo test`, `pnpm check`, `pnpm check:tokens`, `pnpm build`.
