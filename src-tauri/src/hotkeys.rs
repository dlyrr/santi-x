//! The low-level keyboard hook fallback for hotkeys `RegisterHotKey` refuses.
//!
//! `tauri-plugin-global-shortcut` registers through `RegisterHotKey`, which the
//! OS hands to exactly one process. Combos another process already owns —
//! `Win+Shift+S` (the shell's snipping shortcut), `PrintScreen` and
//! `Alt+PrintScreen` when ShareX is running — simply fail to register. A
//! `WH_KEYBOARD_LL` hook sees keys *before* hotkey dispatch, so it can claim
//! such a combo and suppress the original by returning 1.
//!
//! # Scope — this is keylogger-shaped machinery, built so it isn't one
//!
//! ARCHITECTURE-M2.6 §1 makes these requirements, not style notes:
//!
//! * **Nothing is logged, stored, buffered, counted or sent anywhere.** There is
//!   no key history in this file: no `Vec<Key>`, no channel carrying a key code,
//!   no event payload naming a key. [`BINDINGS`] holds the user's *own*
//!   configured combos and nothing else, and it is written only by
//!   [`install`]/[`uninstall`].
//! * A key that is not one of those combos leaves [`keyboard_proc`] through
//!   `CallNextHookEx` on the first branch, having been compared and then
//!   forgotten.
//! * The only live state is the modifier flags needed to match a combo, and even
//!   those are not tracked — they are read on demand from `GetAsyncKeyState`
//!   *after* a candidate virtual-key matched.
//! * The hook procedure does no capture work. It hands the action to
//!   [`crate::dispatch`], which moves to a worker thread, and returns. Blocking
//!   inside a `WH_KEYBOARD_LL` procedure stalls input system-wide.
//! * The hook lives on its own thread with its own message loop, is installed at
//!   startup and is torn down on shutdown and before every rebind.
//! * A panic inside the hook procedure cannot escape it. Unwinding out of an
//!   `extern "system"` function aborts the process, so [`keyboard_proc`] catches
//!   instead and passes the key on — the worst case is one keystroke that is not
//!   claimed, never a wedged input path.
//!
//! # Threading
//!
//! `HHOOK` is not `Send`, and Windows requires `UnhookWindowsHookEx` to run on
//! the thread that installed the hook. Both fall out for free here: the handle
//! is a local inside [`hook_thread`] and never leaves it. What crosses threads
//! is the thread *id* (a `u32`), which is all [`uninstall`] needs to post
//! `WM_QUIT` at the message loop.

use std::sync::{mpsc, Mutex, MutexGuard, OnceLock, RwLock};
use std::thread::JoinHandle;

use serde::Serialize;
use tauri::AppHandle;

use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, PeekMessageW, PostThreadMessageW,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, HC_ACTION, KBDLLHOOKSTRUCT, MSG,
    PM_NOREMOVE, WH_KEYBOARD_LL, WM_KEYDOWN, WM_QUIT, WM_SYSKEYDOWN, WM_USER,
};

use crate::Action;

// ---------------------------------------------------------------------------
// combos
// ---------------------------------------------------------------------------

const MOD_CTRL: u8 = 1 << 0;
const MOD_SHIFT: u8 = 1 << 1;
const MOD_ALT: u8 = 1 << 2;
const MOD_WIN: u8 = 1 << 3;

// Virtual-key codes used for the modifier probe. `VK_SHIFT`/`VK_CONTROL`/
// `VK_MENU` are the side-agnostic forms, which is what an accelerator means.
const VK_SHIFT: i32 = 0x10;
const VK_CONTROL: i32 = 0x11;
const VK_MENU: i32 = 0x12;
const VK_LWIN: i32 = 0x5B;
const VK_RWIN: i32 = 0x5C;

/// One parsed accelerator: a modifier mask plus the single non-modifier key.
///
/// Matching is exact on both halves — `Ctrl+Shift+1` must not fire for
/// `Ctrl+Alt+Shift+1`, or a hook-bound hotkey would eat combos it was never
/// given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Combo {
    mods: u8,
    vk: u16,
}

