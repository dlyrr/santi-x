//! FTP and explicit FTPS uploads (M3 §5).
//!
//! # SFTP is not here, and says so
//!
//! FTPS and SFTP sound like the same thing and are not: FTPS is FTP with TLS
//! bolted on, SFTP is a subsystem of SSH and shares no code with either. This
//! module implements plain FTP and **explicit FTPS** (`AUTH TLS`), and nothing
//! else. SFTP would need an SSH stack — in practice libssh2, which is C, which
//! this machine cannot build — and half-implementing it would be worse than an
//! honest gap.
//!
//! `FtpSettings::security` can still *say* `sftp` — it is a plain string, and a
//! settings file is a text file — so [`Ftp::from_settings`] refuses it by name
//! with [`SFTP_NOT_IMPLEMENTED`], before a password is so much as read. That is
//! the difference between "santi.sharex does not do this" and a connection error
//! the user spends an evening debugging.
//!
//! # TLS
//!
//! `rustls` with the `ring` provider, never native-tls — the same
//! no-C-toolchain constraint as the rest of M3. The trust anchors are
//! `webpki-roots`, the same set HTTPS uses here, and there is deliberately no
//! "accept any certificate" escape hatch: a switch like that gets turned on once
//! in frustration and then left on forever.
//!
//! # The password
//!
//! Lives in Windows Credential Manager (M3 §2) and is read once per upload. It
//! is never stored in `settings.json`, never printed, and every error string
//! this module produces is run through [`redact`] before it is returned — an FTP
//! failure that echoed the password into a toast is the exact bug §2 exists to
//! prevent, and FTP servers are chatty about their errors.

use std::io::Read;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use suppaftp::rustls::{ClientConfig, RootCertStore};
use suppaftp::types::FileType;
use suppaftp::{FtpStream, ImplFtpStream, Mode, RustlsConnector, RustlsFtpStream, TlsStream};

use crate::store::{
    Settings, FTP_SECURITY_EXPLICIT_TLS, FTP_SECURITY_PLAIN, FTP_SECURITY_SFTP,
};

use super::custom::redact;
use super::secrets::Secret;
use super::{Destination, UploadCtx, UploadResult};

/// The one secret this destination has (M3 §2). Matches the field name `mod.rs`
/// reports in `destination_status`; the credential-store namespace is
/// `DestinationKind::Ftp.id()`, which `UploadCtx` supplies.
pub const PASSWORD_FIELD: &str = "password";

/// What the destination picker says instead of offering SFTP (M3 §5).
pub const SFTP_NOT_IMPLEMENTED: &str = "santi.sharex does not support SFTP. \
Despite the name it is not a variant of FTP or FTPS — it is a subsystem of SSH, and carrying an \
SSH stack for it is not something santi.sharex does. This is a real gap, not a setting to turn on. \
Use FTP or explicit FTPS (AUTH TLS) here, or a custom uploader for an HTTP endpoint.";

/// What plain FTP costs, in the words the UI should use where the user turns
/// FTPS off (M3 §5: say it there, do not bury it in docs).
pub const PLAIN_FTP_WARNING: &str = "Plain FTP sends your password, and the file, in clear text — \
anyone between this machine and the server can read both. Leave FTPS on unless the server cannot \
do it.";

/// The IANA control port.
pub const DEFAULT_PORT: u16 = 21;

/// The user name an FTP server expects when nobody signed in, and the password
/// convention that goes with it. Used only when the destination has no user name
/// configured — an anonymous login is a real configuration, so an empty field is
/// not by itself an error.
const ANONYMOUS_USER: &str = "anonymous";
const ANONYMOUS_PASSWORD: &str = "anonymous@santi.sharex";

/// How long the TCP connect gets. A host that is not there should fail quickly.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// Read/write timeout on the *control* connection, so a server that accepts the
/// socket and then stops talking cannot hold the transfer open indefinitely.
///
/// It does not cover the data connection, which suppaftp opens separately. What
/// bounds that is the payload reader: `ctx.reader()` fails the moment the user
/// cancels, which tears the data connection down mid-transfer.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------------------
// the destination
// ---------------------------------------------------------------------------

