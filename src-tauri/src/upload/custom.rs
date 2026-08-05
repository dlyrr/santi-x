//! ShareX-compatible custom uploaders (M3 §4), plus the small HTTP helper
//! [`imgur`](super::imgur) shares with them.
//!
//! # A `.sxcu` is either understood or refused, never half-understood
//!
//! ShareX's custom-uploader format is much larger than the subset M3 §4 commits
//! to. The tempting way to handle that is to parse what we know and ignore the
//! rest — and it is the wrong way, because the result is a destination that
//! imports cleanly, sits in Settings looking configured, and then fails at
//! upload time with a message about a status code. The user has no way to get
//! from there back to "this file needed OAuth".
//!
//! So [`import`] is a *validator* first. Everything outside the supported subset
//! is rejected at import time with a message naming the field or the placeholder
//! that caused it. The supported subset, in full:
//!
//! * `RequestMethod` — `POST` or `PUT` (`RequestType` accepted as the older name)
//! * `RequestURL` — `http`/`https` only
//! * `Headers` and `Parameters` (`Arguments` accepted as the older name)
//! * `Body` — `MultipartFormData` or `Binary` (`RequestBodyType` as the older name)
//! * `FileFormName`
//! * `URL` / `ThumbnailURL` / `DeletionURL` response templates
//! * `$json:path.to.field$` and `$response$` inside those templates
//! * `{filename}` inside header and parameter *values*
//!
//! Anything else — an `OAuth2` block, a `RegexList`, `$regex:…$`, a body type an
//! image cannot go in, a destination type that is not an image uploader — is an
//! error with that name in it.
//!
//! The same validation runs again in [`CustomUploader::from_settings`], because
//! `settings.json` is a text file a user can edit: a stored config is checked on
//! the way *out* as well as on the way in.
//!
//! # Secrets
//!
//! A `.sxcu` routinely carries an API key in `Headers`. Storing that in
//! `settings.json` would put it in every config backup and every pasted bug
//! report, so [`import`] splits those headers out (see
//! [`looks_like_a_credential`]) and hands them back as [`HeaderSecret`]s for
//! [`install`] to write to Windows Credential Manager (M3 §2). The values are
//! deliberately awkward to misuse: [`HeaderSecret`] is not `Serialize`, so it
//! cannot cross the IPC boundary, and its `Debug` prints a placeholder.
//!
//! Every error string that leaves an upload goes through [`redact`] with the
//! live secret values first. Error text ends up in toasts, on stderr and
//! eventually in GitHub issues; a header value must not be able to ride along.

use std::collections::BTreeMap;
use std::io::Read;

use reqwest::blocking::multipart::{Form, Part};
use reqwest::blocking::{Body, Client, Request};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, CONTENT_TYPE};
use reqwest::Method as HttpMethod;
use serde_json::{Map, Value};

use crate::store::{CustomUploaderSettings, Settings};

use super::secrets;
use super::{http_client, Destination, UploadCtx, UploadResult};

/// The credential-store namespace this destination's header secrets live under.
///
/// Must equal `DestinationKind::Custom.id()`, which is what `UploadCtx::secret`
/// reads them back with — [`tests::the_import_writes_where_uploads_read`] is
/// what stops the two drifting. It is spelled out here rather than derived
/// because [`install`] runs outside an upload, with no `UploadCtx` to ask.
pub const KIND: &str = "custom";

// ---------------------------------------------------------------------------
// shared HTTP helpers
// ---------------------------------------------------------------------------
//
// These live here rather than in `mod.rs` because a custom uploader *is* the
// general case — an arbitrary URL, method and body — and Imgur is one fixed
// instance of it.

/// Ceiling on how much of a response body is read. A destination is a URL the
/// user typed, so it is not necessarily friendly; without this a server that
/// streams forever would be an unbounded allocation. Generous next to any real
/// upload response, which is a few hundred bytes of JSON.
const MAX_RESPONSE_BYTES: u64 = 1 << 20;

/// How much of a failing response body is quoted back at the user.
const ERROR_SNIPPET: usize = 400;

/// A secret shorter than this is not redacted from error text: replacing every
/// occurrence of a two-character "secret" would mangle the message without
/// protecting anything. Real API keys and passwords are far longer.
const MIN_REDACTABLE: usize = 6;

/// What came back, with the body already read and bounded.
pub(in crate::upload) struct HttpReply {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: String,
}

/// Executes one request and reads its reply.
pub(in crate::upload) fn send(client: &Client, request: Request) -> Result<HttpReply, String> {
    let response = client.execute(request).map_err(describe_transport_error)?;
    let status = response.status().as_u16();
    let headers = response.headers().clone();

    let mut buf = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES)
        .read_to_end(&mut buf)
        .map_err(|e| format!("the server stopped replying part-way through: {e}"))?;

    Ok(HttpReply {
        status,
        headers,
        body: String::from_utf8_lossy(&buf).into_owned(),
    })
}

/// reqwest's own `Display` is a chain of `source`s that reads as noise; the two
/// cases a user can act on are pulled out by hand. A cancelled upload surfaces
/// here too — as a torn request body — and `mod.rs` rewrites it from the cancel
/// flag rather than from this text.
fn describe_transport_error(e: reqwest::Error) -> String {
    if e.is_connect() {
        return format!("Could not reach the upload server: {e}");
    }
    format!("The upload failed: {e}")
}

/// Removes every live secret from a message before anyone can read it.
///
/// The belt to §2's braces. Nothing here deliberately interpolates a key or a
/// password, but error text is assembled from server replies and third-party
/// `Display` impls, and a server that echoed back the `Authorization` header it
/// did not like would otherwise put it straight into a toast — and from there
/// into an issue. Takes borrowed values so a caller never has to make an owned
/// copy of a [`secrets::Secret`] just to protect it.
pub(in crate::upload) fn redact(message: String, secrets: &[&str]) -> String {
    let mut out = message;
    for secret in secrets {
        let secret = secret.trim();
        if secret.len() >= MIN_REDACTABLE && out.contains(secret) {
            out = out.replace(secret, "<redacted>");
        }
    }
    out
}

