//! rsync-events — parse rsync 3.4.x / 3.5.x output into structured events.
//!
//! The pure, UI-free core of a GTK4/libadwaita rsync frontend. No GTK
//! dependencies here, ever: this crate must stay testable on any host.
//!
//! The app must invoke the *bundled* rsync with this exact reporting contract:
//!
//! ```text
//! rsync -a --info=progress2 --out-format='%i %n%L' SRC DST   # real run
//! rsync -a -n -i --delete SRC DST                            # dry-run preview
//! ```
//!
//! A preview that carries filter rules also gets `--debug=FILTER`, whose
//! `[sender] hiding …` lines become [`Event::Filter`] — the evidence of which
//! rules matched anything. They are extra lines, never a change to the two
//! formats above.
//!
//! Whether `SRC` carries a trailing `/` is the app's choice and does not affect
//! this contract — it only shifts where the paths in [`ItemizedChange`] are
//! rooted (`SRC` yields `dir/file`, `SRC/` yields `file`). The event *format* is
//! identical either way.
//!
//! The app may prepend optional user flags (`-v`, `--bwlimit`, `--exclude`,
//! `--remove-source-files`, free-form extra args) *before* these reporting
//! flags. Those change which files move, how fast, or how chatty rsync is —
//! never the `%i %n%L` / `--info=progress2` output *format*. `-v` only adds
//! extra informational lines (file list, names, stats trailer); those don't
//! match the itemize or progress patterns, so they fall through to
//! [`Message`] events. The reporting flags are always emitted last so user
//! input can't override the contract. See `foresight::job::Job::build_argv`.
//!
//! Pinning the bundled rsync version pins these formats; this crate is tested
//! against transcripts captured from rsync 3.4.4, plus a filter-debug one from
//! 3.5.0, the version now bundled (see `tests/fixtures/` at the workspace
//! root). The 3.4.4 → 3.5.0 bump changed none of these formats: a fresh 3.5.0
//! capture differs only in timestamps, byte counts and temp paths. A Python reference implementation with identical
//! semantics lives in `reference/rsync_events.py`.
//!
//! Typical wiring (gtk-rs): read stdout chunks from `gio::Subprocess` on the
//! main context, push each chunk through [`StreamParser::feed`], dispatch the
//! returned events to your widgets, and call [`StreamParser::finish`] at EOF.

use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::BTreeSet;

// ---------------------------------------------------------------------------
// Event types
// ---------------------------------------------------------------------------

/// UI-level grouping for the dry-run preview list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    Created,
    Updated,
    /// Metadata-only change (perms/owner/times/…).
    Attrs,
    Deleted,
    Unchanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    File,
    Directory,
    Symlink,
    Device,
    Special,
    Unknown,
}

impl FileKind {
    fn from_char(c: char) -> Self {
        match c {
            'f' => Self::File,
            'd' => Self::Directory,
            'L' => Self::Symlink,
            'D' => Self::Device,
            'S' => Self::Special,
            _ => Self::Unknown,
        }
    }
}

/// Attribute names for itemize positions 2..=10 in the `YXcstpoguax` string.
const ATTR_NAMES: [&str; 9] = [
    "checksum", "size", "mtime", "perms", "owner", "group", "atime", "acl", "xattr",
];

/// One `%i %n%L` line, e.g. `>f.s....... readme.txt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemizedChange {
    /// The 11-char `YXcstpoguax` field, or `*deleting` for deletions.
    pub raw_flags: String,
    pub path: String,
    /// From `%L`: the `name -> target` arrow on symlinks.
    pub link_target: Option<String>,
    pub deleted: bool,
}

impl ItemizedChange {
    pub fn file_kind(&self) -> FileKind {
        if self.deleted {
            // rsync doesn't say what kind of thing it deletes
            return FileKind::Unknown;
        }
        self.raw_flags
            .chars()
            .nth(1)
            .map(FileKind::from_char)
            .unwrap_or(FileKind::Unknown)
    }

    pub fn is_new(&self) -> bool {
        !self.deleted && self.raw_flags.len() >= 3 && self.raw_flags[2..].starts_with('+')
    }