/// A configured FTP destination.
///
/// Owned copies rather than a borrow of `Settings`: a transfer takes minutes,
/// and holding a reference into the settings for that long would mean holding
/// something the user can change underneath it.
///
/// `Debug` is safe to derive here and is checked by a test: the password is not
/// a field, it is read from the credential store per transfer and dropped with
/// the session.
#[derive(Debug)]
pub struct Ftp {
    host: String,
    port: u16,
    /// Already resolved to "who we will log in as", so the anonymous fallback is
    /// decided in one place.
    username: String,
    anonymous: bool,
    remote_dir: String,
    passive: bool,
    ftps: bool,
    url_prefix: String,
}

impl Ftp {
    pub fn from_settings(settings: &Settings) -> Result<Self, String> {
        let config = &settings.ftp;

        // First, and before the host is even looked at: SFTP is refused by name
        // rather than attempted and failed. Nothing below this line reads a
        // credential, so an SFTP destination never touches the password either.
        let security = config.security.trim();
        let ftps = if security.eq_ignore_ascii_case(FTP_SECURITY_SFTP) {
            return Err(SFTP_NOT_IMPLEMENTED.to_string());
        } else if security.is_empty() || security.eq_ignore_ascii_case(FTP_SECURITY_PLAIN) {
            false
        } else if security.eq_ignore_ascii_case(FTP_SECURITY_EXPLICIT_TLS) {
            true
        } else {
            return Err(format!(
                "\"{security}\" is not a transfer mode santi.sharex knows. Use \"{FTP_SECURITY_PLAIN}\" for plain FTP or \"{FTP_SECURITY_EXPLICIT_TLS}\" for FTPS."
            ));
        };

        let host = config.host.trim().to_string();
        if host.is_empty() {
            return Err(
                "No FTP server is set up yet — add its address in Settings › Destinations."
                    .to_string(),
            );
        }
        if config.port == 0 {
            return Err("An FTP port of 0 is not a port anything listens on.".to_string());
        }

        let configured_user = config.username.trim();
        let anonymous = configured_user.is_empty();

        Ok(Self {
            host,
            port: config.port,
            username: if anonymous {
                ANONYMOUS_USER.to_string()
            } else {
                configured_user.to_string()
            },
            anonymous,
            remote_dir: config.remote_dir.clone(),
            passive: config.passive,
            ftps,
            url_prefix: config.url_prefix.clone(),
        })
    }

    /// The password to log in with, and the [`Secret`] it came from — held by the
    /// caller so the plaintext lives exactly as long as the session and is
    /// zeroed on the way out.
    ///
    /// A destination with no stored password is not an error: an anonymous login
    /// wants the conventional `anonymous@` and a server that needs a real one
    /// will say so itself, which is a better message than anything guessed here.
    fn password(&self, ctx: &UploadCtx) -> Result<Option<Secret>, String> {
        ctx.optional_secret(PASSWORD_FIELD)
    }

    fn fallback_password(&self) -> &'static str {
        if self.anonymous {
            ANONYMOUS_PASSWORD
        } else {
            ""
        }
    }

    fn label(&self) -> String {
        format!(
            "{} as {}{}",
            self.host,
            self.username,
            if self.ftps { " over FTPS" } else { "" }
        )
    }
}

impl Destination for Ftp {
    /// The payload comes from `ctx.reader()`, not from `bytes` — same data, but
    /// the reader is what reports progress and fails the transfer on a cancel.
    fn upload(&self, ctx: &UploadCtx, _bytes: &[u8], name: &str) -> Result<UploadResult, String> {
        let secret = self.password(ctx)?;
        let password = secret
            .as_ref()
            .map(|s| s.expose())
            .unwrap_or_else(|| self.fallback_password());

        ctx.check_cancel()?;

        // One redaction point for the whole session, so no error path added
        // later can forget it.
        self.session(password, |ftp| {
            enter_dir(ftp, &self.remote_dir, true)?;

            let mut reader = ctx.reader();
            if let Err(e) = ftp.store_file(name, &mut reader) {
                // A transfer that stopped part-way leaves a truncated file at the
                // address the user is about to be handed — including when they
                // cancelled it. Best effort: the control connection may be no
                // healthier than the data one was.
                let _ = ftp.remove_file(name);
                return Err(format!("Could not upload {name}: {e}"));
            }
            Ok(())
        })
        .map_err(|e| redact(e, &[password]))?;

        Ok(UploadResult {
            url: self.public_url(name),
            // FTP has no notion of either. A deletion link would have to be an
            // FTP path with credentials in it, which is not something to put on
            // a clipboard.
            deletion_url: None,
            thumbnail_url: None,
        })
    }