/// A bounded, single-line quote of a failing reply. Servers answer failures with
/// anything from four words of JSON to a full HTML error page.
pub(in crate::upload) fn snippet(body: &str) -> String {
    let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return String::new();
    }
    if flat.chars().count() <= ERROR_SNIPPET {
        return format!(": {flat}");
    }
    let cut: String = flat.chars().take(ERROR_SNIPPET).collect();
    format!(": {cut}…")
}

/// A body sent as `Binary` needs a content type, and the only thing we have to
/// derive one from is the file name the capture pipeline produced. santi.sharex
/// only ever hands this module a PNG today, so the table exists to stay correct
/// rather than to be exercised.
fn content_type_for(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, ext)| ext.to_ascii_lowercase()) {
        Some(ext) => match ext.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "bmp" => "image/bmp",
            _ => "application/octet-stream",
        },
        None => "application/octet-stream",
    }
}

// ---------------------------------------------------------------------------
// the shapes a `.sxcu` is allowed to describe
// ---------------------------------------------------------------------------

/// The two request methods M3 §4 supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Post,
    Put,
}

impl Method {
    /// As it is written in a `.sxcu` and stored in `settings.json`.
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Post => "POST",
            Method::Put => "PUT",
        }
    }

    fn http(self) -> HttpMethod {
        match self {
            Method::Post => HttpMethod::POST,
            Method::Put => HttpMethod::PUT,
        }
    }
}

/// The two body shapes an image fits in. Spelled exactly as ShareX spells them,
/// so a stored config reads like the file it was imported from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKind {
    MultipartFormData,
    Binary,
}

impl BodyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BodyKind::MultipartFormData => "MultipartFormData",
            BodyKind::Binary => "Binary",
        }
    }
}

// ---------------------------------------------------------------------------
// import
// ---------------------------------------------------------------------------

/// `DestinationType` values whose request shape is "here is a file, take it".
///
/// `ImageUploader` is what M3 §4 names. `FileUploader` is accepted alongside it
/// because the request it describes is byte-for-byte the same one — a PNG is a
/// file — and refusing a generic file host would reject configurations that work
/// perfectly. A `TextUploader`, `URLShortener` or `URLSharingService` has nowhere
/// to put an image and is refused.
const IMAGE_DESTINATIONS: [&str; 2] = ["ImageUploader", "FileUploader"];

/// Fields whose mere presence means the file needs something not implemented.
const REFUSED_FIELDS: [(&str, &str); 2] = [
    (
        "OAuth2",
        "an OAuth sign-in flow, which santi.sharex does not implement",
    ),
    (
        "RegexList",
        "regular-expression response parsing ($regex:…$), which santi.sharex does not implement",
    ),
];

/// Header names treated as credentials and moved to the credential store
/// (M3 §2, §4). Matched as substrings of the lowercased name, so
/// `Authorization`, `X-Api-Key`, `api_key`, `X-Auth-Token` and `Cookie` are all
/// caught.
///
/// Deliberately over-broad. A false positive costs nothing — the header is still
/// sent, it is just stored somewhere safer — while a false negative writes
/// somebody's API key into `settings.json`.
const CREDENTIAL_MARKERS: [&str; 7] = [
    "auth", "key", "token", "secret", "password", "credential", "cookie",
];

pub fn looks_like_a_credential(header: &str) -> bool {
    let lower = header.to_ascii_lowercase();
    CREDENTIAL_MARKERS.iter().any(|m| lower.contains(m))
}

/// One header value on its way to the credential store.
///
/// Deliberately *not* `Serialize`: this type cannot be returned from a
/// `#[tauri::command]`, which makes M3 §2's "may never read one back" a
/// compile-time property rather than a review item.
pub struct HeaderSecret {
    /// The credential field, which for a custom uploader is the header name —
    /// the same key `mod.rs`'s `secret_fields` reports and `ctx.secret` reads.
    field: String,
    value: String,
}

impl HeaderSecret {
    pub fn field(&self) -> &str {
        &self.field
    }

    /// Consumes the secret to hand the value to the credential store. The only
    /// way to reach it, and it leaves nothing behind.
    pub(in crate::upload) fn into_value(self) -> String {
        self.value
    }
}

/// Redacted, because `Debug` output is one `dbg!` away from stdout.
impl std::fmt::Debug for HeaderSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeaderSecret")
            .field("field", &self.field)
            .field("value", &"<redacted>")
            .finish()
    }
}

/// What [`import`] produces: config to persist, and secrets to store separately.
#[derive(Debug)]
pub struct Imported {
    pub settings: CustomUploaderSettings,
    pub secrets: Vec<HeaderSecret>,
}

