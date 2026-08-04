//! One-time migrations from the install this app shipped as before the rename,
//! when it was called Nimbus and its bundle identifier was `com.santi.nimbus`.
//!
//! Two of the things a rename changes are keys the OS and Tauri use to find
//! state that already exists, so without these the rename would read as a
//! factory reset:
//!
//! * `app_data_dir()` is derived from the bundle identifier, so the new
//!   `com.santi.sharex` directory starts out empty while the settings, the
//!   history and every thumbnail sit in `com.santi.nimbus` next door.
//! * The launch-at-login registration is an `HKCU` `Run` value named after the
//!   app, so the old `Nimbus` value survives the rename pointing at an exe that
//!   is no longer installed.
//!
//! Both are best-effort: every failure is logged and returned from, none of it
//! is fatal, and nothing here deletes the old app data directory. Leaving it is
//! not just caution — the `path` and `thumb` fields inside the migrated
//! `history.json` are absolute and still name files under the old directory, so
//! the copies here are the *new* home while the originals stay the ones those
//! records resolve through.

use std::fs;
use std::path::Path;

use tauri::AppHandle;

use crate::store;

/// The bundle identifier before the rename. `app_data_dir()` is
/// `<data dir>/<identifier>`, so the old directory is the new one's sibling.
const LEGACY_IDENTIFIER: &str = "com.santi.nimbus";

/// The `Run` value the old install's launch-at-login registration wrote. The
/// plugin names it after the app, so the new one is a different value entirely
/// and this one would linger forever.
const LEGACY_AUTOSTART_VALUE: &str = "Nimbus";

/// The file stem of the old install's executable. A `Run` command that does not
/// point at this is not ours to touch, whatever the value is called.
const LEGACY_EXE_STEM: &str = "nimbus";

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

const SETTINGS_FILE: &str = "settings.json";
const HISTORY_FILE: &str = "history.json";
const THUMBS_DIR: &str = "thumbs";

// ---------------------------------------------------------------------------
// app data
// ---------------------------------------------------------------------------

/// Brings the old install's app data across, once, before anything reads it.
///
/// Must run before `AppState::new`, which is the first thing to open
/// `settings.json` and `history.json`: a read that lands first would see an
/// empty directory and hand the user defaults, and the first save after that
/// would write those defaults over the top.
pub(crate) fn adopt_legacy_app_data(app: &AppHandle) {
    let new_dir = store::app_data(app);
    let Some(parent) = new_dir.parent() else {
        return;
    };
    let old_dir = parent.join(LEGACY_IDENTIFIER);

    match migrate_app_data(&old_dir, &new_dir) {
        Ok(true) => eprintln!(
            "santi.sharex: brought the app data across from {}",
            old_dir.display()
        ),
        Ok(false) => {}
        // Never fatal. Startup continues on whatever the new directory holds,
        // which for a failed migration means defaults.
        Err(e) => eprintln!(
            "santi.sharex: could not bring the app data across from {}: {e}",
            old_dir.display()
        ),
    }
}

/// Whether there is anything to migrate.
///
/// `settings.json` is the marker on both sides: it is written on the first save
/// and never removed, so "the new directory has one" means this app has already
/// run under its new identifier and its state is the current one. Copying over
/// that would undo whatever the user has changed since.
fn should_migrate(old: &Path, new: &Path) -> bool {
    !new.join(SETTINGS_FILE).exists() && old.join(SETTINGS_FILE).exists()
}

