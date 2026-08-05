//! Finding ffmpeg — and never fetching one (M4 §1).
//!
//! ShareX downloads an ~80 MB binary on first use and runs it. santi.sharex does
//! not: that is a fetch-and-execute of an unsigned third-party build, which sits
//! badly beside M3 §1's rule that nothing leaves — or arrives on — this machine
//! without being asked for. So ffmpeg is *resolved*, in the documented order,
//! and when nothing resolves the failure is structured data: which binary or
//! encoder is missing, which locations were tried, and the one command that
//! fixes it. The UI renders those fields; it is never handed a paragraph to
//! print.
//!
//! Resolution order, first hit wins (M4 §1):
//!
//! 1. `Settings.ffmpeg_path`, when the user set one explicitly
//! 2. `ffmpeg` on `PATH`
//! 3. `%USERPROFILE%\scoop\shims\ffmpeg.exe`
//! 4. `%LOCALAPPDATA%\Microsoft\WinGet\Links\ffmpeg.exe`
//! 5. `%ProgramFiles%\ffmpeg\bin\ffmpeg.exe`
//!
//! A candidate is only accepted once `ffmpeg -version` has actually run, and the
//! encoder list is read in the same breath — a job that needs `libx264` finds
//! out here, not three minutes into a recording that then produces nothing.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;

use serde::Serialize;

/// `CREATE_NO_WINDOW`. Every ffmpeg santi.sharex spawns is a console program, and
/// without this each one flashes a console window over the desktop — which, for
/// a screen recorder, means into the recording.
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Where a resolved binary came from. Mirrored by the UI, which shows it beside
/// the path so "why is it using *that* ffmpeg" has an answer.
pub const SOURCE_SETTING: &str = "setting";
pub const SOURCE_PATH: &str = "path";
pub const SOURCE_SCOOP: &str = "scoop";
pub const SOURCE_WINGET: &str = "winget";
pub const SOURCE_PROGRAM_FILES: &str = "programFiles";

/// The encoders M4 asks for by name.
pub const ENCODER_X264: &str = "libx264";
pub const ENCODER_GIF: &str = "gif";
pub const ENCODER_NVENC: &str = "h264_nvenc";

/// Hardware H.264 encoders worth reporting to the UI. `h264_nvenc` is the only
/// one M4 §3 wires up; the other two are listed so the Settings page can say
/// what this build of ffmpeg could do rather than implying NVIDIA is the only
/// option that exists.
pub const HW_ENCODERS: [&str; 3] = [ENCODER_NVENC, "h264_amf", "h264_qsv"];

/// How long a probe may take before it is treated as a hung binary. Generous:
/// `-encoders` on a cold cache off a spinning disk is not instant.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);

// ---------------------------------------------------------------------------
// the resolved binary
// ---------------------------------------------------------------------------

/// A binary that has answered `-version` and listed its encoders.
#[derive(Debug, Clone)]
pub struct Ffmpeg {
    pub path: PathBuf,
    /// First line of `-version`, trimmed of the copyright tail.
    pub version: String,
    pub source: &'static str,
    /// Every encoder name `-encoders` reported. Kept whole rather than filtered
    /// down to the three we use, so a future format costs a lookup and not
    /// another probe.
    pub encoders: Vec<String>,
}

impl Ffmpeg {
    pub fn has(&self, encoder: &str) -> bool {
        self.encoders.iter().any(|e| e == encoder)
    }

    /// A `Command` for this binary with the two flags every invocation wants.
    /// Deliberately *not* `-nostdin`: the recording pipe feeds frames in on
    /// stdin, and that flag would close the one thing the pipeline depends on.
    pub fn command(&self) -> Command {
        let mut cmd = Command::new(&self.path);
        cmd.arg("-hide_banner");
        no_window(&mut cmd);
        cmd
    }
}

/// Keeps a console window from appearing. Called on every spawn in M4, not just
/// the ones here.
pub fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
}

// ---------------------------------------------------------------------------
// the structured "unavailable" state
// ---------------------------------------------------------------------------

/// One thing that is not there. `id` is what the UI switches on; `label` is the
/// noun phrase it can drop into its own sentence.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Missing {
    pub id: String,
    pub label: String,
}