/// Parses and validates one `.sxcu` file.
///
/// Every `Err` names the field or placeholder that caused it. See the module
/// docs for why that matters more than accepting the file. Pure: nothing is
/// written anywhere — see [`install`].
pub fn import(raw: &str) -> Result<Imported, String> {
    let doc: Value = serde_json::from_str(raw)
        .map_err(|e| format!("That is not a valid .sxcu file — it is not JSON ({e})."))?;
    let Value::Object(doc) = doc else {
        return Err(
            "That .sxcu file is not a JSON object, so there is no uploader in it.".to_string(),
        );
    };

    for (field, what) in REFUSED_FIELDS {
        if present(&doc, field).is_some() {
            return Err(format!(
                "This uploader uses {field}, i.e. {what}. It was not imported."
            ));
        }
    }

    check_destination_type(&doc)?;

    let method = read_method(&doc)?;
    let body = read_body(&doc)?;

    let request_url = text(&doc, "RequestURL")?
        .unwrap_or_default()
        .trim()
        .to_string();
    if request_url.is_empty() {
        return Err(
            "This uploader has no RequestURL, so there is nowhere to send the image.".to_string(),
        );
    }
    if !(request_url.starts_with("https://") || request_url.starts_with("http://")) {
        return Err(format!(
            "RequestURL \"{request_url}\" is not an http:// or https:// address, so santi.sharex will not upload to it."
        ));
    }

    let file_form_name = text(&doc, "FileFormName")?
        .unwrap_or_default()
        .trim()
        .to_string();
    if body == BodyKind::MultipartFormData && file_form_name.is_empty() {
        return Err(
            "This uploader sends MultipartFormData but has no FileFormName, so the image has no field to go in."
                .to_string(),
        );
    }

    // `Arguments` is what ShareX called `Parameters` before v12. A file with
    // both is merged rather than refused, newer name last.
    let mut parameters = string_map(&doc, "Arguments")?;
    parameters.extend(string_map(&doc, "Parameters")?);

    let mut headers = string_map(&doc, "Headers")?;
    let mut secrets: Vec<HeaderSecret> = Vec::new();
    let mut secret_headers: Vec<String> = Vec::new();
    headers.retain(|name, value| {
        if !looks_like_a_credential(name) {
            return true;
        }
        secrets.push(HeaderSecret {
            field: name.clone(),
            value: std::mem::take(value),
        });
        secret_headers.push(name.clone());
        false
    });

    // A file with no `URL` template is ShareX's "the body *is* the link", which
    // is exactly what `$response$` means. Making that explicit here leaves the
    // upload path with one rule instead of two.
    let url = match text(&doc, "URL")?.unwrap_or_default() {
        t if t.trim().is_empty() => "$response$".to_string(),
        t => t,
    };
    let thumbnail_url = text(&doc, "ThumbnailURL")?.unwrap_or_default();
    let deletion_url = text(&doc, "DeletionURL")?.unwrap_or_default();

    // Parsed, not just stored: an unsupported placeholder is a refusal, and it
    // has to happen here rather than after the image is already on the server.
    parse_template("URL", &url)?;
    parse_template("ThumbnailURL", &thumbnail_url)?;
    parse_template("DeletionURL", &deletion_url)?;

    Ok(Imported {
        settings: CustomUploaderSettings {
            name: text(&doc, "Name")?.unwrap_or_default(),
            request_method: method.as_str().to_string(),
            request_url,
            headers,
            parameters,
            body: body.as_str().to_string(),
            file_form_name,
            url,
            thumbnail_url,
            deletion_url,
            secret_headers,
        },
        secrets,
    })
}

/// [`import`], then the credential writes — the whole of "import this file"
/// except persisting the returned settings, which belongs to whoever owns
/// `settings.json`.
///
/// The plaintext never leaves this function: [`HeaderSecret::into_value`]
/// consumes each secret straight into [`secrets::set`], and what comes back is
/// the non-secret half, ready to be stored as `Settings::custom_uploader`.
///
/// A failed credential write aborts the import rather than leaving a config
/// behind that references a secret that is not there — that config would present
/// as configured in Settings and then fail on the first upload.
///
/// `previous` is the uploader being replaced. Its credentials are removed unless
/// the new file uses the same header name: without that, importing a second
/// `.sxcu` would leave the first one's API key in Credential Manager forever —
/// and if the same header name ever came back, Settings would show it as already
/// configured with a key belonging to a service the user has stopped using.
pub fn install(
    raw: &str,
    previous: &CustomUploaderSettings,
) -> Result<CustomUploaderSettings, String> {
    let imported = import(raw)?;

    for secret in imported.secrets {
        let field = secret.field().to_string();
        secrets::set(KIND, &field, &secret.into_value())
            .map_err(|e| format!("Could not save the \"{field}\" header's value: {e}"))?;
    }

    for stale in &previous.secret_headers {
        let still_used = imported
            .settings
            .secret_headers
            .iter()
            .any(|header| header.eq_ignore_ascii_case(stale));
        if !still_used {
            // Best effort: a credential that will not delete is worth a line on
            // stderr, not a failed import of a file that is otherwise fine.
            if let Err(e) = secrets::clear(KIND, stale) {
                eprintln!("santi.sharex: could not remove the old \"{stale}\" credential: {e}");
            }
        }
    }

    Ok(imported.settings)
}

/// A key that is present and not `null`. ShareX writes `null` for fields it is
/// not using, so `"OAuth2": null` must not read as "uses OAuth".
fn present<'a>(doc: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    doc.get(key).filter(|v| !v.is_null())
}

/// One string field, refusing a non-string rather than coercing it.
fn text(doc: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match present(doc, key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(format!(
            "{key} should be a piece of text in a .sxcu file, but this one has {}.",
            describe_json(other)
        )),
    }
}

/// One `{ "name": "value" }` map, refusing non-string values by name.
fn string_map(doc: &Map<String, Value>, key: &str) -> Result<BTreeMap<String, String>, String> {
    let Some(value) = present(doc, key) else {
        return Ok(BTreeMap::new());
    };
    let Value::Object(map) = value else {
        return Err(format!(
            "{key} should be a set of name/value pairs, but this one is {}.",
            describe_json(value)
        ));
    };

    let mut out = BTreeMap::new();
    for (name, value) in map {
        match value {
            Value::String(s) => {
                out.insert(name.clone(), s.clone());
            }
            // Numbers and booleans are unambiguous and common in hand-written
            // files; anything structured is not a header or a form field.
            Value::Number(n) => {
                out.insert(name.clone(), n.to_string());
            }
            Value::Bool(b) => {
                out.insert(name.clone(), b.to_string());
            }
            other => {
                return Err(format!(
                    "{key}'s \"{name}\" is {}, which cannot be sent as a header or a form field.",
                    describe_json(other)
                ))
            }
        }
    }
    Ok(out)
}

fn describe_json(value: &Value) -> &'static str {
    match value {
        Value::Null => "nothing",
        Value::Bool(_) => "a true/false value",
        Value::Number(_) => "a number",
        Value::String(_) => "a piece of text",
        Value::Array(_) => "a list",
        Value::Object(_) => "a nested object",
    }
}