    /// Connects, logs in and looks at the remote directory — and leaves nothing
    /// behind, so it never creates one.
    fn test(&self, ctx: &UploadCtx) -> Result<String, String> {
        let secret = self.password(ctx)?;
        let password = secret
            .as_ref()
            .map(|s| s.expose())
            .unwrap_or_else(|| self.fallback_password());

        let dir = self.remote_dir.trim().to_string();
        let reached = self
            .session(password, |ftp| {
                // `false`: a test that creates directories is a test that changed
                // the server. A missing directory is reported, and the first
                // upload is what makes it.
                Ok(enter_dir(ftp, &dir, false).is_ok())
            })
            .map_err(|e| redact(e, &[password]))?;

        let mut message = format!("Connected to {}. ", self.label());
        if dir.is_empty() {
            message.push_str("Uploads will go to the directory the login lands in.");
        } else if reached {
            message.push_str(&format!("Uploads will go to \"{dir}\"."));
        } else {
            message.push_str(&format!(
                "\"{dir}\" does not exist yet; the first upload will create it."
            ));
        }
        if !self.ftps {
            message.push(' ');
            message.push_str(PLAIN_FTP_WARNING);
        }
        Ok(message)
    }
}

impl Ftp {
    /// Connect, log in, run `work`, quit — the part that is identical for FTP and
    /// FTPS, written once against the stream type rather than twice against the
    /// two concrete ones.
    fn session<T, F>(&self, password: &str, work: F) -> Result<T, String>
    where
        F: FnOnce(&mut dyn AnyFtp) -> Result<T, String>,
    {
        let address = resolve(&self.host, self.port)?;

        if self.ftps {
            let mut ftp = RustlsFtpStream::connect_timeout(address, CONNECT_TIMEOUT)
                .map_err(|e| format!("Could not reach {}: {e}", self.host))?
                // Explicit FTPS: plain connect, then AUTH TLS *before* the login,
                // so the password is never the thing that goes out in the clear.
                .into_secure(RustlsConnector::from(tls_config()?), &self.host)
                .map_err(|e| {
                    format!(
                        "{} accepted the connection but the TLS handshake failed: {e}. \
                         santi.sharex will not accept a self-signed certificate.",
                        self.host
                    )
                })?;
            self.run(&mut ftp, password, work)
        } else {
            let mut ftp = FtpStream::connect_timeout(address, CONNECT_TIMEOUT)
                .map_err(|e| format!("Could not reach {}: {e}", self.host))?;
            self.run(&mut ftp, password, work)
        }
    }

    /// `S: 'static` because `work` is handed a `&mut dyn AnyFtp`, and a trait
    /// object cannot borrow from a stream type with an unknown lifetime. Both
    /// concrete streams are owned sockets, so it costs nothing.
    fn run<S: TlsStream + 'static, T, F>(
        &self,
        ftp: &mut ImplFtpStream<S>,
        password: &str,
        work: F,
    ) -> Result<T, String>
    where
        F: FnOnce(&mut dyn AnyFtp) -> Result<T, String>,
    {
        let socket = ftp.get_ref();
        let _ = socket.set_read_timeout(Some(CONTROL_TIMEOUT));
        let _ = socket.set_write_timeout(Some(CONTROL_TIMEOUT));

        ftp.set_mode(if self.passive {
            Mode::Passive
        } else {
            Mode::Active
        });

        ftp.login(self.username.as_str(), password)
            .map_err(|e| format!("{} refused the login: {e}", self.host))?;

        // Without this the server is free to mangle line endings, which turns a
        // PNG into a corrupt PNG that still has the right size.
        ftp.transfer_type(FileType::Binary)
            .map_err(|e| format!("Could not switch {} into binary mode: {e}", self.host))?;

        let outcome = work(ftp);

        // Not fatal either way — the bytes are already there, or already not —
        // but skipping it leaves the server holding a session open until it
        // times out.
        if let Err(e) = ftp.quit() {
            eprintln!("santi.sharex: {} did not acknowledge QUIT: {e}", self.host);
        }
        outcome
    }
}

