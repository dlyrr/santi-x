//! Anonymous Imgur uploads (M3 §3).
//!
//! # No embedded client ID
//!
//! ShareX ships its own. santi.sharex will not, because this repository is
//! public: an ID committed here would be scraped, shared, hammered and
//! rate-limited into uselessness — for every user of this app, not just the one
//! who abused it. So the user registers their own, and it lives in Windows
//! Credential Manager like any other key (M3 §2). [`NO_CLIENT_ID`] is the whole
//! setup path in one message, because a bare "not configured" next to an empty
//! field reads like a bug in the app.
//!
//! # Rate limits are an answer, not a failure
//!
//! Imgur's anonymous quota is per client ID and per hour, and hitting it is a
//! completely normal thing to do after a busy afternoon. "Upload failed" leaves
//! the user retrying into a wall; [`rate_limit_message`] reads the
//! `X-RateLimit-*` headers and says *when* the quota comes back instead.
//!
//! # One endpoint
//!
//! `POST https://api.imgur.com/3/image` is the only request this file makes.
//! That is deliberate: it is the endpoint M3 §3 names, and a capture tool has no
//! business opening connections the contract did not ask for. It is also why
//! [`Imgur::test`] does not talk to Imgur — see the note there.

use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::blocking::multipart::{Form, Part};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde_json::Value;

use crate::store::Settings;

use super::custom::{redact, send, snippet};
use super::{http_client, Destination, UploadCtx, UploadResult};

/// The one secret this destination has (M3 §2, §3). Matches the field name
/// `mod.rs` reports in `destination_status`; the credential-store namespace is
/// `DestinationKind::Imgur.id()`, which `UploadCtx` supplies.
pub const CLIENT_ID_FIELD: &str = "clientId";

const ENDPOINT: &str = "https://api.imgur.com/3/image";

/// The multipart field Imgur reads the image out of.
const IMAGE_FIELD: &str = "image";

/// Shown whenever there is no usable client ID. The registration path is spelled
/// out because "Imgur is not configured" with an empty text box next to it is
/// indistinguishable from a broken build.
pub const NO_CLIENT_ID: &str = "santi.sharex has no Imgur Client ID to upload with. \
It deliberately ships without one: this is a public repository, and an embedded ID would be \
extracted and rate-limited into uselessness for everybody. \
Register your own at https://api.imgur.com/oauth2/addclient — choose \"Anonymous usage without \
user authorization\" — and paste the Client ID into Settings › Destinations › Imgur.";

/// Anonymous Imgur has nothing non-secret to configure, which is why there is no
/// `ImgurSettings` in `store.rs` and nothing to hold here.
pub struct Imgur;

impl Imgur {
    pub fn from_settings(_settings: &Settings) -> Result<Self, String> {
        Ok(Self)
    }

    /// The stored client ID, or the setup message.
    ///
    /// Whether the credential store is empty or unreachable, what the user has
    /// to do about it is the same — set a Client ID — so they get the actionable
    /// message and the real reason goes to stderr.
    fn client_id(ctx: &UploadCtx) -> Result<super::secrets::Secret, String> {
        match ctx.optional_secret(CLIENT_ID_FIELD) {
            Ok(Some(secret)) if !secret.is_empty() => Ok(secret),
            Ok(_) => Err(NO_CLIENT_ID.to_string()),
            Err(e) => {
                eprintln!("santi.sharex: could not read the Imgur client ID: {e}");
                Err(NO_CLIENT_ID.to_string())
            }
        }
    }
}

impl Destination for Imgur {
    fn upload(&self, ctx: &UploadCtx, bytes: &[u8], name: &str) -> Result<UploadResult, String> {
        let client_id = Self::client_id(ctx)?;

        // Every error below is redacted against the ID before it is returned.
        // Imgur has no reason to echo it back, but "has no reason to" is not a
        // guarantee, and this message ends up in toasts and issue reports.
        transfer(ctx, bytes, name, client_id.expose())
            .map_err(|e| redact(e, &[client_id.expose()]))
    }