fn check_destination_type(doc: &Map<String, Value>) -> Result<(), String> {
    // Older files omit it entirely; ShareX assumed an image uploader then, and
    // so does this.
    let Some(raw) = text(doc, "DestinationType")? else {
        return Ok(());
    };
    if raw.trim().is_empty() {
        return Ok(());
    }

    let supported = raw
        .split(',')
        .map(str::trim)
        .any(|t| IMAGE_DESTINATIONS.iter().any(|s| t.eq_ignore_ascii_case(s)));
    if supported {
        return Ok(());
    }

    Err(format!(
        "This uploader's DestinationType is \"{}\", and santi.sharex only uploads images — it has nothing to send a text or URL destination.",
        raw.trim()
    ))
}

/// `RequestMethod`, or `RequestType` under its pre-v12 name.
///
/// The refusal names whichever of the two keys the value actually came from:
/// telling someone to fix `RequestMethod` in a file that says `RequestType`
/// sends them looking for a line that is not there.
fn read_method(doc: &Map<String, Value>) -> Result<Method, String> {
    let (field, raw) = match text(doc, "RequestMethod")? {
        Some(v) => ("RequestMethod", v),
        None => match text(doc, "RequestType")? {
            Some(v) => ("RequestType", v),
            None => return Ok(Method::Post),
        },
    };
    parse_method(field, &raw)
}

/// Also the way a stored `settings.json` value is checked on the way back out.
fn parse_method(field: &str, raw: &str) -> Result<Method, String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("POST") {
        return Ok(Method::Post);
    }
    if raw.eq_ignore_ascii_case("PUT") {
        return Ok(Method::Put);
    }
    Err(format!(
        "{field} is \"{raw}\", and santi.sharex uploads with POST or PUT only."
    ))
}

/// `Body`, or `RequestBodyType` under its older name. Same naming rule.
fn read_body(doc: &Map<String, Value>) -> Result<BodyKind, String> {
    let (field, raw) = match text(doc, "Body")? {
        Some(v) => ("Body", v),
        None => match text(doc, "RequestBodyType")? {
            Some(v) => ("RequestBodyType", v),
            None => return Ok(BodyKind::MultipartFormData),
        },
    };
    parse_body(field, &raw)
}

fn parse_body(field: &str, raw: &str) -> Result<BodyKind, String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.eq_ignore_ascii_case("MultipartFormData") {
        return Ok(BodyKind::MultipartFormData);
    }
    if raw.eq_ignore_ascii_case("Binary") {
        return Ok(BodyKind::Binary);
    }
    Err(format!(
        "{field} is \"{raw}\", and santi.sharex can only send MultipartFormData or Binary — a {raw} body has nowhere to put an image."
    ))
}

// ---------------------------------------------------------------------------
// response templates
// ---------------------------------------------------------------------------

/// One piece of a `URL` / `ThumbnailURL` / `DeletionURL` template.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Chunk {
    Literal(String),
    /// `$response$` — the whole reply body.
    Response,
    /// `$json:path.to.field$`, with the path already split into steps.
    Json { path: String, steps: Vec<Step> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    Key(String),
    Index(usize),
}

/// Splits a template into literals and placeholders, refusing any placeholder
/// outside the supported two.
///
/// A `$` with no closing `$` is an error rather than a literal. Response
/// templates are URLs and JSON paths, where a stray dollar is far more likely to
/// be a typo in a placeholder than a real character — and this runs at import
/// time, where saying so costs nothing.
fn parse_template(field: &str, template: &str) -> Result<Vec<Chunk>, String> {
    let mut chunks = Vec::new();
    let mut rest = template;

    while let Some(open) = rest.find('$') {
        if open > 0 {
            chunks.push(Chunk::Literal(rest[..open].to_string()));
        }
        let after = &rest[open + 1..];
        let Some(close) = after.find('$') else {
            return Err(format!(
                "{field} has a \"$\" with no closing \"$\": {template}"
            ));
        };
        chunks.push(parse_token(field, &after[..close], template)?);
        rest = &after[close + 1..];
    }

    if !rest.is_empty() {
        chunks.push(Chunk::Literal(rest.to_string()));
    }
    Ok(chunks)
}

fn parse_token(field: &str, token: &str, template: &str) -> Result<Chunk, String> {
    let trimmed = token.trim();
    if trimmed.eq_ignore_ascii_case("response") {
        return Ok(Chunk::Response);
    }
    if trimmed.to_ascii_lowercase().starts_with("json:") {
        let path = trimmed["json:".len()..].trim();
        if path.is_empty() {
            return Err(format!(
                "{field} has a $json:$ placeholder with no path: {template}"
            ));
        }
        return Ok(Chunk::Json {
            path: path.to_string(),
            steps: parse_path(path),
        });
    }
    if trimmed.is_empty() {
        return Err(format!(
            "{field} has an empty \"$$\" placeholder: {template}"
        ));
    }
    Err(format!(
        "{field} uses the ${trimmed}$ placeholder, which santi.sharex does not support — it reads $json:path.to.field$ and $response$ only."
    ))
}

/// `data.files[0].url` → `[Key("data"), Key("files"), Index(0), Key("url")]`.
fn parse_path(path: &str) -> Vec<Step> {
    let mut steps = Vec::new();
    for segment in path.split('.') {
        let (name, mut rest) = match segment.find('[') {
            Some(at) => (&segment[..at], &segment[at..]),
            None => (segment, ""),
        };
        if !name.is_empty() {
            steps.push(Step::Key(name.to_string()));
        }
        while let Some(close) = rest.find(']') {
            let index = rest[1..close].trim();
            match index.parse::<usize>() {
                Ok(i) => steps.push(Step::Index(i)),
                // Not a number, so it was never an index — kept as written so
                // the "no such field" error quotes the path the user wrote.
                Err(_) => steps.push(Step::Key(index.to_string())),
            }
            rest = &rest[close + 1..];
        }
    }
    steps
}