/// A fix the user can run, verbatim. `command` is empty for [`REMEDY_BROWSE`],
/// which is a control in the UI rather than something to paste.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Remedy {
    pub id: String,
    pub label: String,
    pub command: String,
}

pub const REMEDY_SCOOP: &str = "scoop";
pub const REMEDY_WINGET: &str = "winget";
pub const REMEDY_BROWSE: &str = "browse";

/// A location that was tried, and what came of it. Shown so a user whose ffmpeg
/// lives somewhere unusual can see that santi.sharex looked and where.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Attempt {
    pub source: String,
    pub path: String,
    /// `"ok"` | `"notFound"` | `"unusable"`
    pub outcome: String,
    pub detail: Option<String>,
}

pub const OUTCOME_OK: &str = "ok";
pub const OUTCOME_NOT_FOUND: &str = "notFound";
pub const OUTCOME_UNUSABLE: &str = "unusable";

/// Everything the Tools page and the recorder need to decide whether recording
/// is offered, and what to say when it is not.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Availability {
    /// `true` only when a binary answered *and* every encoder M4 needs is in it.
    pub available: bool,
    pub path: String,
    pub version: String,
    pub source: String,
    /// Which of [`ENCODER_X264`], [`ENCODER_GIF`], [`ENCODER_NVENC`] this binary
    /// has. Not the whole list — that is thousands of characters of nothing the
    /// UI can use.
    pub encoders: Vec<String>,
    pub hardware_encoders: Vec<String>,
    /// Empty exactly when [`Self::available`] is true.
    pub missing: Vec<Missing>,
    pub remedies: Vec<Remedy>,
    pub searched: Vec<Attempt>,
}

impl Availability {
    /// The one-sentence form, for the places that can only carry a string — a
    /// command's `Err`, which lands in the existing `capture://error` toast.
    /// The structured fields above stay the source of truth for the UI.
    pub fn sentence(&self) -> String {
        if self.available {
            return format!("ffmpeg {} at {}", self.version, self.path);
        }
        let what = self
            .missing
            .iter()
            .map(|m| m.label.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let fix = self
            .remedies
            .iter()
            .filter(|r| !r.command.is_empty())
            .map(|r| r.command.as_str())
            .collect::<Vec<_>>()
            .join("  or  ");
        if fix.is_empty() {
            format!("Recording needs {what}.")
        } else {
            format!("Recording needs {what} — {fix} fixes it.")
        }
    }
}

// ---------------------------------------------------------------------------
// resolution
// ---------------------------------------------------------------------------

/// One session's worth of cache, keyed by the `Settings.ffmpeg_path` it was
/// resolved under so changing that setting re-resolves rather than being
/// ignored. Only *successes* are cached: a failure is worth re-checking, because
/// the user's next move after reading the message is usually to run the install
/// command it just gave them, and they should not have to restart the app for it
/// to be noticed.
static CACHE: Mutex<Option<(String, Ffmpeg)>> = Mutex::new(None);

fn cached(key: &str) -> Option<Ffmpeg> {
    let guard = crate::lock(&CACHE);
    guard
        .as_ref()
        .filter(|(k, _)| k == key)
        .map(|(_, f)| f.clone())
}

fn remember(key: &str, ffmpeg: &Ffmpeg) {
    *crate::lock(&CACHE) = Some((key.to_string(), ffmpeg.clone()));
}

/// Forgets the cached binary. Called when the setting changes, so a user who
/// browses to a different build is not still running the old one.
pub fn forget() {
    *crate::lock(&CACHE) = None;
}

struct Candidate {
    source: &'static str,
    path: PathBuf,
}

/// The search list, in M4 §1's order. Missing environment variables simply drop
/// their candidate — a machine without `%LOCALAPPDATA%` is not a machine with a
/// WinGet install.
fn candidates(setting: &str) -> Vec<Candidate> {
    let mut out = Vec::with_capacity(5);

    let setting = setting.trim();
    if !setting.is_empty() {
        out.push(Candidate {
            source: SOURCE_SETTING,
            path: PathBuf::from(setting),
        });
    }

    // Bare name: resolution is the OS's job, and the spawn is the existence
    // check. Anything else would re-implement `PATHEXT` badly.
    out.push(Candidate {
        source: SOURCE_PATH,
        path: PathBuf::from("ffmpeg"),
    });

    if let Ok(home) = std::env::var("USERPROFILE") {
        out.push(Candidate {
            source: SOURCE_SCOOP,
            path: PathBuf::from(home).join("scoop\\shims\\ffmpeg.exe"),
        });
    }
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        out.push(Candidate {
            source: SOURCE_WINGET,
            path: PathBuf::from(local).join("Microsoft\\WinGet\\Links\\ffmpeg.exe"),
        });
    }
    if let Ok(programs) = std::env::var("ProgramFiles") {
        out.push(Candidate {
            source: SOURCE_PROGRAM_FILES,
            path: PathBuf::from(programs).join("ffmpeg\\bin\\ffmpeg.exe"),
        });
    }