    /// Which attributes differ (empty for creations and deletions).
    pub fn changed_attrs(&self) -> BTreeSet<&'static str> {
        let mut out = BTreeSet::new();
        if self.deleted || self.is_new() {
            return out;
        }
        for (i, ch) in self.raw_flags.chars().enumerate().skip(2).take(9) {
            if !matches!(ch, '.' | '+' | ' ') {
                out.insert(ATTR_NAMES[i - 2]);
            }
        }
        out
    }

    pub fn kind(&self) -> ChangeKind {
        if self.deleted {
            return ChangeKind::Deleted;
        }
        if self.is_new() {
            return ChangeKind::Created;
        }
        let attrs = self.changed_attrs();
        if attrs.is_empty() {
            return ChangeKind::Unchanged;
        }
        // content changed if checksum or size differ; otherwise metadata-only
        if attrs.contains("checksum") || attrs.contains("size") {
            ChangeKind::Updated
        } else {
            ChangeKind::Attrs
        }
    }
}

/// One `--info=progress2` update. NOTE: these arrive terminated by `\r`,
/// not `\n` — [`StreamParser`] handles that framing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    pub bytes_done: u64,
    pub percent: u8,
    /// e.g. `"247.96MB/s"` — display as-is.
    pub rate_human: String,
    /// e.g. `"0:00:12"`.
    pub elapsed: String,
    /// From `(xfr#N, …)`.
    pub xfr_index: Option<u32>,
    /// `"to-chk"` or `"ir-chk"` (still scanning).
    pub check_phase: Option<String>,
    pub check_remaining: Option<u64>,
    pub check_total: Option<u64>,
}

impl Progress {
    /// True while incremental recursion is still enumerating files —
    /// totals are still growing; show a "scanning…" state.
    pub fn scanning(&self) -> bool {
        self.check_phase.as_deref() == Some("ir-chk")
    }
}

/// The `--stats` summary block plus the sent/received trailer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stats {
    pub files_total: Option<u64>,
    pub files_created: Option<u64>,
    pub files_deleted: Option<u64>,
    pub files_transferred: Option<u64>,
    pub total_size: Option<u64>,
    pub transferred_size: Option<u64>,
    pub bytes_sent: Option<u64>,
    pub bytes_received: Option<u64>,
    pub speedup: Option<f64>,
}

/// Anything we don't structure: rsync warnings/errors, verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub text: String,
    pub is_error: bool,
}

/// What a filter rule did to one path, as `--debug=FILTER` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterAction {
    /// An exclude kept the sender from seeing the path — it will not transfer.
    Hiding,
    /// An include let the sender see it.
    Showing,
    /// An exclude shielded a destination path from `--delete`.
    Protecting,
    /// An include exposed a destination path to `--delete`.
    Risking,
}

impl FilterAction {
    /// Whether the rule behind this was an exclude (as opposed to an include).
    pub fn is_exclude(self) -> bool {
        matches!(self, Self::Hiding | Self::Protecting)
    }
}

/// One `--debug=FILTER` line: a rule matched a path.
///
/// rsync stops at the first rule that matches a path, so these lines are the
/// only evidence of which rules *did* anything — and a rule that never appears
/// matched nothing, which is the whole reason to collect them. `pattern` is
/// the rule exactly as it was given on the command line (rsync echoes it
/// verbatim, leading `/` and trailing `/` included), so it can be compared
/// with the argv that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterMatch {
    pub action: FilterAction,
    pub is_dir: bool,
    pub path: String,
    pub pattern: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Change(ItemizedChange),
    Progress(Progress),
    Message(Message),
    Filter(FilterMatch),
}

// ---------------------------------------------------------------------------
// Line parsers
// ---------------------------------------------------------------------------

static ITEMIZE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"^(?P<flags>[<>ch.*][fdLDS+?][.+cstpoguaxbn?+ ]{9}) (?P<path>.*?)(?: -> (?P<target>.*))?$",
    )
    .unwrap()
});

static DELETING_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\*deleting\s+(?P<path>.*)$").unwrap());

static PROGRESS_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"^\s*(?P<bytes>[\d,]+)\s+(?P<pct>\d+)%\s+(?P<rate>[\d.,]+\S+/s)\s+(?P<elapsed>[\d:]+)(?:\s+\(xfr#(?P<xfr>\d+),\s+(?P<phase>to-chk|ir-chk)=(?P<rem>\d+)/(?P<tot>\d+)\))?\s*$",
    )
    .unwrap()
});

/// `[sender] hiding directory Photos/private because of pattern private`.
///
/// `path` is greedy so the split lands on the *last* " because of pattern ":
/// a path may contain anything, and of the two a pattern holding that phrase
/// is the less likely.
static FILTER_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"^\[(?:sender|generator|receiver|server|client)\] (?P<action>hiding|showing|protecting|risking) (?P<kind>file|directory) (?P<path>.*) because of pattern (?P<pattern>.*)$",
    )
    .unwrap()
});