    /// Checks the stored Client ID and says plainly that it has not asked Imgur.
    ///
    /// Imgur has no "is this ID valid" endpoint that M3 §3 names, and the one it
    /// does name uploads a picture — which a test must not leave behind. So this
    /// reports exactly what it verified: that a Client ID is saved and can be put
    /// in a header. Claiming "connected" after a request that was never made
    /// would be worse than saying nothing.
    fn test(&self, ctx: &UploadCtx) -> Result<String, String> {
        let client_id = Self::client_id(ctx)?;
        let raw = client_id.expose();

        HeaderValue::from_str(&format!("Client-ID {raw}")).map_err(|_| {
            "That Imgur Client ID has characters in it that cannot be sent in an HTTP header — \
             check for a stray space or line break when pasting it."
                .to_string()
        })?;

        Ok(format!(
            "A Client ID is saved ({} characters) and is usable in a request header. \
             Imgur only checks it when something is actually uploaded, and a connection test \
             must not leave a picture behind, so this does not prove Imgur will accept it.",
            raw.chars().count()
        ))
    }
}

fn transfer(
    ctx: &UploadCtx,
    bytes: &[u8],
    name: &str,
    client_id: &str,
) -> Result<UploadResult, String> {
    let http = http_client()?;
    let total = bytes.len() as u64;

    let part = Part::reader_with_length(ctx.reader(), total)
        .file_name(name.to_string())
        .mime_str("image/png")
        .map_err(|e| format!("could not describe the image to Imgur: {e}"))?;

    // Built by hand rather than through `.header(name, format!(…))` so a client
    // ID with a stray newline in it is refused *here*, with a message that does
    // not contain it, instead of inside reqwest's builder.
    let authorization = HeaderValue::from_str(&format!("Client-ID {client_id}")).map_err(|_| {
        "That Imgur Client ID has characters in it that cannot be sent in an HTTP header — \
         check for a stray space or line break when pasting it."
            .to_string()
    })?;

    let request = http
        .post(ENDPOINT)
        .header(AUTHORIZATION, authorization)
        .multipart(Form::new().part(IMAGE_FIELD, part))
        .build()
        .map_err(|e| format!("could not build the Imgur request: {e}"))?;

    ctx.check_cancel()?;
    let reply = send(&http, request)?;
    let json: Option<Value> = serde_json::from_str(&reply.body).ok();

    if reply.status == 429 {
        return Err(rate_limit_message(&reply.headers, now_unix()));
    }
    if reply.status == 403 {
        return Err(format!(
            "Imgur rejected that Client ID (HTTP 403){}. Check it was copied whole, and that the \
             application is registered for \"Anonymous usage without user authorization\".",
            described(json.as_ref())
        ));
    }
    if !(200..300).contains(&reply.status) {
        return Err(format!(
            "Imgur refused the upload (HTTP {}){}",
            reply.status,
            // Imgur's own error text if it gave one, and the raw body if it did
            // not — a 502 from a proxy in front of Imgur has neither.
            match described(json.as_ref()) {
                described if described.is_empty() => snippet(&reply.body),
                described => described,
            }
        ));
    }

    let json = json.ok_or("Imgur accepted the upload but did not reply with JSON.")?;
    // Imgur reports application-level failures with a matching status code, so
    // this is belt and braces — but a `success: false` that came back 200 must
    // not be presented as an uploaded image.
    if json.get("success").and_then(Value::as_bool) == Some(false) {
        return Err(format!(
            "Imgur refused the upload{}.",
            described(Some(&json))
        ));
    }

    let data = json
        .get("data")
        .ok_or("Imgur's reply had no image in it, so there is no link to copy.")?;
    let url = data
        .get("link")
        .and_then(Value::as_str)
        .ok_or("Imgur accepted the upload but did not return a link.")?;

    Ok(UploadResult {
        url: url.to_string(),
        // An anonymous upload cannot be deleted by id — the delete hash is the
        // only handle to it, and this is where it is spent. Worth keeping:
        // without it an anonymous upload is genuinely permanent.
        deletion_url: data
            .get("deletehash")
            .and_then(Value::as_str)
            .filter(|hash| !hash.is_empty())
            .map(|hash| format!("https://imgur.com/delete/{hash}")),
        // Imgur does expose thumbnails by suffixing the image id, but the suffix
        // is a convention rather than part of the reply. Deriving one would mean
        // handing the user a link santi.sharex guessed at.
        thumbnail_url: None,
    })
}