    out
}

/// Resolves, validates and (on success) caches. The `Vec<Attempt>` is the audit
/// trail the UI shows when nothing worked.
fn locate(setting: &str) -> (Option<Ffmpeg>, Vec<Attempt>) {
    if let Some(hit) = cached(setting) {
        let attempt = Attempt {
            source: hit.source.to_string(),
            path: hit.path.to_string_lossy().into_owned(),
            outcome: OUTCOME_OK.to_string(),
            detail: None,
        };
        return (Some(hit), vec![attempt]);
    }

    let mut attempts = Vec::new();
    for candidate in candidates(setting) {
        // A bare name has nothing to stat; everything else does, and skipping
        // the spawn for a path that is not there keeps the failure path fast.
        if candidate.source != SOURCE_PATH && !candidate.path.exists() {
            attempts.push(Attempt {
                source: candidate.source.to_string(),
                path: candidate.path.to_string_lossy().into_owned(),
                outcome: OUTCOME_NOT_FOUND.to_string(),
                detail: None,
            });
            continue;
        }

        match probe(&candidate.path) {
            Ok((version, encoders)) => {
                let ffmpeg = Ffmpeg {
                    path: candidate.path.clone(),
                    version,
                    source: candidate.source,
                    encoders,
                };
                attempts.push(Attempt {
                    source: candidate.source.to_string(),
                    path: candidate.path.to_string_lossy().into_owned(),
                    outcome: OUTCOME_OK.to_string(),
                    detail: None,
                });
                remember(setting, &ffmpeg);
                return (Some(ffmpeg), attempts);
            }
            Err(e) => attempts.push(Attempt {
                source: candidate.source.to_string(),
                path: candidate.path.to_string_lossy().into_owned(),
                outcome: if candidate.source == SOURCE_PATH {
                    OUTCOME_NOT_FOUND.to_string()
                } else {
                    OUTCOME_UNUSABLE.to_string()
                },
                detail: Some(e),
            }),
        }
    }

    (None, attempts)
}

/// The full picture, for `record_availability` and for the Settings page.
///
/// `needed` is the encoder set the caller cares about — pass what the *current*
/// format requires, or [`required_encoders`] for "can this app record at all".
pub fn availability(setting: &str, needed: &[&str]) -> Availability {
    let (found, searched) = locate(setting);

    let Some(ffmpeg) = found else {
        return Availability {
            available: false,
            path: String::new(),
            version: String::new(),
            source: String::new(),
            encoders: Vec::new(),
            hardware_encoders: Vec::new(),
            missing: vec![Missing {
                id: "ffmpeg".into(),
                label: "ffmpeg, which santi.sharex could not find".into(),
            }],
            remedies: install_remedies(),
            searched,
        };
    };

    let missing: Vec<Missing> = needed
        .iter()
        .filter(|e| !ffmpeg.has(e))
        .map(|e| Missing {
            id: (*e).to_string(),
            label: encoder_label(e),
        })
        .collect();

    let known = [ENCODER_X264, ENCODER_GIF, ENCODER_NVENC];
    Availability {
        available: missing.is_empty(),
        path: ffmpeg.path.to_string_lossy().into_owned(),
        version: ffmpeg.version.clone(),
        source: ffmpeg.source.to_string(),
        encoders: known
            .iter()
            .filter(|e| ffmpeg.has(e))
            .map(|e| (*e).to_string())
            .collect(),
        hardware_encoders: HW_ENCODERS
            .iter()
            .filter(|e| ffmpeg.has(e))
            .map(|e| (*e).to_string())
            .collect(),
        // An ffmpeg that exists but lacks an encoder is a *minimal* build, and
        // the fix is the same install command — the full builds both package
        // managers ship carry all of these.
        remedies: if missing.is_empty() {
            Vec::new()
        } else {
            install_remedies()
        },
        missing,
        searched,
    }
}