static ERROR_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^rsync(:| error:)").unwrap());

/// ssh's own diagnostics, which share the stream for a remote transfer.
///
/// These carry the *reason* a remote job failed. rsync only ever says
/// "unexplained error (code 255)" — the useful line ("Permission denied",
/// "Host key verification failed", "REMOTE HOST IDENTIFICATION HAS CHANGED")
/// comes from ssh and does not start with `rsync:`. Without this they reached
/// the activity log but never the collected-errors list behind the result
/// banner, so a failed remote sync reported that something went wrong and not
/// one word about what.
///
/// Deliberately a short list of known-fatal lines rather than anything
/// resembling "looks scary": ssh's routine chatter (`Warning: Permanently
/// added …`, the `@@@@` banner rule that decorates the host-key warning) is
/// *not* an error, and promoting it would put noise in front of the real cause.
static SSH_ERROR_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?x)
        ^(
            Host\ key\ verification\ failed
            # Distinguishes a CHANGED key from a merely unknown one — the
            # difference between first contact and someone may be in the
            # middle — so it must not be dropped as noise.
          | Host\ key\ for\ .+\ has\ changed
          | No\ .+\ host\ key\ is\ known\ for
          | Permission\ denied
          | ssh:
          | Connection\ closed\ by
          | Connection\ timed\ out
          | kex_exchange_identification:
          | Bad\ configuration\ option:
        )
        # Unanchored on purpose: ssh prints this padded inside its @-banner,
        # as `@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @`.
        # Anchoring it — which is the obvious thing to write — silently misses
        # the single loudest line ssh has.
        | WARNING:\ REMOTE\ HOST\ IDENTIFICATION\ HAS\ CHANGED
        # `user@host: Permission denied (publickey).` — the reason a key-based
        # login was refused, which is the single most likely remote failure.
        | :\ Permission\ denied\ \(
        ",
    )
    .unwrap()
});

fn parse_u64(s: &str) -> u64 {
    s.replace(',', "").parse().unwrap_or(0)
}

pub fn parse_itemize_line(line: &str) -> Option<ItemizedChange> {
    if let Some(c) = DELETING_RE.captures(line) {
        return Some(ItemizedChange {
            raw_flags: "*deleting".into(),
            path: c["path"].to_string(),
            link_target: None,
            deleted: true,
        });
    }
    ITEMIZE_RE.captures(line).map(|c| ItemizedChange {
        raw_flags: c["flags"].to_string(),
        path: c["path"].to_string(),
        link_target: c.name("target").map(|m| m.as_str().to_string()),
        deleted: false,
    })
}

pub fn parse_filter_line(line: &str) -> Option<FilterMatch> {
    FILTER_RE.captures(line).map(|c| FilterMatch {
        action: match &c["action"] {
            "hiding" => FilterAction::Hiding,
            "showing" => FilterAction::Showing,
            "protecting" => FilterAction::Protecting,
            _ => FilterAction::Risking,
        },
        is_dir: &c["kind"] == "directory",
        path: c["path"].to_string(),
        pattern: c["pattern"].to_string(),
    })
}

pub fn parse_progress_line(line: &str) -> Option<Progress> {
    PROGRESS_RE.captures(line).map(|c| Progress {
        bytes_done: parse_u64(&c["bytes"]),
        percent: c["pct"].parse().unwrap_or(0),
        rate_human: c["rate"].to_string(),
        elapsed: c["elapsed"].to_string(),
        xfr_index: c.name("xfr").and_then(|m| m.as_str().parse().ok()),
        check_phase: c.name("phase").map(|m| m.as_str().to_string()),
        check_remaining: c.name("rem").map(|m| parse_u64(m.as_str())),
        check_total: c.name("tot").map(|m| parse_u64(m.as_str())),
    })
}

