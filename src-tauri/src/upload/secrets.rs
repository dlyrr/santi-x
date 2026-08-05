//! Windows Credential Manager — the only place a santi.sharex secret is ever stored,
//! and the only place one is ever in the clear (M3 §2).
//!
//! FTP passwords and API keys never touch `settings.json`, never appear in a log
//! line, never go into an error message and never cross the IPC back to the
//! frontend. The frontend may **write** one ([`set`]), **remove** one
//! ([`clear`]) and ask *whether* one is set ([`exists`]). It may not read one
//! back, and the type system says so rather than a comment: [`get`] is
//! `pub(in crate::upload)`, so it is reachable from the four files under
//! `src/upload/` and from nowhere else. A `#[tauri::command]` cannot call it
//! without moving into this module tree, which would be a visible edit rather
//! than an accident.
//!
//! # Key derivation
//!
//! `santi.sharex/<destination-id>/<field>`, as a `CRED_TYPE_GENERIC` target name.
//! The service prefix keeps every entry together (and identifiable) in the
//! Credential Manager UI; the destination id namespaces one destination's
//! `password` from another's. Both parts are normalised by
//! [`sanitize_key_part`] so a header name out of an imported `.sxcu` cannot
//! reach in and forge a key belonging to another destination.
//!
//! Persistence is `CRED_PERSIST_LOCAL_MACHINE`: the credential is tied to this
//! Windows account on this machine, survives a reboot, and does not roam to a
//! domain profile — which keeps it out of backups of the config directory, out
//! of the repo, and out of anything a user might paste into an issue.
//!
//! # Zeroing
//!
//! Every plaintext buffer this module touches is overwritten before it is
//! released: the UTF-8 copy handed to `CredWriteW`, the blob `CredReadW`
//! allocates (zeroed before `CredFree`, since it is ordinary process heap), and
//! [`Secret`]'s own buffer on drop. That is not a guarantee against a memory
//! dump taken mid-upload — nothing here is — it is a guarantee that the window
//! is as short as the code can make it.

use std::fmt;
use std::ptr;
use std::sync::atomic::{compiler_fence, Ordering};