/// The handful of commands the session bodies use, so they can be written once
/// against a trait object instead of being made generic all the way down — and
/// so [`enter_dir`]'s two behaviours can be tested without an FTP server.
///
/// The names deliberately differ from suppaftp's (`cwd`, `mkdir`, `put_file`,
/// `rm`): identical ones would shadow the inherent methods inside the impl below
/// and turn a typo into infinite recursion.
trait AnyFtp {
    fn change_dir(&mut self, path: &str) -> Result<(), String>;
    fn make_dir(&mut self, path: &str) -> Result<(), String>;
    fn store_file(&mut self, name: &str, reader: &mut dyn Read) -> Result<u64, String>;
    fn remove_file(&mut self, name: &str) -> Result<(), String>;
}

impl<S: TlsStream> AnyFtp for ImplFtpStream<S> {
    fn change_dir(&mut self, path: &str) -> Result<(), String> {
        self.cwd(path).map_err(|e| e.to_string())
    }

    fn make_dir(&mut self, path: &str) -> Result<(), String> {
        self.mkdir(path).map_err(|e| e.to_string())
    }

    fn store_file(&mut self, name: &str, reader: &mut dyn Read) -> Result<u64, String> {
        // `&mut &mut dyn Read`, because `put_file` wants a `&mut R` with `R:
        // Read + Sized` and `dyn Read` is not sized — but a `&mut dyn Read` is,
        // and it implements `Read`.
        let mut reader = reader;
        self.put_file(name, &mut reader).map_err(|e| e.to_string())
    }

    fn remove_file(&mut self, name: &str) -> Result<(), String> {
        self.rm(name).map_err(|e| e.to_string())
    }
}

/// Walks into `dir` one segment at a time, creating what is missing when
/// `create` is set.
///
/// Segment by segment rather than one `MKD /a/b/c`, because most servers refuse
/// to create intermediate directories and the ones that accept it disagree about
/// what it means.
fn enter_dir(ftp: &mut dyn AnyFtp, dir: &str, create: bool) -> Result<(), String> {
    let dir = dir.trim();
    if dir.is_empty() {
        return Ok(());
    }

    if dir.starts_with('/') || dir.starts_with('\\') {
        ftp.change_dir("/")
            .map_err(|e| format!("Could not start from the server's root directory: {e}"))?;
    }

    for segment in dir.split(['/', '\\']).filter(|s| !s.is_empty()) {
        if ftp.change_dir(segment).is_ok() {
            continue;
        }
        if !create {
            return Err(format!("There is no \"{segment}\" directory on the server."));
        }
        ftp.make_dir(segment).map_err(|e| {
            format!("There is no \"{segment}\" directory on the server and it could not be created: {e}")
        })?;
        ftp.change_dir(segment)
            .map_err(|e| format!("Created \"{segment}\" on the server but could not open it: {e}"))?;
    }
    Ok(())
}

fn resolve(host: &str, port: u16) -> Result<SocketAddr, String> {
    (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("Could not look up \"{host}\": {e}"))?
        .next()
        .ok_or_else(|| format!("\"{host}\" does not resolve to an address."))
}