/// Both encoders M4 needs before recording is offered at all.
pub fn required_encoders() -> [&'static str; 2] {
    [ENCODER_X264, ENCODER_GIF]
}

/// The binary, or the reason there is not one, as a sentence a toast can carry.
/// The structured form is one [`availability`] call away for anything that can
/// render more than a string.
pub fn require(setting: &str, needed: &[&str]) -> Result<Ffmpeg, String> {
    let (found, _) = locate(setting);
    match found {
        Some(ffmpeg) if needed.iter().all(|e| ffmpeg.has(e)) => Ok(ffmpeg),
        _ => Err(availability(setting, needed).sentence()),
    }
}

fn encoder_label(encoder: &str) -> String {
    match encoder {
        ENCODER_X264 => "the libx264 encoder, which MP4 recording needs".into(),
        ENCODER_GIF => "the gif encoder, which GIF recording needs".into(),
        ENCODER_NVENC => "the h264_nvenc encoder, which hardware encoding needs".into(),
        other => format!("the {other} encoder"),
    }
}

/// The two package managers this machine plausibly has, plus the escape hatch.
/// Never a download: see the module header.
fn install_remedies() -> Vec<Remedy> {
    vec![
        Remedy {
            id: REMEDY_SCOOP.into(),
            label: "Install with scoop".into(),
            command: "scoop install ffmpeg".into(),
        },
        Remedy {
            id: REMEDY_WINGET.into(),
            label: "Install with WinGet".into(),
            command: "winget install ffmpeg".into(),
        },
        Remedy {
            id: REMEDY_BROWSE.into(),
            label: "Choose ffmpeg.exe…".into(),
            command: String::new(),
        },
    ]
}

// ---------------------------------------------------------------------------
// probing
// ---------------------------------------------------------------------------

/// `-version` then `-encoders`, both of which must succeed for a candidate to
/// count. Two spawns rather than one because there is no single invocation that
/// answers both, and this runs once per session.
fn probe(path: &PathBuf) -> Result<(String, Vec<String>), String> {
    let version_out = run_capture(path, &["-hide_banner", "-nostdin", "-version"])?;
    let version = version_out
        .lines()
        .next()
        .unwrap_or("")
        .split(" Copyright")
        .next()
        .unwrap_or("")
        .trim()
        .trim_start_matches("ffmpeg version ")
        .to_string();
    if version.is_empty() {
        return Err("-version printed nothing recognisable".into());
    }

    let encoders_out = run_capture(path, &["-hide_banner", "-nostdin", "-encoders"])?;
    Ok((version, parse_encoders(&encoders_out)))
}

/// Runs a short-lived ffmpeg and returns its stdout, killing it if it hangs.
///
/// The timeout is not paranoia: a candidate path can point at something that is
/// not ffmpeg at all, and a probe that blocks forever would take the whole
/// Settings page with it.
fn run_capture(path: &PathBuf, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new(path);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    no_window(&mut cmd);

    let mut child = cmd.spawn().map_err(|e| e.to_string())?;

    // `wait_with_output` cannot be given a deadline, so the read happens on a
    // thread and this one watches the clock.
    let mut stdout = child.stdout.take();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(pipe) = stdout.as_mut() {
            use std::io::Read;
            let _ = pipe.read_to_string(&mut buf);
        }
        let _ = tx.send(buf);
    });

    match rx.recv_timeout(PROBE_TIMEOUT) {
        Ok(text) => {
            let _ = child.wait();
            Ok(text)
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(format!(
                "did not answer within {}s",
                PROBE_TIMEOUT.as_secs()
            ))
        }
    }
}