fn walk<'a>(value: &'a Value, steps: &[Step]) -> Option<&'a Value> {
    let mut current = value;
    for step in steps {
        current = match step {
            Step::Key(key) => current.get(key)?,
            Step::Index(i) => current.get(i)?,
        };
    }
    Some(current)
}

/// Fills in one template against a reply.
///
/// `json` is parsed once per upload and shared by all three templates; a
/// template with no `$json:` placeholder never needs it, which is why a reply
/// that is not JSON is only an error for the templates that actually read it.
fn render(
    field: &str,
    chunks: &[Chunk],
    body: &str,
    json: Option<&Value>,
) -> Result<String, String> {
    let mut out = String::new();
    for chunk in chunks {
        match chunk {
            Chunk::Literal(text) => out.push_str(text),
            Chunk::Response => out.push_str(body.trim()),
            Chunk::Json { path, steps } => {
                let Some(json) = json else {
                    return Err(format!(
                        "The server's reply was not JSON, so {field}'s $json:{path}$ had nothing to read."
                    ));
                };
                let Some(value) = walk(json, steps) else {
                    return Err(format!(
                        "The upload was accepted, but the reply has no \"{path}\" for {field} to read."
                    ));
                };
                match value {
                    Value::String(s) => out.push_str(s),
                    Value::Number(n) => out.push_str(&n.to_string()),
                    Value::Bool(b) => out.push_str(&b.to_string()),
                    other => {
                        return Err(format!(
                            "The reply's \"{path}\" is {}, which is not something {field} can be built from.",
                            describe_json(other)
                        ))
                    }
                }
            }
        }
    }
    Ok(out)
}

/// `{filename}` in a header or parameter *value* (M3 §4).
fn substitute(value: &str, name: &str) -> String {
    value.replace("{filename}", name)
}

// ---------------------------------------------------------------------------
// the destination
// ---------------------------------------------------------------------------

/// A configured custom uploader, ready to send one request.
///
/// Built from [`CustomUploaderSettings`] with everything re-validated and the
/// three response templates already parsed, so a broken template is reported
/// before the image is on somebody's server rather than after.
///
/// `Debug` is safe to derive and a test pins why: `secret_headers` holds header
/// *names*, and the values are fetched per request and dropped with it.
#[derive(Debug)]
pub struct CustomUploader {
    method: Method,
    url: String,
    body: BodyKind,
    file_form_name: String,
    headers: BTreeMap<String, String>,
    secret_headers: Vec<String>,
    parameters: BTreeMap<String, String>,
    url_template: Vec<Chunk>,
    thumbnail_template: Vec<Chunk>,
    deletion_template: Vec<Chunk>,
}

impl CustomUploader {
    /// `settings.json` is a text file a user can edit, so everything [`import`]
    /// checked is checked again here.
    pub fn from_settings(settings: &Settings) -> Result<Self, String> {
        let config = &settings.custom_uploader;

        let url = config.request_url.trim().to_string();
        if url.is_empty() {
            return Err(
                "No custom uploader has been imported yet — add a .sxcu file in Settings › Destinations."
                    .to_string(),
            );
        }
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err(format!(
                "This uploader's address (\"{url}\") is not an http:// or https:// address."
            ));
        }

        let method = parse_method("RequestMethod", &config.request_method)?;
        let body = parse_body("Body", &config.body)?;
        if body == BodyKind::MultipartFormData && config.file_form_name.trim().is_empty() {
            return Err(
                "This uploader sends MultipartFormData but has no file field name, so the image has nowhere to go."
                    .to_string(),
            );
        }

        Ok(Self {
            method,
            url,
            body,
            file_form_name: config.file_form_name.trim().to_string(),
            headers: config.headers.clone(),
            secret_headers: config.secret_headers.clone(),
            parameters: config.parameters.clone(),
            url_template: parse_template("URL", &config.url)?,
            thumbnail_template: parse_template("ThumbnailURL", &config.thumbnail_url)?,
            deletion_template: parse_template("DeletionURL", &config.deletion_url)?,
        })
    }

    /// Every header this request carries, `{filename}` expanded and secrets
    /// fetched. The secret values come back alongside so the caller can redact
    /// them out of whatever goes wrong next.
    fn headers_for(
        &self,
        ctx: &UploadCtx,
        name: &str,
    ) -> Result<(Vec<(String, String)>, Vec<secrets::Secret>), String> {
        let mut headers: Vec<(String, String)> = self
            .headers
            .iter()
            .map(|(header, value)| (header.clone(), substitute(value, name)))
            .collect();

        let mut held = Vec::with_capacity(self.secret_headers.len());
        for header in &self.secret_headers {
            let secret = ctx.secret(header)?;
            headers.push((header.clone(), secret.expose().to_string()));
            held.push(secret);
        }
        Ok((headers, held))
    }

    fn build(
        &self,
        client: &Client,
        ctx: &UploadCtx,
        total: u64,
        name: &str,
        headers: Vec<(String, String)>,
    ) -> Result<Request, String> {
        let mut request = client.request(self.method.http(), self.url.as_str());

        for (header, value) in headers {
            // Built by hand so a rejected header names itself and *not* its
            // value — some of these values are credentials.
            let header_name = HeaderName::from_bytes(header.as_bytes())
                .map_err(|_| format!("\"{header}\" is not a usable HTTP header name."))?;
            let header_value = HeaderValue::from_str(&value).map_err(|_| {
                format!(
                    "The value configured for the \"{header}\" header cannot be sent in an HTTP header."
                )
            })?;
            request = request.header(header_name, header_value);
        }

        let parameters: Vec<(String, String)> = self
            .parameters
            .iter()
            .map(|(k, v)| (k.clone(), substitute(v, name)))
            .collect();

        request = match self.body {
            // ShareX puts the extra fields in the form alongside the image…
            BodyKind::MultipartFormData => {
                let mut form = Form::new();
                for (k, v) in parameters {
                    form = form.text(k, v);
                }
                let part = Part::reader_with_length(ctx.reader(), total)
                    .file_name(name.to_string())
                    .mime_str(content_type_for(name))
                    .map_err(|e| format!("could not describe the image to the server: {e}"))?;
                request.multipart(form.part(self.file_form_name.clone(), part))
            }
            // …and in the query string when the body is the raw image.
            BodyKind::Binary => request
                .query(&parameters)
                .header(CONTENT_TYPE, content_type_for(name))
                .body(Body::sized(ctx.reader(), total)),
        };

        request
            .build()
            .map_err(|e| format!("could not build the upload request: {e}"))
    }
}

