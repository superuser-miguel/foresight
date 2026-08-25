//! Host-key trust for remote sync (Milestone 5, part 2).
//!
//! Foresight keeps its **own** `known_hosts` in the app config dir and never
//! reads or writes `~/.ssh`. Two reasons, and both are load-bearing:
//!
//! 1. `~/.ssh` is not visible in the sandbox and must stay that way. The agent
//!    socket (`--socket=ssh-auth`, part 1) is all we need for *authentication*,
//!    and it hands over signatures rather than keys. Host-key *trust* is a
//!    separate problem and gets a separate, app-owned file.
//! 2. The sandbox home is **ephemeral**. ssh's own default file lives at
//!    `~/.ssh/known_hosts` inside the sandbox, so `StrictHostKeyChecking=accept-new`
//!    appears to work and then loses the record on the next launch — every
//!    connection is a first connection, which is TOFU that never remembers.
//!    The config dir persists (it is where `profiles.ini` already lives).
//!
//! The trust flow is therefore explicit, never `accept-new`: [`scan_host`]
//! fetches the host's keys, the UI shows the [`HostKey::fingerprint`] for
//! confirmation, and only then does [`trust`] write the entry. Transfers run
//! with `StrictHostKeyChecking=yes` against that file, so an unknown or changed
//! host key is a hard failure rather than a silent accept.
//!
//! ## rsync's `-e` is a string it re-parses
//!
//! Everything here ultimately becomes the value of rsync's `-e`, which rsync
//! tokenises **itself** — argv discipline does not save us at this one boundary.
//! Measured against the bundled rsync 3.4.4 (see PLAN.md §6):
//!
//! - Single **or** double quotes group a token, so a path containing a space
//!   survives as one argument.
//! - The *other* quote character is literal inside a quoted token.
//! - **Backslash is not an escape.** `/a\ b` tokenises to `/a\`, not `/a b`.
//!
//! So a value containing both quote characters is genuinely inexpressible, and
//! [`quote_for_rsh`] returns `None` rather than emitting a command that would
//! break in a way no one could diagnose from the error.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// ssh's default port. A `known_hosts` entry is bracketed (`[host]:port`) only
/// for a non-default port, which is the format ssh itself uses.
const DEFAULT_SSH_PORT: u16 = 22;

/// How long `ssh-keyscan` may wait for a host, in seconds. Short enough that a
/// dead host does not look like a hung app.
const SCAN_TIMEOUT_SECS: u32 = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustError {
    /// The config path cannot be expressed in an `-e` value (see module docs).
    UnquotablePath(String),
    /// The config path is not valid UTF-8.
    NonUtf8Path,
    /// The host string is not something we will hand to a command.
    InvalidHost(String),
    /// `ssh-keyscan` could not be run, or failed.
    ScanFailed(String),
    /// The host answered but offered no usable key.
    NoKeys(String),
}

impl std::fmt::Display for TrustError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use crate::i18n::{i18n, i18n_f};
        match self {
            Self::UnquotablePath(p) => write!(
                f,
                "{}",
                i18n_f(
                    "the path “{}” contains both a single and a double quote, which \
                     rsync's -e option cannot express",
                    &[p]
                )
            ),
            Self::NonUtf8Path => write!(f, "{}", i18n("the configuration path is not valid UTF-8")),
            Self::InvalidHost(h) => {
                write!(f, "{}", i18n_f("“{}” is not a valid host name", &[h]))
            }
            Self::ScanFailed(e) => write!(f, "{}", i18n_f("could not read the host key: {}", &[e])),
            Self::NoKeys(h) => write!(f, "{}", i18n_f("{} offered no host key", &[h])),
        }
    }
}

/// The app-managed `known_hosts`, beside `profiles.ini` in the config dir.
/// Inside the Flatpak that is
/// `~/.var/app/io.github.superuser_miguel.Foresight/config/foresight/known_hosts`.
pub fn known_hosts_path() -> PathBuf {
    gtk::glib::user_config_dir()
        .join("foresight")
        .join("known_hosts")
}