/// The TLS settings FTPS connects with.
///
/// The provider is named rather than left to `ClientConfig::builder()`, which
/// picks whichever one is compiled in and *panics* if that is ambiguous. Naming
/// `ring` keeps this correct if anything in the tree ever pulls in a second
/// provider — a panic inside an upload is not a failure mode worth risking to
/// save a line.
fn tls_config() -> Result<Arc<ClientConfig>, String> {
    let roots = RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let provider = Arc::new(suppaftp::rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("Could not set up TLS for FTPS: {e}"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

// ---------------------------------------------------------------------------
// the link that goes on the clipboard
// ---------------------------------------------------------------------------

impl Ftp {
    /// Where the uploaded file can be found.
    ///
    /// With a URL prefix — the usual setup, where the FTP account writes into a
    /// directory a web server also serves — this is the public address. Without
    /// one it is the `ftp://` path the file actually went to, which is at least
    /// true; inventing an `https://` guess would hand the user a dead link.
    ///
    /// **Never carries credentials.** `ftp://user:password@host/…` is a legal
    /// URL and this must not produce one: the whole point of a copied link is
    /// that it gets pasted somewhere.
    pub fn public_url(&self, name: &str) -> String {
        let encoded = encode_segment(name);

        let prefix = self.url_prefix.trim();
        if !prefix.is_empty() {
            return format!("{}/{}", prefix.trim_end_matches('/'), encoded);
        }

        let mut url = format!("ftp://{}", self.host);
        if self.port != DEFAULT_PORT {
            url.push_str(&format!(":{}", self.port));
        }
        for segment in self.remote_dir.split(['/', '\\']).filter(|s| !s.is_empty()) {
            url.push('/');
            url.push_str(&encode_segment(segment));
        }
        url.push('/');
        url.push_str(&encoded);
        url
    }
}

/// Percent-encodes one path segment.
///
/// Capture file names come out of `store::resolve_filename`, which already
/// strips what Windows refuses — but spaces and ` (2)` survive it, and a raw
/// space in a link is a link that breaks the moment it is pasted into anything
/// that autolinks.
fn encode_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for byte in segment.as_bytes() {
        let c = *byte as char;
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::FtpSettings;

    fn settings(ftp: FtpSettings) -> Settings {
        let mut settings = Settings::default();
        settings.ftp = ftp;
        settings
    }

    fn configured() -> Ftp {
        Ftp::from_settings(&settings(FtpSettings {
            host: "files.example.com".into(),
            port: DEFAULT_PORT,
            username: "santi".into(),
            remote_dir: "/public_html/shots".into(),
            passive: true,
            security: FTP_SECURITY_EXPLICIT_TLS.into(),
            url_prefix: String::new(),
        }))
        .expect("a complete config must load")
    }

    /// A fresh install has no FTP destination, and saying so is more useful than
    /// a connection error against an empty host.
    #[test]
    fn an_unconfigured_destination_says_what_is_missing() {
        let error = Ftp::from_settings(&Settings::default()).unwrap_err();
        assert!(error.contains("Settings"), "{error}");

        let mut ftp = FtpSettings::default();
        ftp.host = "files.example.com".into();
        ftp.port = 0;
        assert!(Ftp::from_settings(&settings(ftp)).is_err());
    }

    /// The gap M3 §5 asks santi.sharex to be honest about. An `sftp` destination
    /// must be refused by name, and refused *early* — before the host is
    /// validated and long before a password is read out of Credential Manager.
    #[test]
    fn an_sftp_destination_is_refused_by_name_before_anything_else() {
        let mut config = FtpSettings::default();
        config.security = FTP_SECURITY_SFTP.into();
        // No host either, so the only way this can produce the SFTP message is
        // by checking `security` first.
        let error = Ftp::from_settings(&settings(config)).unwrap_err();
        assert_eq!(error, SFTP_NOT_IMPLEMENTED);
    }

    /// A hand-edited or newer-build `security` must not be guessed at — least of
    /// all guessed *down* to plain FTP, which would send the password in the
    /// clear for a user who asked for something else.
    #[test]
    fn an_unknown_security_value_is_refused_rather_than_downgraded() {
        let mut config = FtpSettings::default();
        config.host = "files.example.com".into();
        config.security = "implicitTls".into();
        let error = Ftp::from_settings(&settings(config)).unwrap_err();
        assert!(error.contains("implicitTls"), "{error}");
        assert!(error.contains(FTP_SECURITY_EXPLICIT_TLS), "{error}");
    }

    /// `Debug` is derived on [`Ftp`], so this pins the reason that is safe: the
    /// password is not a field. If one is ever added, this fails.
    #[test]
    fn the_destination_holds_no_password_to_print() {
        let printed = format!("{:?}", configured());
        assert!(printed.contains("files.example.com"));
        assert!(!printed.to_lowercase().contains("password"), "{printed}");
    }

    /// An empty user name is an anonymous login, not a broken destination —
    /// which is why `mod.rs` marks the password optional.
    #[test]
    fn an_empty_user_name_is_an_anonymous_login() {
        let mut config = FtpSettings::default();
        config.host = "files.example.com".into();
        let ftp = Ftp::from_settings(&settings(config)).expect("must load");
        assert!(ftp.anonymous);
        assert_eq!(ftp.username, ANONYMOUS_USER);
        assert_eq!(ftp.fallback_password(), ANONYMOUS_PASSWORD);

        // A named login with no stored password gets an empty one and lets the
        // server say what it thinks, rather than guessing at a convention.
        assert_eq!(configured().fallback_password(), "");
    }

    #[test]
    fn a_url_prefix_produces_the_public_link() {
        let mut config = FtpSettings::default();
        config.host = "files.example.com".into();
        config.url_prefix = "https://cdn.example.com/shots".into();
        let ftp = Ftp::from_settings(&settings(config.clone())).unwrap();
        assert_eq!(
            ftp.public_url("region_2026-08-04.png"),
            "https://cdn.example.com/shots/region_2026-08-04.png"
        );

        // A trailing slash on the prefix must not double up.
        config.url_prefix = "https://cdn.example.com/shots/".into();
        let ftp = Ftp::from_settings(&settings(config)).unwrap();
        assert_eq!(
            ftp.public_url("a.png"),
            "https://cdn.example.com/shots/a.png"
        );
    }

    #[test]
    fn without_a_prefix_the_link_is_the_ftp_path_it_really_went_to() {
        assert_eq!(
            configured().public_url("a.png"),
            "ftp://files.example.com/public_html/shots/a.png"
        );

        let mut config = FtpSettings::default();
        config.host = "files.example.com".into();
        config.port = 2121;
        let ftp = Ftp::from_settings(&settings(config)).unwrap();
        assert_eq!(ftp.public_url("a.png"), "ftp://files.example.com:2121/a.png");
    }

    /// The link goes on the clipboard, so it must not be able to carry the
    /// login. `ftp://user:password@host/…` is a legal URL and exactly the thing
    /// M3 §2 forbids.
    #[test]
    fn the_link_never_carries_credentials() {
        let url = configured().public_url("a.png");
        assert!(!url.contains('@'), "{url}");
        assert!(!url.contains("santi"), "{url}");
    }

    #[test]
    fn file_names_are_encoded_for_a_link() {
        assert_eq!(
            encode_segment("region_2026-08-04.png"),
            "region_2026-08-04.png"
        );
        assert_eq!(encode_segment("shot (2).png"), "shot%20%282%29.png");
        assert_eq!(encode_segment("a b"), "a%20b");
    }

    /// A test connection must not change the server, so a missing directory is
    /// reported rather than created — and an upload must create it, or the first
    /// upload to a fresh account fails.
    #[test]
    fn only_an_upload_creates_directories() {
        /// Nothing exists, everything can be created — so `cwd` fails until the
        /// segment has been made.
        #[derive(Default)]
        struct Fake {
            made: Vec<String>,
        }

        impl AnyFtp for Fake {
            fn change_dir(&mut self, path: &str) -> Result<(), String> {
                if path == "/" || self.made.iter().any(|m| m == path) {
                    Ok(())
                } else {
                    Err("550 No such directory".to_string())
                }
            }
            fn make_dir(&mut self, path: &str) -> Result<(), String> {
                self.made.push(path.to_string());
                Ok(())
            }
            fn store_file(&mut self, _: &str, _: &mut dyn Read) -> Result<u64, String> {
                Ok(0)
            }
            fn remove_file(&mut self, _: &str) -> Result<(), String> {
                Ok(())
            }
        }

        let mut probe = Fake::default();
        let error = enter_dir(&mut probe, "/public_html/shots", false).unwrap_err();
        assert!(error.contains("public_html"), "{error}");
        assert!(
            probe.made.is_empty(),
            "a connection test created {:?} on the server",
            probe.made
        );

        let mut upload = Fake::default();
        enter_dir(&mut upload, "/public_html/shots", true).expect("an upload must create the path");
        assert_eq!(upload.made, vec!["public_html", "shots"]);
    }

    /// SFTP has to read as a gap santi.sharex owns, not as a setting the user
    /// got wrong.
    #[test]
    fn sftp_is_named_as_unimplemented_rather_than_offered() {
        assert!(SFTP_NOT_IMPLEMENTED.contains("SFTP"));
        assert!(SFTP_NOT_IMPLEMENTED.contains("SSH"));
        assert!(SFTP_NOT_IMPLEMENTED.contains("FTPS"));
    }

    /// M3 §5: the clear-text warning belongs where FTPS is turned off, so it
    /// lives next to the code and is repeated in the test-connection result.
    #[test]
    fn plain_ftp_says_what_it_costs() {
        assert!(PLAIN_FTP_WARNING.contains("clear text"));
        assert!(PLAIN_FTP_WARNING.contains("password"));
    }

    /// The FTPS client config has to actually build. It is constructed once per
    /// upload, and a provider mismatch is a panic rather than an error — the
    /// cheapest place to find that out is here.
    #[test]
    fn the_ftps_client_config_builds_with_real_roots() {
        let config = tls_config().expect("the FTPS TLS config must build");
        assert!(
            !config.crypto_provider().cipher_suites.is_empty(),
            "the TLS provider offered no cipher suites"
        );
        assert!(
            !webpki_roots::TLS_SERVER_ROOTS.is_empty(),
            "there are no trust anchors to verify an FTPS server against"
        );
    }
}