impl Destination for CustomUploader {
    fn upload(&self, ctx: &UploadCtx, bytes: &[u8], name: &str) -> Result<UploadResult, String> {
        let (headers, held) = self.headers_for(ctx, name)?;
        let exposed: Vec<&str> = held.iter().map(|s| s.expose()).collect();

        // One redaction point for the whole transfer, so no error path can be
        // added later that forgets it.
        self.transfer(ctx, bytes, name, headers)
            .map_err(|e| redact(e, &exposed))
    }

    /// Checks the configuration and the credentials, and says plainly that it
    /// has not talked to the server.
    ///
    /// There is no way to test a custom uploader for real without uploading:
    /// the only request it describes is the one that puts a file somewhere, and
    /// M3 §6 is explicit that a test must leave nothing behind. Reporting
    /// "connected" after a request that was never made would be worse than
    /// useless, so this reports exactly what it did check.
    fn test(&self, ctx: &UploadCtx) -> Result<String, String> {
        let mut missing: Vec<&str> = Vec::new();
        for header in &self.secret_headers {
            match ctx.optional_secret(header)? {
                Some(secret) if !secret.is_empty() => {}
                _ => missing.push(header.as_str()),
            }
        }
        if !missing.is_empty() {
            return Err(format!(
                "No value is saved for {}. Add {} in Settings › Destinations.",
                missing.join(", "),
                if missing.len() == 1 { "it" } else { "them" }
            ));
        }

        Ok(format!(
            "{} {} looks complete: {} header{} configured, and the response templates parse. \
             santi.sharex has not contacted the server — the only request this uploader describes \
             is the one that uploads a file, and a test must not leave one behind.",
            self.method.as_str(),
            self.url,
            self.headers.len() + self.secret_headers.len(),
            if self.headers.len() + self.secret_headers.len() == 1 {
                ""
            } else {
                "s"
            }
        ))
    }
}

impl CustomUploader {
    fn transfer(
        &self,
        ctx: &UploadCtx,
        bytes: &[u8],
        name: &str,
        headers: Vec<(String, String)>,
    ) -> Result<UploadResult, String> {
        let client = http_client()?;
        let total = bytes.len() as u64;
        let request = self.build(&client, ctx, total, name, headers)?;

        ctx.check_cancel()?;
        let reply = send(&client, request)?;

        if !(200..300).contains(&reply.status) {
            return Err(format!(
                "The server refused the upload (HTTP {}){}",
                reply.status,
                snippet(&reply.body)
            ));
        }

        // Parsed once, shared by the three templates below.
        let json: Option<Value> = serde_json::from_str(&reply.body).ok();
        let url = render("URL", &self.url_template, &reply.body, json.as_ref())?;
        if url.trim().is_empty() {
            return Err(
                "The upload was accepted, but the reply produced an empty link — check this uploader's URL template."
                    .to_string(),
            );
        }

        Ok(UploadResult {
            url: url.trim().to_string(),
            thumbnail_url: optional(render(
                "ThumbnailURL",
                &self.thumbnail_template,
                &reply.body,
                json.as_ref(),
            )),
            deletion_url: optional(render(
                "DeletionURL",
                &self.deletion_template,
                &reply.body,
                json.as_ref(),
            )),
        })
    }
}