/// Quote one token for rsync's `-e` string, or `None` if it cannot be done.
///
/// Prefers single quotes: a `known_hosts` path is far likelier to contain an
/// apostrophe than a double quote, and reaching for double quotes only when
/// forced keeps the common output readable.
pub fn quote_for_rsh(value: &str) -> Option<String> {
    match (value.contains('\''), value.contains('"')) {
        (false, _) => Some(format!("'{value}'")),
        (true, false) => Some(format!("\"{value}\"")),
        // rsync offers no escape, so this is not a formatting problem we can
        // work around — it is a value the option cannot carry.
        (true, true) => None,
    }
}

/// Build the value for rsync's `-e`: the ssh command a transfer should use.
///
/// The options are the whole point of this module:
/// - `UserKnownHostsFile` — our file, never `~/.ssh/known_hosts`.
/// - `GlobalKnownHostsFile=/dev/null` — ignore the runtime's system file, so a
///   trust decision is ours alone and does not vary with the runtime image.
/// - `StrictHostKeyChecking=yes` — refuse anything not already confirmed.
///   Never `accept-new`: that is trust-on-first-use without the *use* asking.
/// - `BatchMode=yes` — never prompt. rsync runs with no terminal here, so an
///   ssh password or passphrase prompt would hang the transfer forever with a
///   progress bar sitting at zero. Failing immediately is the honest outcome;
///   key-based auth through the forwarded agent needs no prompt.
///
/// `port` must match the one used for [`scan_host`] and [`is_trusted`]: a
/// `known_hosts` entry is keyed by host *and* port, so connecting on a port we
/// did not confirm is correctly a strict-checking failure, not a fallback.
pub fn rsh_command(known_hosts: &Path, port: Option<u16>) -> Result<String, TrustError> {
    let path = known_hosts.to_str().ok_or(TrustError::NonUtf8Path)?;
    let known = quote_for_rsh(&format!("UserKnownHostsFile={path}"))
        .ok_or_else(|| TrustError::UnquotablePath(path.to_string()))?;
    let port_opt = match port {
        Some(p) if p != DEFAULT_SSH_PORT => format!(" -p {p}"),
        _ => String::new(),
    };
    Ok(format!(
        "ssh -o {known} -o GlobalKnownHostsFile=/dev/null \
         -o StrictHostKeyChecking=yes -o BatchMode=yes{port_opt}"
    ))
}

/// Reject host strings we will not hand to `ssh-keyscan` or rsync.
///
/// A leading `-` would be read as an option by anything we pass it to, and
/// whitespace would split into extra arguments. This is validation, not
/// sanitising: there is no rewriting of a bad value into a good one.
pub fn validate_host(host: &str) -> Result<(), TrustError> {
    let invalid = host.is_empty()
        || host.starts_with('-')
        || host.contains(char::is_whitespace)
        || host.contains('\0');
    if invalid {
        return Err(TrustError::InvalidHost(host.to_string()));
    }
    Ok(())
}

/// How a host is written in `known_hosts`: bare, or `[host]:port` off 22.
pub fn host_spec(host: &str, port: Option<u16>) -> String {
    match port {
        Some(p) if p != DEFAULT_SSH_PORT => format!("[{host}]:{p}"),
        _ => host.to_string(),
    }
}

/// One host key offered by a server, with the fingerprint to show a user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKey {
    /// The verbatim `known_hosts` line, exactly as `ssh-keyscan` produced it.
    /// Stored rather than rebuilt so what gets trusted is what was seen.
    pub entry: String,
    /// e.g. `ED25519`, `RSA`.
    pub key_type: String,
    /// e.g. `SHA256:j/c5fxd5A9lQ0Z12kcBhwv6odCbV/scgeD+lC2rKReg`. This is the
    /// string a user compares against what the server's owner told them.
    pub fingerprint: String,
}

/// Fetch the host keys a server offers, for confirmation before trusting.
///
/// This deliberately does **not** write anything: seeing a key and trusting it
/// are separate steps, and only [`trust`] performs the second.
pub fn scan_host(host: &str, port: Option<u16>) -> Result<Vec<HostKey>, TrustError> {
    validate_host(host)?;

    let mut cmd = Command::new("ssh-keyscan");
    cmd.arg("-T").arg(SCAN_TIMEOUT_SECS.to_string());
    if let Some(p) = port {
        cmd.arg("-p").arg(p.to_string());
    }
    cmd.arg(host);

    let out = cmd
        .output()
        .map_err(|e| TrustError::ScanFailed(format!("could not run ssh-keyscan: {e}")))?;

    // ssh-keyscan puts progress and errors on stderr and keys on stdout, and
    // exits 0 even when it found nothing — so the key lines are the only
    // reliable signal of success.
    let keys: Vec<HostKey> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|line| {
            fingerprint_of(line).map(|(t, fp)| HostKey {
                entry: line.to_string(),
                key_type: t,
                fingerprint: fp,
            })
        })
        .collect();

    if keys.is_empty() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            TrustError::NoKeys(host_spec(host, port))
        } else {
            TrustError::ScanFailed(stderr)
        });
    }
    Ok(keys)
}