/// Parses a Tauri accelerator (`"CmdOrCtrl+Shift+1"`, `"Alt+PrintScreen"`) into
/// a [`Combo`]. Returns `None` for anything this hook cannot match — an unknown
/// key name, a bare modifier, or an empty string — so the caller can report the
/// hotkey as unbound instead of silently dropping it.
///
/// The key names accepted are exactly the ones `SettingsView`'s recorder emits
/// (`KeyA` → `A`, `Digit1` → `1`, `F5`, `Numpad3`, `PrintScreen`, `ArrowUp`,
/// `Comma`, …), matched case-insensitively.
pub fn parse(accelerator: &str) -> Option<Combo> {
    let mut mods = 0u8;
    let mut vk: Option<u16> = None;

    for token in accelerator.split('+') {
        let token = token.trim();
        if token.is_empty() {
            return None;
        }
        match token.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "cmd" | "command" | "cmdorctrl" | "commandorcontrol" => {
                mods |= MOD_CTRL
            }
            "shift" => mods |= MOD_SHIFT,
            "alt" | "option" => mods |= MOD_ALT,
            "super" | "meta" | "win" | "windows" => mods |= MOD_WIN,
            key => {
                // A second non-modifier token means the string is malformed.
                if vk.is_some() {
                    return None;
                }
                vk = Some(key_to_vk(key)?);
            }
        }
    }

    vk.map(|vk| Combo { mods, vk })
}

/// `name` is already lowercase.
fn key_to_vk(name: &str) -> Option<u16> {
    if name.len() == 1 {
        let c = name.as_bytes()[0];
        // 'A'..='Z' and '0'..='9' are their own virtual-key codes.
        if c.is_ascii_alphabetic() {
            return Some(c.to_ascii_uppercase() as u16);
        }
        if c.is_ascii_digit() {
            return Some(c as u16);
        }
    }

    if let Some(n) = name.strip_prefix('f').and_then(|r| r.parse::<u16>().ok()) {
        if (1..=24).contains(&n) {
            return Some(0x6F + n); // VK_F1 = 0x70
        }
    }

    if let Some(d) = name.strip_prefix("numpad").and_then(|r| r.parse::<u16>().ok()) {
        if d <= 9 {
            return Some(0x60 + d); // VK_NUMPAD0 = 0x60
        }
    }

    Some(match name {
        "printscreen" | "print" | "prtsc" | "snapshot" => 0x2C,
        "space" => 0x20,
        "enter" | "return" => 0x0D,
        "escape" | "esc" => 0x1B,
        "tab" => 0x09,
        "backspace" => 0x08,
        "delete" | "del" => 0x2E,
        "insert" | "ins" => 0x2D,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" => 0x21,
        "pagedown" => 0x22,
        "arrowleft" | "left" => 0x25,
        "arrowup" | "up" => 0x26,
        "arrowright" | "right" => 0x27,
        "arrowdown" | "down" => 0x28,
        "capslock" => 0x14,
        "numlock" => 0x90,
        "scrolllock" => 0x91,
        "pause" => 0x13,
        "semicolon" => 0xBA,
        "equal" => 0xBB,
        "comma" => 0xBC,
        "minus" => 0xBD,
        "period" => 0xBE,
        "slash" => 0xBF,
        "backquote" | "grave" => 0xC0,
        "bracketleft" => 0xDB,
        "backslash" => 0xDC,
        "bracketright" => 0xDD,
        "quote" => 0xDE,
        _ => return None,
    })
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

/// Which mechanism ended up owning a hotkey (M2.6 §1, "Report per-hotkey which
/// mechanism won, so Settings can show it"). Emitted on `hotkeys://status` and
/// readable through the `get_hotkey_status` command.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyStatus {
    /// `"region" | "fullscreen" | "activeWindow"` — the `Hotkeys` field name.
    pub action: String,
    pub accelerator: String,
    /// `"plugin"` (RegisterHotKey), `"hook"` (WH_KEYBOARD_LL), or `"none"`.
    pub mechanism: String,
    pub bound: bool,
    /// Why it is unbound, when it is.
    pub error: Option<String>,
}

pub const MECHANISM_PLUGIN: &str = "plugin";
pub const MECHANISM_HOOK: &str = "hook";
pub const MECHANISM_NONE: &str = "none";

static STATUS: Mutex<Vec<HotkeyStatus>> = Mutex::new(Vec::new());

pub(crate) fn set_status(statuses: Vec<HotkeyStatus>) {
    *crate::lock(&STATUS) = statuses;
}

pub fn status() -> Vec<HotkeyStatus> {
    crate::lock(&STATUS).clone()
}

// ---------------------------------------------------------------------------
// hook state
// ---------------------------------------------------------------------------

/// The combos the hook may match. Written only while no hook is installed, so
/// the read side never contends in practice — and when it somehow does,
/// [`matched`] gives up rather than block (see below).
static BINDINGS: RwLock<Vec<(Combo, Action)>> = RwLock::new(Vec::new());