pub fn parse_stats_block(text: &str) -> Stats {
    static PATS: Lazy<Vec<(&str, Regex)>> = Lazy::new(|| {
        vec![
            (
                "files_total",
                Regex::new(r"^Number of files: ([\d,]+)").unwrap(),
            ),
            (
                "files_created",
                Regex::new(r"^Number of created files: ([\d,]+)").unwrap(),
            ),
            (
                "files_deleted",
                Regex::new(r"^Number of deleted files: ([\d,]+)").unwrap(),
            ),
            (
                "files_transferred",
                Regex::new(r"^Number of regular files transferred: ([\d,]+)").unwrap(),
            ),
            (
                "total_size",
                Regex::new(r"^Total file size: ([\d,]+) bytes").unwrap(),
            ),
            (
                "transferred_size",
                Regex::new(r"^Total transferred file size: ([\d,]+) bytes").unwrap(),
            ),
            (
                "bytes_sent",
                Regex::new(r"^(?:Total bytes sent|sent) ([\d,]+) bytes").unwrap(),
            ),
            (
                "bytes_received",
                Regex::new(r"received ([\d,]+) bytes").unwrap(),
            ),
        ]
    });
    static SPEEDUP_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"speedup is ([\d.]+)").unwrap());

    let mut st = Stats::default();
    for raw in text.lines() {
        let line = raw.trim();
        for (key, rx) in PATS.iter() {
            // Each pattern is matched (searched, not anchored) against the
            // trimmed line; the sent/received trailer lines are not at column 0.
            if let Some(c) = rx.captures(line) {
                let v = parse_u64(&c[1]);
                let slot = match *key {
                    "files_total" => &mut st.files_total,
                    "files_created" => &mut st.files_created,
                    "files_deleted" => &mut st.files_deleted,
                    "files_transferred" => &mut st.files_transferred,
                    "total_size" => &mut st.total_size,
                    "transferred_size" => &mut st.transferred_size,
                    "bytes_sent" => &mut st.bytes_sent,
                    "bytes_received" => &mut st.bytes_received,
                    _ => unreachable!(),
                };
                if slot.is_none() {
                    *slot = Some(v);
                }
            }
        }
        if let Some(c) = SPEEDUP_RE.captures(line) {
            st.speedup = c[1].parse().ok();
        }
    }
    st
}

// ---------------------------------------------------------------------------
// Streaming parser — feed it raw stdout chunks from gio::Subprocess
// ---------------------------------------------------------------------------

/// Incremental parser: handles the fact that progress updates end in `\r`
/// while everything else ends in `\n`, and that chunk boundaries can fall
/// anywhere — including mid-line and mid-number.
#[derive(Debug, Default)]
pub struct StreamParser {
    buf: String,
}

impl StreamParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a chunk of decoded stdout; returns every completed event.
    pub fn feed(&mut self, chunk: &str) -> Vec<Event> {
        self.buf.push_str(chunk);
        let mut events = Vec::new();
        while let Some(idx) = self.buf.find(['\n', '\r']) {
            let line: String = self.buf.drain(..=idx).collect();
            let line = &line[..line.len() - 1]; // strip the terminator
            if let Some(ev) = Self::parse_line(line) {
                events.push(ev);
            }
        }
        events
    }

    /// Call after EOF to flush a final unterminated line.
    pub fn finish(&mut self) -> Vec<Event> {
        let rest = std::mem::take(&mut self.buf);
        if rest.trim().is_empty() {
            return Vec::new();
        }
        Self::parse_line(&rest).into_iter().collect()
    }

    fn parse_line(line: &str) -> Option<Event> {
        if line.trim().is_empty() {
            return None;
        }
        if let Some(p) = parse_progress_line(line) {
            return Some(Event::Progress(p));
        }
        if let Some(c) = parse_itemize_line(line) {
            return Some(Event::Change(c));
        }
        if let Some(f) = parse_filter_line(line) {
            return Some(Event::Filter(f));
        }
        Some(Event::Message(Message {
            text: line.trim_end().to_string(),
            // A remote job's stream carries ssh's stderr as well as rsync's,
            // and for those failures ssh is the one that says why.
            is_error: ERROR_RE.is_match(line) || SSH_ERROR_RE.is_match(line),
        }))
    }
}

// ---------------------------------------------------------------------------
// Exit-code translation for the UI
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Success,
    /// Exit 23/24/25 — completed with warnings. Normal life for big syncs:
    /// show a warning banner with the collected error Messages attached,
    /// never a failure wall.
    Partial,
    Cancelled,
    Error,
}

