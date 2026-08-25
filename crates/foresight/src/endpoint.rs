//! Remote endpoints — the `[user@]host:path` operand rsync takes for a machine
//! on the other end of an ssh connection (Milestone 5, part 3).
//!
//! The UI collects four separate fields (user, host, port, path) rather than
//! one string, because a port is *not* part of rsync's operand at all: it rides
//! in the ssh command via `-e ssh -p N` (see [`crate::ssh::rsh_command`]), and
//! it also changes how the host is keyed in `known_hosts`. Parsing a pasted
//! `user@host:/path` back into those fields is offered as a convenience, not as
//! the storage format.
//!
//! ## IPv6
//!
//! An address containing colons must be **bracketed** in the operand, or the
//! first colon reads as the host/path separator. Measured against the bundled
//! rsync 3.4.4: it accepts `user@[fe80::1%wlo1]:/path`, strips the brackets,
//! and hands `fe80::1%wlo1` — scope id intact — to ssh. Link-local addresses
//! with a `%scope` are a real case here: syncing to a phone over a hotspot is
//! sometimes IPv6-only (see the project notes), so the scope id has to survive
//! from the entry field all the way into argv.

use std::fmt;

/// A remote side of a transfer, as the user described it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Endpoint {
    /// Remote login. `None` means "whatever ssh would use", i.e. omit `user@`.
    pub user: Option<String>,
    /// Bare host — never bracketed in this field, even for IPv6. Bracketing is
    /// a property of the *operand*, applied by [`Endpoint::operand`].
    pub host: String,
    /// `None` or 22 means the default; anything else reaches ssh via `-p`.
    pub port: Option<u16>,
    /// Remote path, verbatim. May be empty, which rsync reads as the login's
    /// home directory.
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointError {
    MissingHost,
    /// No `:` at all — the string names no path, so it is not an operand.
    MissingPathSeparator,
    InvalidHost(String),
    InvalidUser(String),
    UnclosedBracket,
    InvalidPort(String),
}

impl fmt::Display for EndpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use crate::i18n::{i18n, i18n_f};
        match self {
            Self::MissingHost => write!(f, "{}", i18n("no host given")),
            Self::MissingPathSeparator => write!(
                f,
                "{}",
                i18n(
                    "a remote location needs a colon before the path, as in \
                     user@host:/path"
                )
            ),
            Self::InvalidHost(h) => {
                write!(f, "{}", i18n_f("“{}” is not a valid host name", &[h]))
            }
            Self::InvalidUser(u) => {
                write!(f, "{}", i18n_f("“{}” is not a valid user name", &[u]))
            }
            Self::UnclosedBracket => write!(
                f,
                "{}",
                i18n("the [ before an IPv6 address is never closed")
            ),
            Self::InvalidPort(p) => {
                write!(f, "{}", i18n_f("“{}” is not a port number", &[p]))
            }
        }
    }
}

/// Reject anything that would stop being one field once it reaches a command
/// line. Leading `-` would be read as an option; whitespace would split.
fn valid_component(s: &str) -> bool {
    !s.is_empty() && !s.starts_with('-') && !s.contains(char::is_whitespace) && !s.contains('\0')
}

impl Endpoint {
    /// Does this host need bracketing in an operand? Only IPv6 does, and the
    /// tell is a colon — which is exactly the character that would otherwise be
    /// read as the host/path separator.
    fn needs_brackets(&self) -> bool {
        self.host.contains(':')
    }

    /// The host as it appears in an operand: bracketed for IPv6, bare
    /// otherwise.
    pub fn host_for_operand(&self) -> String {
        if self.needs_brackets() {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        }
    }

    /// The rsync operand: `[user@]host:path`.
    ///
    /// Passed to rsync **verbatim** — in particular no trailing slash is ever
    /// appended. For a local folder that slash is what distinguishes "the
    /// folder" from "its contents", but a remote path is typed by the user, so
    /// adding one would silently change what they asked for.
    pub fn operand(&self) -> String {
        let user = match &self.user {
            Some(u) => format!("{u}@"),
            None => String::new(),
        };
        format!("{user}{}:{}", self.host_for_operand(), self.path)
    }

    /// Validate the fields as a set. Called before an endpoint is accepted, so
    /// nothing half-formed reaches argv or a `known_hosts` lookup.
    pub fn validate(&self) -> Result<(), EndpointError> {
        if self.host.is_empty() {
            return Err(EndpointError::MissingHost);
        }
        if !valid_component(&self.host) {
            return Err(EndpointError::InvalidHost(self.host.clone()));
        }
        if let Some(u) = &self.user {
            if !valid_component(u) || u.contains('@') {
                return Err(EndpointError::InvalidUser(u.clone()));
            }
        }
        Ok(())
    }

    /// Parse a pasted `[user@]host:path`, with IPv6 bracketed.
    ///
    /// The port is deliberately not parsed out of the string: `host:2222/path`
    /// is not rsync syntax, and guessing that a numeric first path segment
    /// meant a port would misread the perfectly ordinary path `host:/2222`.
    pub fn parse(spec: &str) -> Result<Self, EndpointError> {
        let spec = spec.trim();

        // user@ — split on the FIRST '@'. An IPv6 address contains no '@', and
        // a remote path legitimately can, so anything after the first one
        // belongs to the host/path half.
        let (user, rest) = match spec.split_once('@') {
            Some((u, rest)) => (Some(u.to_string()), rest),
            None => (None, spec),
        };

        let (host, path) = if let Some(after) = rest.strip_prefix('[') {
            // Bracketed IPv6: the host runs to the closing bracket, and the
            // separator is the colon immediately after it.
            let (inside, tail) = after
                .split_once(']')
                .ok_or(EndpointError::UnclosedBracket)?;
            let path = tail
                .strip_prefix(':')
                .ok_or(EndpointError::MissingPathSeparator)?;
            (inside.to_string(), path.to_string())
        } else {
            let (h, p) = rest
                .split_once(':')
                .ok_or(EndpointError::MissingPathSeparator)?;
            (h.to_string(), p.to_string())
        };

        let endpoint = Self {
            user,
            host,
            port: None,
            path,
        };
        endpoint.validate()?;
        Ok(endpoint)
    }
}