/// Set once, on the first successful install. `AppHandle` is `Send + Sync` and
/// cheap to clone; the hook procedure has no user-data parameter, so this is the
/// only way to reach the app from it.
static APP: OnceLock<AppHandle> = OnceLock::new();

struct HookThread {
    /// Win32 thread id — the target of the `WM_QUIT` that ends the message loop.
    id: u32,
    join: JoinHandle<()>,
}

static HOOK_THREAD: Mutex<Option<HookThread>> = Mutex::new(None);

fn hook_thread() -> MutexGuard<'static, Option<HookThread>> {
    crate::lock(&HOOK_THREAD)
}

// ---------------------------------------------------------------------------
// install / uninstall
// ---------------------------------------------------------------------------

/// Replaces whatever is installed with `bindings`. An empty list uninstalls.
///
/// Blocks until the hook thread reports success or failure, so a caller that
/// gets `Ok(())` knows the hook is live.
pub fn install(app: &AppHandle, bindings: Vec<(Combo, Action)>) -> Result<(), String> {
    uninstall();
    if bindings.is_empty() {
        return Ok(());
    }

    let _ = APP.set(app.clone());
    *crate::write_lock(&BINDINGS) = bindings;

    let (ready_tx, ready_rx) = mpsc::channel::<Result<u32, String>>();
    let join = std::thread::Builder::new()
        .name("santi-sharex-hotkey-hook".into())
        .spawn(move || hook_thread_main(ready_tx))
        .map_err(|e| {
            crate::write_lock(&BINDINGS).clear();
            format!("could not start the hotkey hook thread: {e}")
        })?;

    match ready_rx.recv() {
        Ok(Ok(id)) => {
            *hook_thread() = Some(HookThread { id, join });
            Ok(())
        }
        // The thread returns immediately on failure, so joining cannot hang.
        Ok(Err(e)) => {
            let _ = join.join();
            crate::write_lock(&BINDINGS).clear();
            Err(e)
        }
        Err(_) => {
            let _ = join.join();
            crate::write_lock(&BINDINGS).clear();
            Err("the hotkey hook thread stopped before it installed the hook".into())
        }
    }
}

/// Tears the hook down and forgets every combo. Idempotent — it is the first
/// thing [`install`] does, and it runs again on `RunEvent::Exit`.
pub fn uninstall() {
    // Taken out of the mutex before joining: the guard must not be held across
    // a thread join.
    let thread = hook_thread().take();
    if let Some(HookThread { id, join }) = thread {
        // `WM_QUIT` drops `GetMessageW` out of its loop; the thread then calls
        // `UnhookWindowsHookEx` itself, which is the only thread allowed to.
        unsafe {
            let _ = PostThreadMessageW(id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        let _ = join.join();
    }
    crate::write_lock(&BINDINGS).clear();
}

/// Owns the `HHOOK` for its whole lifetime, which is what keeps the non-`Send`
/// handle on one thread.
fn hook_thread_main(ready: mpsc::Sender<Result<u32, String>>) {
    unsafe {
        let mut msg = MSG::default();
        // Force the message queue into existence *before* the thread id is
        // published. A `PostThreadMessageW` aimed at a thread that has never
        // pumped is dropped on the floor, and the loop below would then never
        // exit.
        let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);

        // `hmod` is `None` and `dwthreadid` 0: a global low-level hook lives in
        // the installing process, so there is no DLL to name.
        let hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), None, 0) {
            Ok(hook) => hook,
            Err(e) => {
                let _ = ready.send(Err(format!(
                    "the low-level keyboard hook could not be installed ({e})"
                )));
                return;
            }
        };

        if ready.send(Ok(GetCurrentThreadId())).is_err() {
            let _ = UnhookWindowsHookEx(hook);
            return;
        }

        // A `WH_KEYBOARD_LL` hook is only called while its installing thread
        // pumps messages, which is the entire reason this thread exists.
        loop {
            let result = GetMessageW(&mut msg, None, 0, 0);
            // 0 is WM_QUIT, -1 is an error; both mean stop.
            if result.0 <= 0 {
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        let _ = UnhookWindowsHookEx(hook);
    }
}

// ---------------------------------------------------------------------------
// the hook procedure
// ---------------------------------------------------------------------------

/// Runs on every key event in every process, so it must be trivial and must
/// never block. It compares one virtual-key code against the configured combos
/// and forgets it; nothing here records, accumulates or transmits a keystroke.
unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN) {
        // Read-only, and only for the duration of the comparison. Copied out as a
        // plain `u16` immediately: nothing below this line can reach the event.
        let vk = (*(lparam.0 as *const KBDLLHOOKSTRUCT)).vkCode as u16;

        // A panic must not cross this boundary. Unwinding out of an
        // `extern "system"` function aborts the process, and a process that dies
        // inside its own hook procedure is one bad key event away from a
        // half-installed hook on a system-wide input path. Catching here means
        // the worst case is one keypress that falls through untouched: there is
        // no retained state to leave inconsistent, so `CallNextHookEx` is always
        // the safe answer.
        let claimed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let Some(action) = matched(vk) else {
                return false;
            };
            // Hand off and get out — `dispatch` moves the capture to a worker
            // thread.
            if let Some(app) = APP.get() {
                crate::dispatch(app, action);
            }
            true
        }))
        .unwrap_or(false);

        // Returning 1 suppresses the key, which is what claims the combo from
        // whoever `RegisterHotKey`'d it. Only ever for a combo the user
        // configured — a panic reports "not ours" and the key passes through.
        if claimed {
            return LRESULT(1);
        }
    }

    // Everything else — which is every key the user did not bind — leaves here,
    // unexamined beyond the comparison above and unrecorded.
    CallNextHookEx(None, code, wparam, lparam)
}