/// The filesystem half, split out from [`adopt_legacy_app_data`] so it takes two
/// plain paths and can be tested without an `AppHandle`.
///
/// `Ok(true)` means files were copied, `Ok(false)` that there was nothing to do.
fn migrate_app_data(old: &Path, new: &Path) -> Result<bool, String> {
    if !should_migrate(old, new) {
        return Ok(false);
    }

    fs::create_dir_all(new).map_err(|e| format!("could not create {}: {e}", new.display()))?;

    // `settings.json` is copied *last*, because it is also the marker
    // `should_migrate` keys off: writing it is what records the migration as
    // done. Copying it first would mean a run that died half way — a disk that
    // filled, a file locked by a backup agent — left the marker in place with
    // the history still sitting in the old directory, and every later launch
    // would skip the migration and call that the finished state. Written last,
    // it is a commit point: until it lands the whole thing is retried on the
    // next launch, and `copy_file` leaves existing destinations alone so the
    // retry resumes rather than duplicating work.

    // Optional — a user who has never taken a capture has neither — so their
    // absence is not a failure, but a *failed* copy is, so the retry above can
    // pick it up.
    let old_history = old.join(HISTORY_FILE);
    if old_history.is_file() {
        copy_file(&old_history, &new.join(HISTORY_FILE))?;
    }

    let old_thumbs = old.join(THUMBS_DIR);
    if old_thumbs.is_dir() {
        // The one exception to the retry rule. The migrated records' `thumb`
        // fields are absolute and still resolve into the old directory, which
        // is never deleted, so these copies are a spare rather than the live
        // path and a thumbnail that will not copy costs nothing visible.
        // Holding the settings and the history hostage to it would be the far
        // worse trade.
        if let Err(e) = copy_dir(&old_thumbs, &new.join(THUMBS_DIR)) {
            eprintln!("santi.sharex: could not bring every thumbnail across: {e}");
        }
    }

    // The marker, and the only required file.
    copy_file(&old.join(SETTINGS_FILE), &new.join(SETTINGS_FILE))?;

    Ok(true)
}

/// Copies one file, leaving an existing destination alone. Nothing here may
/// overwrite state the new install has already written.
fn copy_file(from: &Path, to: &Path) -> Result<(), String> {
    if to.exists() {
        return Ok(());
    }
    fs::copy(from, to)
        .map(|_| ())
        .map_err(|e| format!("could not copy {} to {}: {e}", from.display(), to.display()))
}

/// Recursive so it cannot be surprised by a subdirectory, though `thumbs/` is
/// flat `{id}.png` files in practice.
fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|e| format!("could not create {}: {e}", to.display()))?;

    let entries =
        fs::read_dir(from).map_err(|e| format!("could not read {}: {e}", from.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("could not read {}: {e}", from.display()))?;
        let source = entry.path();
        let target = to.join(entry.file_name());
        if source.is_dir() {
            copy_dir(&source, &target)?;
        } else {
            copy_file(&source, &target)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// autostart
// ---------------------------------------------------------------------------

/// Drops the `Run` value the old install left behind, if it is still there and
/// still points at the old executable.
///
/// Runs after `sync_autostart`, so the new registration is already in place and
/// this only ever removes the dead one. A value that does not name the old exe
/// belongs to something else and is left exactly as it is.
pub(crate) fn remove_legacy_autostart() {
    let current_exe = std::env::current_exe().ok();
    match prune_legacy_run_value(current_exe.as_deref()) {
        Ok(true) => eprintln!(
            "santi.sharex: removed the stale \"{LEGACY_AUTOSTART_VALUE}\" launch-at-login entry"
        ),
        Ok(false) => {}
        // Best-effort like the app data migration: the worst case is one dead
        // Run entry, which Windows silently ignores.
        Err(e) => eprintln!(
            "santi.sharex: could not check for a stale \"{LEGACY_AUTOSTART_VALUE}\" launch-at-login entry: {e}"
        ),
    }
}

/// Whether a `Run` command line is the old install's.
///
/// Pure string work, kept apart from the registry calls so the decision — the
/// part that must never delete someone else's value — is testable.
fn is_legacy_autostart_command(command: &str, current_exe: Option<&Path>) -> bool {
    let Some(exe) = command_executable(command) else {
        return false;
    };
    let exe = Path::new(exe);

    let old_exe = exe
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case(LEGACY_EXE_STEM));
    if !old_exe {
        return false;
    }

    // Belt and braces: if the value somehow points at the executable running
    // right now, deleting it would turn launch-at-login off behind the user's
    // back rather than tidying up after the rename.
    match current_exe {
        Some(current) => !same_path(exe, current),
        None => true,
    }
}

/// The executable out of a `Run` command line. The autostart plugin writes the
/// path quoted with its arguments after it; an unquoted one is handled too, on
/// the assumption that arguments start at the first ` -`.
fn command_executable(command: &str) -> Option<&str> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    if let Some(rest) = command.strip_prefix('"') {
        let (exe, _) = rest.split_once('"')?;
        return (!exe.is_empty()).then_some(exe);
    }
    let exe = command
        .split_once(" -")
        .map(|(exe, _)| exe)
        .unwrap_or(command)
        .trim_end();
    (!exe.is_empty()).then_some(exe)
}

/// Windows paths are case-insensitive, so a byte comparison would call the same
/// file two different files.
fn same_path(a: &Path, b: &Path) -> bool {
    a.as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
}