/// Pulls the encoder names out of `ffmpeg -encoders`.
///
/// The table looks like `` V....D libx264   libx264 H.264 / AVC …`` under a
/// `------` rule; everything above the rule is the flag legend and would
/// otherwise contribute nonsense names.
fn parse_encoders(listing: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_table = false;
    for line in listing.lines() {
        let trimmed = line.trim_end();
        if !in_table {
            if trimmed.trim_start().starts_with("------") {
                in_table = true;
            }
            continue;
        }
        let mut fields = trimmed.split_whitespace();
        // First field is the capability flags, second is the name.
        let Some(_flags) = fields.next() else { continue };
        let Some(name) = fields.next() else { continue };
        if !name.is_empty() {
            out.push(name.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real shape of `ffmpeg -encoders` on this machine, legend and all.
    const LISTING: &str = "Encoders:
 V..... = Video
 A..... = Audio
 ------
 V....D gif                  GIF (Graphics Interchange Format)
 V....D libx264              libx264 H.264 / AVC / MPEG-4 AVC / MPEG-4 part 10 (codec h264)
 V....D h264_nvenc           NVIDIA NVENC H.264 encoder (codec h264)
 A....D aac                  AAC (Advanced Audio Coding)
";

    #[test]
    fn the_legend_above_the_rule_is_not_mistaken_for_encoders() {
        let names = parse_encoders(LISTING);
        assert_eq!(names, ["gif", "libx264", "h264_nvenc", "aac"]);
        assert!(
            !names.iter().any(|n| n == "="),
            "the flag legend leaked into the encoder list"
        );
    }

    #[test]
    fn an_encoder_lookup_is_exact() {
        let ffmpeg = Ffmpeg {
            path: PathBuf::from("ffmpeg"),
            version: "8.1.1".into(),
            source: SOURCE_PATH,
            encoders: parse_encoders(LISTING),
        };
        assert!(ffmpeg.has(ENCODER_X264));
        assert!(ffmpeg.has(ENCODER_GIF));
        assert!(ffmpeg.has(ENCODER_NVENC));
        // Prefix matches must not count: `libx264rgb` is a different encoder.
        assert!(!ffmpeg.has("libx264rgb"));
        assert!(!ffmpeg.has("h264_amf"));
    }

    /// The setting is tried first and `PATH` second — the whole point of the
    /// setting is to override a resolution that picked the wrong build.
    #[test]
    fn the_setting_outranks_every_other_location() {
        let with = candidates(r"D:\tools\ffmpeg.exe");
        assert_eq!(with[0].source, SOURCE_SETTING);
        assert_eq!(with[0].path, PathBuf::from(r"D:\tools\ffmpeg.exe"));
        assert_eq!(with[1].source, SOURCE_PATH);

        // Blank means "resolve for me", not "look for a binary called nothing".
        let without = candidates("   ");
        assert_eq!(without[0].source, SOURCE_PATH);
        assert!(!without.iter().any(|c| c.source == SOURCE_SETTING));
    }

    /// The unavailable state has to carry enough for the UI to build its own
    /// sentence and its own buttons. A message string alone would not.
    #[test]
    fn an_unavailable_state_names_what_is_missing_and_how_to_fix_it() {
        let state = Availability {
            available: false,
            path: String::new(),
            version: String::new(),
            source: String::new(),
            encoders: Vec::new(),
            hardware_encoders: Vec::new(),
            missing: vec![Missing {
                id: "ffmpeg".into(),
                label: "ffmpeg, which santi.sharex could not find".into(),
            }],
            remedies: install_remedies(),
            searched: Vec::new(),
        };

        let ids: Vec<&str> = state.remedies.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, [REMEDY_SCOOP, REMEDY_WINGET, REMEDY_BROWSE]);
        assert_eq!(state.remedies[0].command, "scoop install ffmpeg");
        assert_eq!(state.remedies[1].command, "winget install ffmpeg");
        assert!(
            state.remedies[2].command.is_empty(),
            "Browse is a control, not a command to paste"
        );
        assert!(state.sentence().contains("scoop install ffmpeg"));

        // And it must never suggest downloading one (M4 §1).
        let json = serde_json::to_string(&state).unwrap();
        assert!(!json.to_lowercase().contains("download"));
    }
}