/// `" — <what Imgur said>"`, or empty.
fn described(json: Option<&Value>) -> String {
    imgur_error(json)
        .map(|e| format!(" — {e}"))
        .unwrap_or_default()
}

/// Imgur's error text, which arrives as either `data.error` or, on newer
/// endpoints, `data.error.message`.
fn imgur_error(json: Option<&Value>) -> Option<String> {
    let error = json?.get("data")?.get("error")?;
    match error {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Object(_) => error
            .get("message")
            .and_then(Value::as_str)
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// rate limits
// ---------------------------------------------------------------------------

/// Imgur's quotas — the credit pool and the per-hour POST limit — each with a
/// "how many are left" header and a "when does it reset" one.
const USER_REMAINING: &str = "x-ratelimit-userremaining";
const USER_RESET: &str = "x-ratelimit-userreset";
const CLIENT_REMAINING: &str = "x-ratelimit-clientremaining";
const CLIENT_RESET: &str = "x-ratelimit-clientreset";
const POST_REMAINING: &str = "x-post-rate-limit-remaining";
const POST_RESET: &str = "x-post-rate-limit-reset";

/// Anything above this is read as a Unix timestamp rather than as a number of
/// seconds to wait.
///
/// Imgur documents the `*Reset` headers as timestamps and has, at various
/// points, sent seconds-until-reset instead. Both readings are unambiguous in
/// practice — a wait is minutes or hours, a timestamp is a billion and a half —
/// so this picks the one that is not nonsense rather than trusting the docs.
/// Roughly 2001-09-09.
const EPOCH_THRESHOLD: i64 = 1_000_000_000;

/// Says when the quota comes back, which is the only useful thing to say here.
fn rate_limit_message(headers: &HeaderMap, now: i64) -> String {
    match seconds_until_reset(headers, now) {
        Some(seconds) => format!(
            "Imgur is rate-limiting this Client ID, so nothing was uploaded. Try again {}. \
             The anonymous quota is per Client ID and per hour — retrying sooner will not help.",
            humanize(seconds)
        ),
        None => "Imgur is rate-limiting this Client ID, so nothing was uploaded, and it did not say \
                 when the quota resets. The anonymous quota is per Client ID and per hour, so try \
                 again later rather than straight away."
            .to_string(),
    }
}

/// How long to wait, from whichever header actually describes the limit that was
/// hit.
///
/// Order matters. `Retry-After` is the server telling us directly. Failing that,
/// a quota whose `*Remaining` reads zero is the one that refused this upload, so
/// its reset is the honest answer; falling back to the longest wait on offer is
/// better than the shortest, which would invite a retry into the same 429.
fn seconds_until_reset(headers: &HeaderMap, now: i64) -> Option<i64> {
    if let Some(after) = number(headers, "retry-after") {
        return Some(after.max(0));
    }

    let quotas = [
        (POST_REMAINING, POST_RESET),
        (USER_REMAINING, USER_RESET),
        (CLIENT_REMAINING, CLIENT_RESET),
    ];

    let mut exhausted: Option<i64> = None;
    let mut any: Option<i64> = None;
    for (remaining, reset) in quotas {
        let Some(wait) = number(headers, reset).map(|v| as_wait(v, now)) else {
            continue;
        };
        any = Some(any.map_or(wait, |a: i64| a.max(wait)));
        if number(headers, remaining) == Some(0) {
            exhausted = Some(exhausted.map_or(wait, |e: i64| e.max(wait)));
        }
    }

    exhausted.or(any).map(|s| s.max(0))
}

fn as_wait(value: i64, now: i64) -> i64 {
    if value > EPOCH_THRESHOLD {
        value - now
    } else {
        value
    }
}

fn number(headers: &HeaderMap, name: &str) -> Option<i64> {
    headers
        .get(name)?
        .to_str()
        .ok()?
        .trim()
        .parse::<f64>()
        .ok()
        .map(|v| v as i64)
}

/// "in 40 seconds" / "in about 12 minutes" / "in about 2 hours".
///
/// Deliberately vague above a minute: the headers are not precise enough to
/// promise a wall-clock time, and a countdown that is thirty seconds out reads
/// as a lie when the retry fails.
fn humanize(seconds: i64) -> String {
    if seconds <= 0 {
        return "now".to_string();
    }
    if seconds < 90 {
        return format!("in {seconds} seconds");
    }
    let minutes = (seconds + 59) / 60;
    if minutes < 90 {
        return format!("in about {minutes} minutes");
    }
    format!("in about {} hours", (minutes + 59) / 60)
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderName;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    const NOW: i64 = 1_785_835_580;

    #[test]
    fn a_rate_limit_says_when_to_retry_not_that_it_failed() {
        let message = rate_limit_message(
            &headers(&[
                (USER_REMAINING, "0"),
                (USER_RESET, &(NOW + 2_400).to_string()),
            ]),
            NOW,
        );
        assert!(message.contains("40 minutes"), "{message}");
        assert!(!message.to_lowercase().contains("failed"), "{message}");
    }

    /// The header is documented as a timestamp and has been observed as a plain
    /// wait. Both must produce the same answer, or one of the two readings tells
    /// the user to come back in fifty-six thousand years.
    #[test]
    fn a_reset_header_is_read_as_a_timestamp_or_as_a_wait() {
        let as_timestamp = seconds_until_reset(
            &headers(&[(USER_REMAINING, "0"), (USER_RESET, &(NOW + 600).to_string())]),
            NOW,
        );
        let as_wait =
            seconds_until_reset(&headers(&[(USER_REMAINING, "0"), (USER_RESET, "600")]), NOW);
        assert_eq!(as_timestamp, Some(600));
        assert_eq!(as_wait, Some(600));
    }

    #[test]
    fn retry_after_wins_over_everything_else() {
        let map = headers(&[
            ("retry-after", "30"),
            (USER_REMAINING, "0"),
            (USER_RESET, "3600"),
        ]);
        assert_eq!(seconds_until_reset(&map, NOW), Some(30));
    }

    /// Whichever quota reads zero is the one that refused this upload; a reset
    /// from a quota with credits left would send the user back too early.
    #[test]
    fn the_exhausted_quota_is_the_one_that_is_reported() {
        let map = headers(&[
            (POST_REMAINING, "1200"),
            (POST_RESET, "60"),
            (USER_REMAINING, "0"),
            (USER_RESET, "3600"),
        ]);
        assert_eq!(seconds_until_reset(&map, NOW), Some(3600));
    }

    #[test]
    fn a_429_with_no_headers_still_says_something_useful() {
        let message = rate_limit_message(&HeaderMap::new(), NOW);
        assert!(message.contains("rate-limiting"), "{message}");
        assert!(message.contains("later"), "{message}");
    }

    #[test]
    fn waits_are_described_at_a_sensible_resolution() {
        assert_eq!(humanize(0), "now");
        assert_eq!(humanize(-5), "now");
        assert_eq!(humanize(45), "in 45 seconds");
        assert_eq!(humanize(600), "in about 10 minutes");
        assert_eq!(humanize(7_200), "in about 2 hours");
    }

    #[test]
    fn imgur_errors_are_read_in_both_shapes() {
        let flat: Value = serde_json::from_str(r#"{"data":{"error":"Bad Request"}}"#).unwrap();
        assert_eq!(imgur_error(Some(&flat)).as_deref(), Some("Bad Request"));

        let nested: Value =
            serde_json::from_str(r#"{"data":{"error":{"message":"File is over the size limit"}}}"#)
                .unwrap();
        assert_eq!(
            imgur_error(Some(&nested)).as_deref(),
            Some("File is over the size limit")
        );

        assert_eq!(imgur_error(None), None);
        let empty: Value = serde_json::from_str(r#"{"data":{}}"#).unwrap();
        assert_eq!(imgur_error(Some(&empty)), None);
    }

    /// The setup path *is* the message. If it stops naming the registration
    /// page, the "no embedded client ID" decision starts reading as a missing
    /// feature.
    #[test]
    fn the_missing_client_id_message_carries_the_whole_setup_path() {
        assert!(NO_CLIENT_ID.contains("https://api.imgur.com/oauth2/addclient"));
        assert!(NO_CLIENT_ID.contains("Anonymous usage"));
    }

    /// The one thing this file must never do.
    #[test]
    fn no_error_path_can_carry_the_client_id() {
        let id = "abc123def456abc0";
        let leaked = redact(
            format!("Imgur refused the upload — Client-ID {id} is unknown"),
            &[id],
        );
        assert!(!leaked.contains(id), "{leaked}");
    }
}