use windows::core::{HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::ERROR_NOT_FOUND;
use windows::Win32::Security::Credentials::{
    CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_FLAGS,
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC,
};

/// Prefix on every credential this app owns. Also what makes them findable in
/// `rundll32 keymgr.dll,KRShowKeyMgr` when a user wants to see what is stored.
pub const SERVICE: &str = "santi.sharex";

/// `CRED_MAX_CREDENTIAL_BLOB_SIZE`. Windows rejects anything larger with a bare
/// `ERROR_INVALID_PARAMETER`, which reads as "santi.sharex is broken" rather than
/// "that key is implausibly long", so it is checked here instead.
const MAX_BLOB_BYTES: usize = 5 * 512;

// ---------------------------------------------------------------------------
// the value
// ---------------------------------------------------------------------------

/// A secret in memory, on its way from the credential store to exactly one
/// request.
///
/// Deliberately not `Serialize`, not `Display` and not `Clone`: the only way to
/// see the plaintext is [`Secret::expose`], which — like [`get`] — is confined
/// to `crate::upload`. Its `Debug` prints a placeholder, so a `{:?}` on a
/// struct that happens to contain one cannot leak it into a log line.
pub struct Secret {
    bytes: Vec<u8>,
}

impl Secret {
    /// Errors rather than lossily converting: a credential that is not UTF-8 was
    /// written by something else under our key, and guessing at its encoding
    /// would send whatever fell out to a server.
    fn new(mut bytes: Vec<u8>) -> Result<Self, String> {
        if std::str::from_utf8(&bytes).is_err() {
            zero(&mut bytes);
            return Err(
                "the saved credential is not readable text — set it again in Settings".to_string(),
            );
        }
        Ok(Self { bytes })
    }

    /// The plaintext. Callable only from the upload modules; keep the borrow as
    /// short as the request that needs it.
    pub(in crate::upload) fn expose(&self) -> &str {
        // Checked in `new`, so this cannot fail; `unwrap_or` rather than
        // `expect` so a future refactor degrades to an empty header instead of
        // panicking mid-upload.
        std::str::from_utf8(&self.bytes).unwrap_or("")
    }

    pub(in crate::upload) fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        zero(&mut self.bytes);
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

// ---------------------------------------------------------------------------
// keys
// ---------------------------------------------------------------------------

/// One component of a credential key.
///
/// Lowercased because the fields that reach here include HTTP header names out
/// of an imported `.sxcu`, and `Authorization` and `authorization` are the same
/// header — storing them as two credentials would mean a key the user saved
/// under one spelling silently missing under the other. Everything outside
/// `[a-z0-9._-]` becomes `_`, which is what stops a field named `../imgur` from
/// addressing another destination's slot.
fn sanitize_key_part(raw: &str) -> String {
    let cleaned: String = raw
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();

    if cleaned.is_empty() {
        // A key with an empty component would collide with every other empty
        // one. Callers validate before they get here; this is the backstop.
        "unnamed".to_string()
    } else {
        cleaned
    }
}

/// `santi.sharex/<destination-id>/<field>`.
pub fn target_name(destination: &str, field: &str) -> String {
    format!(
        "{SERVICE}/{}/{}",
        sanitize_key_part(destination),
        sanitize_key_part(field)
    )
}

// ---------------------------------------------------------------------------
// operations
// ---------------------------------------------------------------------------

/// Writes (or overwrites) one secret.
///
/// Rejects a blank value: an empty credential is indistinguishable from a
/// misconfigured one at upload time. The command layer turns "the user cleared
/// the field" into a [`clear`] call, so that policy lives in exactly one place.
pub fn set(destination: &str, field: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err("that value is empty — clear the credential instead".to_string());
    }

    let target = target_name(destination, field);
    let mut target_wide = wide(&target);
    let mut blob = value.as_bytes().to_vec();

    if blob.len() > MAX_BLOB_BYTES {
        zero(&mut blob);
        return Err(format!(
            "that value is {} bytes; Windows Credential Manager accepts at most {MAX_BLOB_BYTES}",
            value.len()
        ));
    }

    let credential = CREDENTIALW {
        Flags: CRED_FLAGS(0),
        Type: CRED_TYPE_GENERIC,
        TargetName: PWSTR(target_wide.as_mut_ptr()),
        Comment: PWSTR::null(),
        LastWritten: Default::default(),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE,
        AttributeCount: 0,
        Attributes: ptr::null_mut(),
        TargetAlias: PWSTR::null(),
        // Left null on purpose: `CRED_TYPE_GENERIC` does not need one, and the
        // user name is another string that would end up on screen in the
        // Credential Manager UI for no benefit.
        UserName: PWSTR::null(),
    };

    // SAFETY: every pointer in `credential` is into a live local that outlives
    // the call, and `CredWriteW` copies what it needs before returning.
    let result = unsafe { CredWriteW(&credential, 0) };

    // Before the error path, and unconditionally: this is our plaintext copy.
    zero(&mut blob);

    result.map_err(|e| {
        // `e` is a Win32 status. It never contains the value — but say what
        // failed, not what was being written.
        format!("could not save that credential ({target}): {}", e.message())
    })
}

/// Removes one secret. Already absent is success: `clear` is what the UI calls
/// when the user empties a field, and "there was nothing to remove" is the state
/// it asked for.
pub fn clear(destination: &str, field: &str) -> Result<(), String> {
    let target = target_name(destination, field);
    let target_wide = wide(&target);

    // SAFETY: `target_wide` is a live NUL-terminated UTF-16 buffer.
    match unsafe { CredDeleteW(PCWSTR(target_wide.as_ptr()), CRED_TYPE_GENERIC, None) } {
        Ok(()) => Ok(()),
        Err(e) if is_not_found(&e) => Ok(()),
        Err(e) => Err(format!(
            "could not remove that credential ({target}): {}",
            e.message()
        )),
    }
}

/// Whether a secret is set. This is the *only* thing the frontend gets to learn
/// about a stored value (M3 §2).
///
/// A read that fails for any reason other than "not found" answers `false` and
/// says so on stderr: santi.sharex cannot prove the credential is there, and claiming
/// it is would present a destination as configured that will fail at upload.
pub fn exists(destination: &str, field: &str) -> bool {
    match get(destination, field) {
        Ok(Some(secret)) => !secret.is_empty(),
        Ok(None) => false,
        Err(e) => {
            eprintln!("santi.sharex: {e}");
            false
        }
    }
}

/// Reads one secret. **Upload paths only** — see the module docs for why this is
/// `pub(in crate::upload)` and must stay that way.
pub(in crate::upload) fn get(destination: &str, field: &str) -> Result<Option<Secret>, String> {
    let target = target_name(destination, field);
    let target_wide = wide(&target);
    let mut raw: *mut CREDENTIALW = ptr::null_mut();

    // SAFETY: `target_wide` is live and NUL-terminated; `raw` receives an
    // allocation this function owns until `CredFree` below.
    let result = unsafe {
        CredReadW(
            PCWSTR(target_wide.as_ptr()),
            CRED_TYPE_GENERIC,
            None,
            &mut raw,
        )
    };
    match result {
        Ok(()) => {}
        Err(e) if is_not_found(&e) => return Ok(None),
        Err(e) => {
            return Err(format!(
                "could not read that credential ({target}): {}",
                e.message()
            ))
        }
    }
    if raw.is_null() {
        // Documented as impossible after a successful read; treated as "absent"
        // rather than dereferenced.
        return Ok(None);
    }

    // SAFETY: `CredReadW` returned success, so `raw` points at exactly one
    // `CREDENTIALW` whose blob is `CredentialBlobSize` bytes long, and it is
    // ours until `CredFree`. The blob is ordinary process heap, so zeroing it
    // before handing it back is sound — and it is the reason this is a raw
    // pointer walk rather than a `&*raw` borrow.
    let bytes = unsafe {
        let len = (*raw).CredentialBlobSize as usize;
        let blob = (*raw).CredentialBlob;
        let copied = if len == 0 || blob.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(blob, len).to_vec()
        };
        if !blob.is_null() && len > 0 {
            zero(std::slice::from_raw_parts_mut(blob, len));
        }
        CredFree(raw as *const _);
        copied
    };

    Secret::new(bytes).map(Some)
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// NUL-terminated UTF-16, for the `W` entry points.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `ERROR_NOT_FOUND` — no credential under that target name. The `windows`
/// crate has already turned it into an `HRESULT`, so the comparison is against
/// `HRESULT_FROM_WIN32(ERROR_NOT_FOUND)` rather than the raw code.
fn is_not_found(error: &windows::core::Error) -> bool {
    error.code() == HRESULT::from_win32(ERROR_NOT_FOUND.0)
}

/// Overwrites a plaintext buffer with zeros.
///
/// `write_volatile` per byte, not `fill(0)`: the writes are dead stores to the
/// optimiser — nothing reads the buffer afterwards — and are exactly the kind of
/// thing LLVM removes. The fence stops the loop being sunk past the drop.
fn zero(buf: &mut [u8]) {
    for byte in buf.iter_mut() {
        // SAFETY: `byte` is a live, aligned, exclusively borrowed `u8`.
        unsafe { ptr::write_volatile(byte, 0) };
    }
    compiler_fence(Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_namespaced_by_destination_and_field() {
        assert_eq!(
            target_name("imgur", "clientId"),
            "santi.sharex/imgur/clientid"
        );
        assert_eq!(target_name("ftp", "password"), "santi.sharex/ftp/password");
    }

    /// Two destinations' `password` must never be the same credential, and a
    /// header name that differs only in case must never be two.
    #[test]
    fn keys_separate_destinations_and_fold_case() {
        assert_ne!(
            target_name("ftp", "password"),
            target_name("custom", "password")
        );
        assert_eq!(
            target_name("custom", "Authorization"),
            target_name("custom", "authorization")
        );
    }

    /// A field name arrives from an imported `.sxcu`, so it is untrusted text.
    /// Nothing in it may address another destination's slot or escape the
    /// `santi.sharex/` namespace.
    #[test]
    fn key_components_cannot_forge_another_destinations_slot() {
        let forged = target_name("custom", "../imgur/clientId");
        assert_eq!(forged, "santi.sharex/custom/.._imgur_clientid");
        assert_ne!(forged, target_name("imgur", "clientId"));
        assert!(forged.starts_with("santi.sharex/custom/"));

        // Whitespace-only and empty field names collapse to a named slot rather
        // than to `santi.sharex/custom/`, which would collide with each other.
        assert_eq!(target_name("custom", "   "), "santi.sharex/custom/unnamed");
        assert_eq!(target_name("custom", ""), target_name("custom", "\t"));
    }

    #[test]
    fn a_secret_never_prints_itself() {
        let secret = Secret::new(b"hunter2".to_vec()).expect("ascii is valid utf-8");
        assert_eq!(secret.expose(), "hunter2");
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
        assert!(!format!("{secret:?}").contains("hunter2"));
    }

    #[test]
    fn a_non_utf8_credential_is_rejected_rather_than_guessed_at() {
        assert!(Secret::new(vec![0xff, 0xfe, 0x00]).is_err());
    }

    #[test]
    fn zeroing_clears_the_buffer() {
        let mut buf = b"hunter2".to_vec();
        zero(&mut buf);
        assert!(buf.iter().all(|b| *b == 0));
    }

    /// Nothing has ever been written under this name, so a `false` here is the
    /// real answer rather than a stale one, and no credential is created.
    #[test]
    fn exists_is_false_for_a_key_that_was_never_written() {
        assert!(!exists("test-never-written", "nothing-here"));
    }

    /// Removing a credential that is not there is success, not an error — the UI
    /// calls `clear` whenever a field is emptied, including when it was already.
    #[test]
    fn clearing_an_absent_credential_succeeds() {
        assert!(clear("test-never-written", "nothing-here").is_ok());
    }

    /// Removes the credential on every exit path, including a panic unwinding
    /// out of the test: a `cargo test` run must not leave anything behind in the
    /// user's real Credential Manager.
    struct Cleanup(&'static str, &'static str);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = clear(self.0, self.1);
        }
    }

    /// The round trip, against the real store, under a target name no
    /// destination uses.
    ///
    /// `#[ignore]` because this is the one test in the crate with a side effect
    /// outside the build directory: it writes a credential into the developer's
    /// own Windows Credential Manager, under the same `santi.sharex/` prefix the
    /// real destinations use. It cleans up on every exit path (see [`Cleanup`]),
    /// but "cleans up afterwards" is not the same as "never wrote" — a hard kill
    /// between the `set` and the `clear` (a test timeout, a Ctrl-C, a panic
    /// elsewhere aborting the process) strands an entry that looks exactly like a
    /// genuine santi.sharex credential in the user's credential UI.
    ///
    /// Nothing is weakened by the attribute: every assertion below is unchanged
    /// and still runs against the real `CredWriteW`/`CredReadW`/`CredDeleteW`
    /// under `cargo test -- --ignored`, which is where the Win32 wiring belongs —
    /// it is an integration test of the OS, not a unit test of this module. The
    /// key derivation those calls are handed is covered unconditionally by the
    /// `target_name` tests above, and the absent-credential paths by the two
    /// tests above this one, neither of which writes anything.
    #[test]
    #[ignore = "writes to the real Windows Credential Manager; run with `cargo test -- --ignored`"]
    fn set_then_exists_then_clear() {
        const DESTINATION: &str = "test-roundtrip";
        const FIELD: &str = "probe";
        let _cleanup = Cleanup(DESTINATION, FIELD);

        // Start from a known state even if an earlier run died mid-test.
        clear(DESTINATION, FIELD).expect("clear must be idempotent");
        assert!(!exists(DESTINATION, FIELD));

        set(DESTINATION, FIELD, "a-value-that-is-not-a-real-secret").expect("write must succeed");
        assert!(exists(DESTINATION, FIELD));

        clear(DESTINATION, FIELD).expect("delete must succeed");
        assert!(
            !exists(DESTINATION, FIELD),
            "a cleared credential must not still read as set"
        );
    }

    /// Not `#[ignore]`d, because `set` rejects a blank value on its first line —
    /// before it builds a `CREDENTIALW` or calls `CredWriteW` — so nothing on this
    /// path reaches the credential store. That early return is the invariant being
    /// pinned: if it ever moved below the write, this test would start leaving an
    /// empty credential behind, which is precisely the state `set` exists to
    /// prevent, so the `exists` check is the assertion that would catch it.
    ///
    /// Its own field name, not the round-trip test's: under
    /// `cargo test -- --include-ignored` the two run concurrently, and sharing a
    /// target would make this `exists` check race that test's `set`.
    #[test]
    fn a_blank_value_is_not_a_secret() {
        assert!(set("test-roundtrip", "blank-probe", "").is_err());
        assert!(!exists("test-roundtrip", "blank-probe"));
    }
}