/// `None` for any key that is not one of the configured combos.
fn matched(vk: u16) -> Option<Action> {
    // `try_read`, never `read`: blocking here would stall the whole input
    // system behind a rebind. A key that arrives mid-rebind just passes through.
    let bindings = BINDINGS.try_read().ok()?;
    if bindings.is_empty() {
        return None;
    }
    // The modifier probe is deliberately *after* the virtual-key test, so the
    // common case is one integer compare per binding and no syscall at all.
    let candidate = bindings.iter().any(|(combo, _)| combo.vk == vk);
    if !candidate {
        return None;
    }
    let mods = live_modifiers();
    bindings
        .iter()
        .find(|(combo, _)| combo.vk == vk && combo.mods == mods)
        .map(|(_, action)| *action)
}

/// The modifiers held *right now*, read from the OS rather than tracked, so
/// there is no key state to keep between events.
fn live_modifiers() -> u8 {
    fn down(vk: i32) -> bool {
        // The high bit is "currently down"; the low bit is "pressed since the
        // last call" and must be masked off.
        (unsafe { GetAsyncKeyState(vk) } as u16 & 0x8000) != 0
    }

    let mut mods = 0u8;
    if down(VK_CONTROL) {
        mods |= MOD_CTRL;
    }
    if down(VK_SHIFT) {
        mods |= MOD_SHIFT;
    }
    if down(VK_MENU) {
        mods |= MOD_ALT;
    }
    if down(VK_LWIN) || down(VK_RWIN) {
        mods |= MOD_WIN;
    }
    mods
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_shipped_defaults() {
        assert_eq!(
            parse("Ctrl+Shift+1"),
            Some(Combo {
                mods: MOD_CTRL | MOD_SHIFT,
                vk: b'1' as u16
            })
        );
        // The recorder in SettingsView writes Ctrl as CmdOrCtrl.
        assert_eq!(parse("CmdOrCtrl+Shift+2"), parse("Ctrl+Shift+2"));
    }

    #[test]
    fn parses_the_combos_registerhotkey_cannot_take() {
        assert_eq!(
            parse("Super+Shift+S"),
            Some(Combo {
                mods: MOD_WIN | MOD_SHIFT,
                vk: b'S' as u16
            })
        );
        assert_eq!(parse("PrintScreen"), Some(Combo { mods: 0, vk: 0x2C }));
        assert_eq!(
            parse("Alt+PrintScreen"),
            Some(Combo {
                mods: MOD_ALT,
                vk: 0x2C
            })
        );
    }

    #[test]
    fn rejects_what_it_cannot_match() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("Ctrl"), None);
        assert_eq!(parse("Ctrl+"), None);
        assert_eq!(parse("Ctrl+A+B"), None);
        assert_eq!(parse("Ctrl+Nonsense"), None);
        assert_eq!(parse("F25"), None);
    }

    #[test]
    fn parses_the_named_keys_the_recorder_emits() {
        assert_eq!(parse("F5").map(|c| c.vk), Some(0x74));
        assert_eq!(parse("Numpad3").map(|c| c.vk), Some(0x63));
        assert_eq!(parse("ArrowUp").map(|c| c.vk), Some(0x26));
        assert_eq!(parse("Comma").map(|c| c.vk), Some(0xBC));
    }
}