/// A secondary link is a nicety, not a result. A `DeletionURL` template that
/// does not match the reply must not throw away an upload that succeeded.
fn optional(rendered: Result<String, String>) -> Option<String> {
    match rendered {
        Ok(value) if !value.trim().is_empty() => Some(value.trim().to_string()),
        Ok(_) => None,
        Err(e) => {
            eprintln!("santi.sharex: {e}");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A realistic file: the shape ShareX exports for a self-hosted uploader,
    /// API key in a header and a nested JSON reply.
    const REAL_SXCU: &str = r#"{
      "Version": "15.0.0",
      "Name": "my little host",
      "DestinationType": "ImageUploader, FileUploader",
      "RequestMethod": "POST",
      "RequestURL": "https://up.example.com/api/upload",
      "Headers": {
        "Authorization": "Bearer sk-not-a-real-token-000",
        "Accept": "application/json"
      },
      "Body": "MultipartFormData",
      "FileFormName": "file",
      "Parameters": {
        "album": "screenshots",
        "title": "{filename}"
      },
      "URL": "https://cdn.example.com/$json:data.id$.png",
      "ThumbnailURL": "https://cdn.example.com/$json:data.id$_t.png",
      "DeletionURL": "https://up.example.com/delete/$json:data.deleteToken$"
    }"#;

    fn settings_from(raw: &str) -> Settings {
        let mut settings = Settings::default();
        settings.custom_uploader = import(raw).expect("must import").settings;
        settings
    }

    #[test]
    fn a_representative_sxcu_round_trips() {
        let imported = import(REAL_SXCU).expect("a supported .sxcu must import");
        let config = &imported.settings;

        assert_eq!(config.name, "my little host");
        assert_eq!(config.request_method, "POST");
        assert_eq!(config.request_url, "https://up.example.com/api/upload");
        assert_eq!(config.body, "MultipartFormData");
        assert_eq!(config.file_form_name, "file");
        assert_eq!(config.parameters.get("album").unwrap(), "screenshots");
        assert_eq!(config.parameters.get("title").unwrap(), "{filename}");
        assert_eq!(config.url, "https://cdn.example.com/$json:data.id$.png");

        // The API key went to the credential store; the harmless header stayed.
        assert_eq!(config.headers.get("Accept").unwrap(), "application/json");
        assert!(
            !config.headers.contains_key("Authorization"),
            "the Authorization header was left in the config"
        );
        assert_eq!(config.secret_headers, vec!["Authorization".to_string()]);
        assert_eq!(imported.secrets.len(), 1);
        // The field is the header name, which is what `mod.rs` reports as this
        // destination's secret field and what `ctx.secret` reads back.
        assert_eq!(imported.secrets[0].field(), "Authorization");

        // And the whole config is safe to write to settings.json.
        let persisted = serde_json::to_string(config).unwrap();
        assert!(
            !persisted.contains("sk-not-a-real-token-000"),
            "the API key reached settings.json"
        );

        // Finally, the stored config builds a working destination — the round
        // trip that matters, since nothing else reads `CustomUploaderSettings`.
        let uploader = CustomUploader::from_settings(&settings_from(REAL_SXCU))
            .expect("an imported uploader must load");
        assert_eq!(uploader.method, Method::Post);
        assert_eq!(uploader.body, BodyKind::MultipartFormData);
        assert_eq!(uploader.secret_headers, vec!["Authorization".to_string()]);
    }

    /// Every unsupported thing M3 §4 lists, each refused with its own name in
    /// the message. This is the test that stops "accepted, then mysteriously
    /// broken at upload" from coming back.
    #[test]
    fn every_unsupported_field_is_refused_by_name() {
        let cases: [(&str, &str); 7] = [
            (
                r#"{"RequestURL":"https://x.test/u","FileFormName":"f","OAuth2":{"Scope":"upload"}}"#,
                "OAuth2",
            ),
            (
                r#"{"RequestURL":"https://x.test/u","FileFormName":"f","RegexList":["https?://.+"]}"#,
                "RegexList",
            ),
            (
                r#"{"RequestURL":"https://x.test/u","FileFormName":"f","Body":"FormURLEncoded"}"#,
                "Body",
            ),
            (
                r#"{"RequestURL":"https://x.test/u","FileFormName":"f","RequestBodyType":"JSON"}"#,
                "RequestBodyType",
            ),
            (
                r#"{"RequestURL":"https://x.test/u","FileFormName":"f","RequestMethod":"DELETE"}"#,
                "RequestMethod",
            ),
            (
                r#"{"RequestURL":"https://x.test/u","FileFormName":"f","DestinationType":"URLShortener"}"#,
                "DestinationType",
            ),
            (
                r#"{"RequestURL":"https://x.test/u","FileFormName":"f","URL":"$regex:1,1$"}"#,
                "regex",
            ),
        ];

        for (raw, must_name) in cases {
            let error = import(raw).unwrap_err();
            assert!(
                error.contains(must_name),
                "refusing {raw} produced \"{error}\", which never mentions {must_name}"
            );
        }
    }

    /// The mirror of the above: the refusals must not be so broad that ordinary
    /// files trip them. A `null` OAuth2 is what ShareX writes for "not using
    /// OAuth", and reading it as "uses OAuth" would refuse most real exports.
    #[test]
    fn a_null_field_is_not_a_used_field() {
        let raw = r#"{
          "RequestURL": "https://x.test/u",
          "FileFormName": "f",
          "OAuth2": null,
          "RegexList": null,
          "DestinationType": "ImageUploader"
        }"#;
        assert!(
            import(raw).is_ok(),
            "a file with null placeholders was refused"
        );
    }

    #[test]
    fn a_file_uploader_is_accepted_and_a_text_uploader_is_not() {
        let with = |kind: &str| {
            format!(
                r#"{{"RequestURL":"https://x.test/u","FileFormName":"f","DestinationType":"{kind}"}}"#
            )
        };
        assert!(import(&with("FileUploader")).is_ok());
        assert!(import(&with("ImageUploader")).is_ok());
        assert!(import(&with("TextUploader")).is_err());
        assert!(import(&with("URLSharingService")).is_err());
    }

    #[test]
    fn a_multipart_uploader_without_a_file_field_is_refused() {
        let raw = r#"{"RequestURL":"https://x.test/u","Body":"MultipartFormData"}"#;
        let error = import(raw).unwrap_err();
        assert!(error.contains("FileFormName"), "{error}");

        // Binary bodies have no form field, so the same file is fine there.
        let raw = r#"{"RequestURL":"https://x.test/u","Body":"Binary"}"#;
        assert!(import(raw).is_ok());
    }

    #[test]
    fn a_non_http_request_url_is_refused() {
        let raw = r#"{"RequestURL":"file:///C:/Users/santi/loot","FileFormName":"f"}"#;
        let error = import(raw).unwrap_err();
        assert!(error.contains("RequestURL"), "{error}");
    }

    #[test]
    fn arguments_is_accepted_as_the_old_name_for_parameters() {
        let raw = r#"{
          "RequestURL": "https://x.test/u",
          "FileFormName": "f",
          "RequestType": "PUT",
          "Arguments": { "key": "abc", "kind": "png" }
        }"#;
        let config = import(raw).expect("an older .sxcu must import").settings;
        assert_eq!(config.request_method, "PUT");
        assert_eq!(config.parameters.get("kind").unwrap(), "png");
        // `key` looks like a credential, but Parameters are not headers — only
        // Headers are split out, because only they are sent as credentials.
        assert_eq!(config.parameters.get("key").unwrap(), "abc");
    }

    /// A hand-edited `settings.json` gets the same treatment an imported file
    /// does, rather than failing halfway through a request.
    #[test]
    fn a_hand_edited_settings_file_is_validated_too() {
        let mut settings = settings_from(REAL_SXCU);
        settings.custom_uploader.request_method = "DELETE".into();
        assert!(CustomUploader::from_settings(&settings)
            .unwrap_err()
            .contains("RequestMethod"));

        let mut settings = settings_from(REAL_SXCU);
        settings.custom_uploader.url = "$regex:1,1$".into();
        assert!(CustomUploader::from_settings(&settings).is_err());

        // And a fresh install, with nothing imported, says so rather than
        // producing an uploader pointed at "".
        let error = CustomUploader::from_settings(&Settings::default()).unwrap_err();
        assert!(error.contains(".sxcu"), "{error}");
    }

    #[test]
    fn credential_headers_are_recognised_and_ordinary_ones_are_not() {
        for name in [
            "Authorization",
            "X-Api-Key",
            "api_key",
            "X-Auth-Token",
            "Cookie",
            "X-Client-Secret",
            "Proxy-Authorization",
        ] {
            assert!(
                looks_like_a_credential(name),
                "{name} was not treated as a secret"
            );
        }
        for name in [
            "Accept",
            "Accept-Encoding",
            "Content-Type",
            "User-Agent",
            "X-Requested-With",
            "Referer",
        ] {
            assert!(
                !looks_like_a_credential(name),
                "{name} was treated as a secret"
            );
        }
    }

    /// [`install`] writes a header's value under [`KIND`]; an upload reads it
    /// back under `DestinationKind::Custom.id()`. If those ever differ, every
    /// imported uploader silently loses its API key — Settings would still show
    /// it as configured, because `destination_status` reads the same key the
    /// upload does.
    #[test]
    fn the_import_writes_where_uploads_read() {
        assert_eq!(KIND, super::super::DestinationKind::Custom.id());
    }

    /// `Debug` is derived on [`CustomUploader`], so this pins the reason that is
    /// safe: it holds header *names*, never values.
    #[test]
    fn the_destination_holds_no_header_values_to_print() {
        let uploader = CustomUploader::from_settings(&settings_from(REAL_SXCU)).unwrap();
        let printed = format!("{uploader:?}");
        assert!(printed.contains("Authorization"));
        assert!(!printed.contains("sk-not-a-real-token-000"), "{printed}");
    }

    #[test]
    fn a_header_secret_never_prints_its_value() {
        let secret = HeaderSecret {
            field: "Authorization".to_string(),
            value: "Bearer sk-not-a-real-token-000".to_string(),
        };
        let printed = format!("{secret:?}");
        assert!(printed.contains("Authorization"));
        assert!(
            !printed.contains("sk-not-a-real-token-000"),
            "Debug printed the secret: {printed}"
        );
    }

    #[test]
    fn templates_read_json_paths_response_bodies_and_literals() {
        let body = r#"{"data":{"id":"abc123","files":[{"url":"https://cdn.test/a.png"}],"n":7}}"#;
        let json: Value = serde_json::from_str(body).unwrap();

        let cases = [
            (
                "https://cdn.test/$json:data.id$.png",
                "https://cdn.test/abc123.png",
            ),
            ("$json:data.files[0].url$", "https://cdn.test/a.png"),
            ("$json:data.n$", "7"),
            ("$response$", body),
            ("plain text", "plain text"),
            ("$json:data.id$-$json:data.n$", "abc123-7"),
        ];
        for (template, expected) in cases {
            let chunks = parse_template("URL", template).expect(template);
            assert_eq!(render("URL", &chunks, body, Some(&json)).unwrap(), expected);
        }
    }

    #[test]
    fn a_missing_json_path_names_the_path_it_could_not_find() {
        let body = r#"{"data":{"id":"abc"}}"#;
        let json: Value = serde_json::from_str(body).unwrap();
        let chunks = parse_template("URL", "$json:data.link$").unwrap();
        let error = render("URL", &chunks, body, Some(&json)).unwrap_err();
        assert!(error.contains("data.link"), "{error}");
    }

    #[test]
    fn a_non_json_reply_is_only_a_problem_for_templates_that_read_json() {
        let body = "https://cdn.test/a.png";
        let chunks = parse_template("URL", "$response$").unwrap();
        assert_eq!(render("URL", &chunks, body, None).unwrap(), body);

        let chunks = parse_template("URL", "$json:url$").unwrap();
        assert!(render("URL", &chunks, body, None).is_err());
    }

    #[test]
    fn an_unterminated_placeholder_is_caught_at_import() {
        let raw = r#"{"RequestURL":"https://x.test/u","FileFormName":"f","URL":"https://x.test/$json:id"}"#;
        assert!(import(raw).is_err());
    }

    #[test]
    fn filename_substitution_reaches_headers_and_parameters() {
        assert_eq!(substitute("{filename}", "shot.png"), "shot.png");
        assert_eq!(substitute("name={filename}!", "shot.png"), "name=shot.png!");
        assert_eq!(substitute("nothing", "shot.png"), "nothing");
    }

    /// The rule §2 exists for, checked on the function every upload error path
    /// in this module and `imgur.rs` runs through.
    #[test]
    fn redaction_removes_secrets_and_leaves_short_ones_alone() {
        let message = redact(
            "the server rejected Bearer sk-not-a-real-token-000 (401)".to_string(),
            &["sk-not-a-real-token-000"],
        );
        assert!(!message.contains("sk-not-a-real-token-000"), "{message}");
        assert!(message.contains("<redacted>"), "{message}");

        // A short value would turn the message into confetti without hiding
        // anything worth hiding.
        assert_eq!(redact("HTTP 401 at /api".to_string(), &["a"]), "HTTP 401 at /api");
    }

    #[test]
    fn a_failing_reply_is_quoted_but_bounded() {
        assert_eq!(snippet(""), "");
        assert_eq!(snippet("  \n "), "");
        assert_eq!(
            snippet("{\n  \"error\": \"nope\"\n}"),
            ": { \"error\": \"nope\" }"
        );
        let long = "x".repeat(ERROR_SNIPPET * 3);
        let cut = snippet(&long);
        assert!(cut.chars().count() < long.len());
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn binary_bodies_are_typed_from_the_file_name() {
        assert_eq!(content_type_for("region_2026.png"), "image/png");
        assert_eq!(content_type_for("SHOT.PNG"), "image/png");
        assert_eq!(content_type_for("noextension"), "application/octet-stream");
    }
}