/// Fingerprint one `known_hosts` line via `ssh-keygen -lf -`, returning
/// `(key_type, fingerprint)`. `None` if the line is not a key ssh recognises.
///
/// Piped through stdin rather than a temp file: the line is untrusted input
/// straight off the network, and this way it never touches the filesystem.
fn fingerprint_of(line: &str) -> Option<(String, String)> {
    use std::io::Write;

    let mut child = Command::new("ssh-keygen")
        .args(["-l", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.as_mut()?.write_all(line.as_bytes()).ok()?;
    let out = child.wait_with_output().ok()?;
    if !out.status.success() {
        return None;
    }

    // "256 SHA256:<base64> <comment> (ED25519)"
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.lines().next()?;
    let fields: Vec<&str> = first.split_whitespace().collect();
    let fingerprint = fields.get(1)?.to_string();
    let key_type = fields
        .last()?
        .trim_start_matches('(')
        .trim_end_matches(')')
        .to_string();
    Some((key_type, fingerprint))
}

/// Is this host already confirmed in our `known_hosts`?
///
/// Asks `ssh-keygen -F`, which is authoritative about the file's format —
/// including hashed entries, which a hand-rolled parser would silently miss and
/// then re-prompt for a host the user already trusted.
pub fn is_trusted(known_hosts: &Path, host: &str, port: Option<u16>) -> bool {
    if validate_host(host).is_err() || !known_hosts.exists() {
        return false;
    }
    Command::new("ssh-keygen")
        .arg("-F")
        .arg(host_spec(host, port))
        .arg("-f")
        .arg(known_hosts)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Append confirmed keys to `known_hosts`, creating the file and its directory.
///
/// Appends rather than replaces: a host may legitimately offer several key
/// types, and other hosts already trusted must survive untouched.
pub fn trust(known_hosts: &Path, keys: &[HostKey]) -> std::io::Result<()> {
    use std::io::Write;

    if let Some(dir) = known_hosts.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(known_hosts)?;
    for key in keys {
        writeln!(file, "{}", key.entry)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh_keygen_available() -> bool {
        Command::new("ssh-keygen")
            .arg("-?")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok()
    }

    /// The four cases of rsync's quoting, which has no escape hatch.
    #[test]
    fn quoting_covers_what_rsync_can_express_and_refuses_what_it_cannot() {
        // No quotes at all, and a space: single-quoted, one token.
        assert_eq!(quote_for_rsh("/a b/kh").unwrap(), "'/a b/kh'");
        // A double quote inside is literal within single quotes.
        assert_eq!(quote_for_rsh("/a\"b/kh").unwrap(), "'/a\"b/kh'");
        // An apostrophe forces double quotes.
        assert_eq!(quote_for_rsh("/a'b/kh").unwrap(), "\"/a'b/kh\"");
        // Both: rsync has no escape, so this is refused rather than mangled.
        assert_eq!(quote_for_rsh("/a'b\"c/kh"), None);
    }

    /// A port only appears when it is not 22, and it must match the port the
    /// host was confirmed on — the two have to agree or strict checking fails.
    #[test]
    fn the_port_appears_only_when_it_is_not_the_default() {
        let p = Path::new("/cfg/known_hosts");
        assert!(!rsh_command(p, None).unwrap().contains(" -p "));
        assert!(!rsh_command(p, Some(22)).unwrap().contains(" -p "));
        assert!(rsh_command(p, Some(2222)).unwrap().ends_with(" -p 2222"));
    }

    #[test]
    fn the_rsh_command_pins_our_trust_policy() {
        let cmd = rsh_command(Path::new("/cfg/foresight/known_hosts"), None).unwrap();
        assert!(cmd.starts_with("ssh "));
        assert!(cmd.contains("-o 'UserKnownHostsFile=/cfg/foresight/known_hosts'"));
        // Trust must not fall back to the system file or the user's own.
        assert!(cmd.contains("GlobalKnownHostsFile=/dev/null"));
        // accept-new would be trust-on-first-use without asking.
        assert!(cmd.contains("StrictHostKeyChecking=yes"));
        assert!(!cmd.contains("accept-new"));
        // No terminal here: a prompt would hang the transfer, not ask anyone.
        assert!(cmd.contains("BatchMode=yes"));
    }

    /// A home directory with a space in it must still produce a working
    /// command — the case that made the quoting rules worth measuring.
    #[test]
    fn a_spaced_config_path_survives_as_one_token() {
        let cmd = rsh_command(Path::new("/home/a b/.config/foresight/known_hosts"), None).unwrap();
        assert!(cmd.contains("-o 'UserKnownHostsFile=/home/a b/.config/foresight/known_hosts'"));
    }

    #[test]
    fn an_inexpressible_path_is_an_error_not_a_broken_command() {
        let err = rsh_command(Path::new("/home/o'\"/known_hosts"), None).unwrap_err();
        assert!(matches!(err, TrustError::UnquotablePath(_)));
        // The message has to name the actual problem; this one reaches a user.
        assert!(err.to_string().contains("single and a double quote"));
    }

    #[test]
    fn hosts_that_would_become_arguments_are_refused() {
        for bad in ["", "-oProxyCommand=x", "a b", "host\0name", "-"] {
            assert!(
                validate_host(bad).is_err(),
                "{bad:?} should be rejected outright"
            );
        }
        for good in [
            "example.com",
            "192.168.1.5",
            "fe80::1%wlo1",
            "user-host.local",
        ] {
            assert!(validate_host(good).is_ok(), "{good:?} should be accepted");
        }
    }

    /// ssh brackets a host only when the port is non-default, and our lookups
    /// must match the format ssh itself writes or we re-prompt forever.
    #[test]
    fn host_spec_brackets_only_a_non_default_port() {
        assert_eq!(host_spec("example.com", None), "example.com");
        assert_eq!(host_spec("example.com", Some(22)), "example.com");
        assert_eq!(host_spec("example.com", Some(2222)), "[example.com]:2222");
    }

    /// The round trip that matters: an untrusted host is untrusted, a trusted
    /// one is found, and trusting a second host leaves the first alone.
    #[test]
    fn trusting_a_host_is_persistent_and_additive() {
        if !ssh_keygen_available() {
            eprintln!("skipping: ssh-keygen not on PATH");
            return;
        }
        let dir = std::env::temp_dir().join(format!("foresight-kh-{}", std::process::id()));
        let path = dir.join("known_hosts");
        let _ = std::fs::remove_dir_all(&dir);

        // A real key so ssh-keygen parses the file rather than rejecting it.
        let key = "AAAAC3NzaC1lZDI1NTE5AAAAIGb9Ry1nBmJ7dJHRLL3JTfLzTNTQfFbqrqM0oJhqPqPs";
        let first = HostKey {
            entry: format!("[example.com]:2222 ssh-ed25519 {key}"),
            key_type: "ED25519".into(),
            fingerprint: "SHA256:irrelevant".into(),
        };
        let second = HostKey {
            entry: format!("other.local ssh-ed25519 {key}"),
            key_type: "ED25519".into(),
            fingerprint: "SHA256:irrelevant".into(),
        };

        assert!(
            !is_trusted(&path, "example.com", Some(2222)),
            "a missing file trusts nothing"
        );

        trust(&path, std::slice::from_ref(&first)).unwrap();
        assert!(is_trusted(&path, "example.com", Some(2222)));
        // Same host, different port, is a different trust decision.
        assert!(!is_trusted(&path, "example.com", Some(2223)));
        assert!(!is_trusted(&path, "example.com", None));

        trust(&path, std::slice::from_ref(&second)).unwrap();
        assert!(is_trusted(&path, "other.local", None));
        assert!(
            is_trusted(&path, "example.com", Some(2222)),
            "trusting a second host must not disturb the first"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_host_we_would_not_scan_is_never_reported_as_trusted() {
        assert!(!is_trusted(
            Path::new("/nonexistent/known_hosts"),
            "-oX=1",
            None
        ));
    }
}