/// Shown in the source/destination row. Includes the port, which the operand
/// cannot carry — so what the row says is the whole truth about where this
/// goes, not just the part rsync happens to take.
impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(u) = &self.user {
            write!(f, "{u}@")?;
        }
        write!(f, "{}", self.host_for_operand())?;
        if let Some(p) = self.port.filter(|p| *p != 22) {
            write!(f, " port {p}")?;
        }
        write!(f, ":{}", self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ep(user: Option<&str>, host: &str, path: &str) -> Endpoint {
        Endpoint {
            user: user.map(str::to_string),
            host: host.to_string(),
            port: None,
            path: path.to_string(),
        }
    }

    #[test]
    fn an_operand_is_user_host_colon_path() {
        assert_eq!(
            ep(Some("miguel"), "nas.local", "/srv/backup").operand(),
            "miguel@nas.local:/srv/backup"
        );
        assert_eq!(
            ep(None, "nas.local", "/srv/backup").operand(),
            "nas.local:/srv/backup"
        );
    }

    /// An empty path is rsync's "the login's home directory", not an error.
    #[test]
    fn an_empty_path_still_makes_an_operand() {
        assert_eq!(ep(Some("m"), "h", "").operand(), "m@h:");
    }

    /// The case the phone sync actually needs: a link-local address keeps its
    /// scope id, and the brackets that stop the first colon being read as the
    /// path separator.
    #[test]
    fn ipv6_is_bracketed_and_keeps_its_scope_id() {
        let e = ep(Some("u"), "fe80::1%wlo1", "/data");
        assert_eq!(e.operand(), "u@[fe80::1%wlo1]:/data");
        assert_eq!(e.host_for_operand(), "[fe80::1%wlo1]");
        // The stored host stays bare — bracketing belongs to the operand, and
        // known_hosts lookups need the unbracketed form.
        assert_eq!(e.host, "fe80::1%wlo1");
    }

    #[test]
    fn ipv4_and_names_are_never_bracketed() {
        assert_eq!(ep(None, "192.168.1.5", "/d").operand(), "192.168.1.5:/d");
        assert_eq!(ep(None, "host.local", "/d").operand(), "host.local:/d");
    }

    #[test]
    fn parsing_round_trips_an_operand() {
        for spec in [
            "miguel@nas.local:/srv/backup",
            "nas.local:/srv/backup",
            "u@[fe80::1%wlo1]:/data",
            "[2001:db8::5]:/data",
            "host:",
        ] {
            let parsed = Endpoint::parse(spec).unwrap_or_else(|e| panic!("{spec}: {e}"));
            assert_eq!(parsed.operand(), spec, "round trip of {spec}");
        }
    }

    /// A path may contain '@' and ':'; neither may be mistaken for a delimiter
    /// after the first one has done its job.
    #[test]
    fn delimiters_inside_a_path_are_just_path() {
        let e = Endpoint::parse("u@host:/srv/a@b:c").unwrap();
        assert_eq!(e.user.as_deref(), Some("u"));
        assert_eq!(e.host, "host");
        assert_eq!(e.path, "/srv/a@b:c");
    }

    #[test]
    fn a_string_with_no_colon_is_not_a_remote_location() {
        assert_eq!(
            Endpoint::parse("just-a-host"),
            Err(EndpointError::MissingPathSeparator)
        );
        // A local path must never be mistaken for one either.
        assert_eq!(
            Endpoint::parse("/home/me/Documents"),
            Err(EndpointError::MissingPathSeparator)
        );
    }

    #[test]
    fn malformed_specs_are_refused_with_the_reason() {
        assert_eq!(
            Endpoint::parse("u@[fe80::1:/data"),
            Err(EndpointError::UnclosedBracket)
        );
        assert_eq!(Endpoint::parse(":/data"), Err(EndpointError::MissingHost));
        assert!(matches!(
            Endpoint::parse("-oProxyCommand=x:/data"),
            Err(EndpointError::InvalidHost(_))
        ));
        assert!(matches!(
            Endpoint::parse("a b@host:/data"),
            Err(EndpointError::InvalidUser(_))
        ));
    }

    /// `host:2222/path` is not rsync syntax, so a numeric segment must stay
    /// part of the path rather than being guessed at as a port.
    #[test]
    fn a_numeric_path_segment_is_not_a_port() {
        let e = Endpoint::parse("host:/2222/data").unwrap();
        assert_eq!(e.port, None);
        assert_eq!(e.path, "/2222/data");
    }

    /// The row text has to state the port, because the operand cannot.
    #[test]
    fn display_shows_a_non_default_port() {
        let mut e = ep(Some("m"), "nas.local", "/srv");
        assert_eq!(e.to_string(), "m@nas.local:/srv");
        e.port = Some(22);
        assert_eq!(e.to_string(), "m@nas.local:/srv", "22 is not worth saying");
        e.port = Some(2222);
        assert_eq!(e.to_string(), "m@nas.local port 2222:/srv");
    }
}
