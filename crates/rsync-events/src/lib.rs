//! rsync-events — parse rsync 3.4.x output into structured events.
//!
//! The pure, UI-free core of a GTK4/libadwaita rsync frontend. No GTK
//! dependencies here, ever: this crate must stay testable on any host.
//!
//! The app must invoke the *bundled* rsync with this exact reporting contract:
//!
//! ```text
//! rsync -a --info=progress2 --out-format='%i %n%L' SRC/ DST/   # real run
//! rsync -a -n -i --delete SRC/ DST/                            # dry-run preview
//! ```
//!
//! Pinning the bundled rsync version pins these formats; this crate is tested
//! against transcripts captured from rsync 3.4.4 (see `tests/fixtures/` at
//! the workspace root). A Python reference implementation with identical
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

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Change(ItemizedChange),
    Progress(Progress),
    Message(Message),
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

static ERROR_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^rsync(:| error:)").unwrap());

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
        Some(Event::Message(Message {
            text: line.trim_end().to_string(),
            is_error: ERROR_RE.is_match(line),
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