/// Map an rsync exit code to a severity and a human-readable message.
pub fn classify_exit(code: i32) -> (Severity, String) {
    use Severity::*;
    let (sev, msg) = match code {
        0 => (Success, "Sync completed."),
        1 => (
            Error,
            "Syntax or usage error — the app built a bad command line.",
        ),
        2 => (Error, "Protocol incompatibility between rsync versions."),
        3 => (
            Error,
            "File selection error — a source or destination is invalid.",
        ),
        5 => (Error, "Error starting the client-server protocol."),
        10 => (
            Error,
            "Socket I/O error — check the network or remote host.",
        ),
        11 => (Error, "File I/O error — check disk space and permissions."),
        12 => (Error, "Protocol data stream error."),
        13 => (Error, "Diagnostics error."),
        14 => (Error, "IPC error."),
        20 => (Cancelled, "Sync was interrupted."),
        23 => (
            Partial,
            "Completed, but some files could not be transferred.",
        ),
        24 => (
            Partial,
            "Completed, but some source files vanished mid-sync.",
        ),
        25 => (Partial, "Stopped early: --max-delete limit reached."),
        30 => (Error, "Timeout waiting for data."),
        35 => (Error, "Timeout waiting for the remote to connect."),
        255 => (
            Error,
            "The remote shell (ssh) failed — check host and keys.",
        ),
        other => return (Error, format!("rsync exited with code {other}.")),
    };
    (sev, msg.to_string())
}

#[cfg(test)]
mod ssh_error_tests {
    use super::*;

    /// Through the public streaming API, exactly as the app consumes it.
    fn is_error(line: &str) -> bool {
        let mut parser = StreamParser::new();
        match parser.feed(&format!("{line}\n")).into_iter().next() {
            Some(Event::Message(m)) => m.is_error,
            other => panic!("{line:?} did not parse as a Message: {other:?}"),
        }
    }

    /// Lines captured **verbatim** from ssh during remote-sync verification —
    /// copied out of a real failing transfer, not transcribed from memory.
    /// Each is the only statement of why a transfer failed; rsync itself
    /// reports nothing better than "unexplained error (code 255)".
    #[test]
    fn ssh_failure_lines_are_collected_as_errors() {
        for line in [
            // Refused key auth (the most likely remote failure of all).
            "definitive_group@127.0.0.1: Permission denied (password,keyboard-interactive).",
            "user@nas.local: Permission denied (publickey).",
            "Permission denied, please try again.",
            // First contact with strict checking on.
            "No ED25519 host key is known for [127.0.0.1]:2222 and you have requested strict checking.",
            // A CHANGED key. Note the padding and the surrounding @ — ssh
            // prints this inside a banner, so an anchored pattern misses it.
            "@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @",
            "Host key for [127.0.0.1]:2222 has changed and you have requested strict checking.",
            "Host key verification failed.",
            // Never reached the host at all.
            "ssh: connect to host nas.local port 22: Connection refused",
            "ssh: Could not resolve hostname nope.invalid: Name or service not known",
            "Connection closed by 127.0.0.1 port 2222",
            "kex_exchange_identification: read: Connection reset by peer",
        ] {
            assert!(is_error(line), "should be collected as an error: {line:?}");
        }
    }

    /// A changed host key must be distinguishable from an unknown one in what
    /// the user is shown: the first means "possible man in the middle", the
    /// second means "you have not been here before".
    #[test]
    fn a_changed_host_key_says_so_and_not_merely_that_it_failed() {
        let changed =
            "Host key for [127.0.0.1]:2222 has changed and you have requested strict checking.";
        assert!(is_error(changed));
        assert!(is_error(
            "@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @"
        ));
    }

    /// ssh is chatty on success too. Promoting its routine lines would bury the
    /// real cause under noise the next time something actually breaks.
    #[test]
    fn routine_ssh_chatter_is_not_an_error() {
        for line in [
            "Warning: Permanently added '[127.0.0.1]:2222' (ED25519) to the list of known hosts.",
            "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@",
            "Authenticated to 127.0.0.1 ([127.0.0.1]:2222) using \"publickey\".",
            "debug1: Reading configuration data /etc/ssh/ssh_config",
        ] {
            assert!(!is_error(line), "should NOT be an error: {line:?}");
        }
    }

    /// The pre-existing rsync classification must be untouched by the addition.
    #[test]
    fn rsync_lines_still_classify_as_before() {
        assert!(is_error(
            "rsync: [sender] link_stat \"/nope\" failed: No such file"
        ));
        assert!(is_error(
            "rsync error: some files could not be transferred (code 23)"
        ));
        assert!(!is_error("sending incremental file list"));
        assert!(!is_error("total size is 1,234  speedup is 5.67"));
    }
}