/// The registry half. `Ok(true)` means a value was deleted.
fn prune_legacy_run_value(current_exe: Option<&Path>) -> Result<bool, String> {
    use windows::core::HSTRING;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER,
        KEY_QUERY_VALUE, KEY_SET_VALUE, REG_EXPAND_SZ, REG_SZ, REG_VALUE_TYPE,
    };

    let subkey = HSTRING::from(RUN_KEY);
    let value = HSTRING::from(LEGACY_AUTOSTART_VALUE);

    let mut key = HKEY::default();
    let status = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            &subkey,
            None,
            KEY_QUERY_VALUE | KEY_SET_VALUE,
            &mut key,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(format!("could not open HKCU\\{RUN_KEY}: error {}", status.0));
    }

    // Everything from here goes through this closure so the key is closed on
    // every path, including the early returns.
    let outcome = (|| -> Result<bool, String> {
        let mut kind = REG_VALUE_TYPE::default();
        let mut len: u32 = 0;
        let status = unsafe {
            RegQueryValueExW(key, &value, None, Some(&mut kind), None, Some(&mut len))
        };
        if status == ERROR_FILE_NOT_FOUND {
            // The usual case on a fresh install, and on every launch after the
            // first one that pruned it.
            return Ok(false);
        }
        if status != ERROR_SUCCESS {
            return Err(format!(
                "could not read the \"{LEGACY_AUTOSTART_VALUE}\" value: error {}",
                status.0
            ));
        }
        if kind != REG_SZ && kind != REG_EXPAND_SZ {
            // Not a command line, so not the registration this is looking for.
            return Ok(false);
        }

        let mut buffer = vec![0u8; len as usize];
        let mut read = len;
        let status = unsafe {
            RegQueryValueExW(
                key,
                &value,
                None,
                None,
                Some(buffer.as_mut_ptr()),
                Some(&mut read),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(format!(
                "could not read the \"{LEGACY_AUTOSTART_VALUE}\" value: error {}",
                status.0
            ));
        }
        buffer.truncate(read as usize);

        let wide: Vec<u16> = buffer
            .chunks_exact(2)
            .map(|pair| u16::from_ne_bytes([pair[0], pair[1]]))
            .collect();
        let command = String::from_utf16_lossy(&wide);
        let command = command.trim_end_matches('\0');

        if !is_legacy_autostart_command(command, current_exe) {
            return Ok(false);
        }

        let status = unsafe { RegDeleteValueW(key, &value) };
        if status != ERROR_SUCCESS {
            return Err(format!(
                "could not delete the \"{LEGACY_AUTOSTART_VALUE}\" value: error {}",
                status.0
            ));
        }
        Ok(true)
    })();

    unsafe {
        let _ = RegCloseKey(key);
    }
    outcome
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A scratch directory of our own, removed when the test drops it.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!("santi-sharex-migrate-{tag}-{nanos}-{n}"));
            fs::create_dir_all(&root).expect("scratch dir");
            Self(root)
        }

        fn dir(&self, name: &str) -> PathBuf {
            let dir = self.0.join(name);
            fs::create_dir_all(&dir).expect("scratch subdir");
            dir
        }

        /// A path under the scratch root that has deliberately not been created.
        fn absent(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent");
        }
        fs::write(path, contents).expect("write");
    }

    #[test]
    fn migrates_when_the_new_dir_has_no_settings_and_the_old_one_does() {
        let scratch = Scratch::new("adopt");
        let old = scratch.dir("com.santi.nimbus");
        let new = scratch.absent("com.santi.sharex");

        write(&old.join(SETTINGS_FILE), "{\"theme\":\"claude-dark\"}");
        write(&old.join(HISTORY_FILE), "[]");
        write(&old.join(THUMBS_DIR).join("1-0.png"), "thumb");

        assert!(should_migrate(&old, &new));
        assert_eq!(migrate_app_data(&old, &new), Ok(true));

        assert_eq!(
            fs::read_to_string(new.join(SETTINGS_FILE)).unwrap(),
            "{\"theme\":\"claude-dark\"}"
        );
        assert_eq!(fs::read_to_string(new.join(HISTORY_FILE)).unwrap(), "[]");
        assert_eq!(
            fs::read_to_string(new.join(THUMBS_DIR).join("1-0.png")).unwrap(),
            "thumb"
        );
        // Never destructive.
        assert!(old.join(SETTINGS_FILE).exists());
    }

    #[test]
    fn skips_when_the_new_dir_already_has_settings() {
        let scratch = Scratch::new("already");
        let old = scratch.dir("com.santi.nimbus");
        let new = scratch.dir("com.santi.sharex");

        write(&old.join(SETTINGS_FILE), "old");
        write(&new.join(SETTINGS_FILE), "new");

        assert!(!should_migrate(&old, &new));
        assert_eq!(migrate_app_data(&old, &new), Ok(false));
        // The live settings are untouched.
        assert_eq!(fs::read_to_string(new.join(SETTINGS_FILE)).unwrap(), "new");
    }

    #[test]
    fn skips_when_neither_dir_has_settings() {
        let scratch = Scratch::new("fresh");
        let old = scratch.absent("com.santi.nimbus");
        let new = scratch.dir("com.santi.sharex");

        assert!(!should_migrate(&old, &new));
        assert_eq!(migrate_app_data(&old, &new), Ok(false));
        assert!(!new.join(SETTINGS_FILE).exists());
    }

    #[test]
    fn migrates_without_a_history_or_thumbs_dir() {
        let scratch = Scratch::new("settings-only");
        let old = scratch.dir("com.santi.nimbus");
        let new = scratch.absent("com.santi.sharex");

        write(&old.join(SETTINGS_FILE), "{}");

        assert_eq!(migrate_app_data(&old, &new), Ok(true));
        assert!(new.join(SETTINGS_FILE).exists());
        assert!(!new.join(HISTORY_FILE).exists());
        assert!(!new.join(THUMBS_DIR).exists());
    }

    /// `settings.json` is written last, so a run that died part-way left the
    /// marker absent and this one has to finish the job — without overwriting
    /// what the dead run already brought across.
    #[test]
    fn resumes_a_migration_that_did_not_reach_the_marker() {
        let scratch = Scratch::new("resume");
        let old = scratch.dir("com.santi.nimbus");
        let new = scratch.dir("com.santi.sharex");

        write(&old.join(SETTINGS_FILE), "{\"theme\":\"claude-dark\"}");
        write(&old.join(HISTORY_FILE), "[{\"id\":\"1-0\"}]");
        // What the interrupted run had already copied.
        write(&new.join(HISTORY_FILE), "[{\"id\":\"1-0\"}]");

        assert!(should_migrate(&old, &new));
        assert_eq!(migrate_app_data(&old, &new), Ok(true));
        assert_eq!(
            fs::read_to_string(new.join(SETTINGS_FILE)).unwrap(),
            "{\"theme\":\"claude-dark\"}"
        );
        assert_eq!(
            fs::read_to_string(new.join(HISTORY_FILE)).unwrap(),
            "[{\"id\":\"1-0\"}]"
        );
        // And now that the marker is there, it is done for good.
        assert!(!should_migrate(&old, &new));
    }

    #[test]
    fn claims_only_run_values_pointing_at_the_old_exe() {
        let current = Path::new(r"C:\Users\santi\AppData\Local\santi.sharex\santi.sharex.exe");

        assert!(is_legacy_autostart_command(
            "\"C:\\Users\\santi\\AppData\\Local\\Nimbus\\Nimbus.exe\" --autostarted",
            Some(current)
        ));
        // Unquoted with arguments, which is the shape the old install actually
        // left in the registry.
        assert!(is_legacy_autostart_command(
            r"C:\Users\santi\AppData\Local\Nimbus\nimbus.exe --autostarted",
            Some(current)
        ));
        // Unquoted, and with no arguments.
        assert!(is_legacy_autostart_command(
            r"C:\Program Files\Nimbus\nimbus.exe",
            Some(current)
        ));
        // Somebody else's entry that happens to sit under the same value name.
        assert!(!is_legacy_autostart_command(
            "\"C:\\Program Files\\OtherApp\\OtherApp.exe\" --run",
            Some(current)
        ));
        // The renamed app itself.
        assert!(!is_legacy_autostart_command(
            "\"C:\\Users\\santi\\AppData\\Local\\santi.sharex\\santi.sharex.exe\" --autostarted",
            Some(current)
        ));
        assert!(!is_legacy_autostart_command("", Some(current)));
    }

    #[test]
    fn never_claims_the_running_executable() {
        let current = Path::new(r"C:\Nimbus\Nimbus.exe");
        // Same path, different case: still us, so still off limits.
        assert!(!is_legacy_autostart_command(
            "\"c:\\nimbus\\nimbus.exe\" --autostarted",
            Some(current)
        ));
    }
}
