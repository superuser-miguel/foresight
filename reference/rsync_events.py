"""rsync_events — parse rsync 3.4.x / 3.5.x output into structured events.

Designed as the pure, UI-free core of a GTK4/libadwaita rsync frontend.

The app must invoke the *bundled* rsync with this exact reporting contract:

    rsync -a --info=progress2 --out-format='%i %n%L' SRC DST     # real run
    rsync -a -n -i --delete SRC DST                              # dry-run preview

Whether SRC carries a trailing "/" is the app's choice and does not affect this
contract — it only shifts where itemized paths are rooted (SRC yields
"dir/file", SRC/ yields "file"). The event format is identical either way.

Pinning the bundled rsync version pins these formats; this module is tested
against captured transcripts from rsync 3.4.4 (see tests/fixtures/).

Stdlib only. No GTK imports here — keep this importable and testable anywhere.

Typical wiring inside the app (GLib main loop):

    parser = StreamParser()
    def on_stdout_chunk(chunk: bytes):
        for event in parser.feed_bytes(chunk):   # as read — do NOT decode
            dispatch(event)          # update progress bars / change list
    ...
    for event in parser.finish():
        dispatch(event)

Bytes in, text out. A read can end inside a multibyte character, and each half
decoded on its own is U+FFFD, so the caller does not decode: the parser splits
the bytes on "\\n" / "\\r" (single bytes that cannot occur inside a UTF-8
sequence) and decodes each completed line once. A line that is not valid UTF-8
is decoded with errors="replace" and still yields its event.

What rsync 3.5.0 writes, measured, without -8: in a UTF-8 locale valid UTF-8
names arrive raw and invalid bytes arrive escaped as \\#ooo (octal); in the
C/POSIX locale every byte above 0x7f is escaped; control characters in a name
(\\#012, \\#015, \\#007 — not tab) are escaped in every locale. The escapes are
passed through as printed. The full account is in the Rust crate's docs.

One line, two events. A progress update is written without a terminator, and
rsync pays the newline it owes before the next thing it prints — to whichever
stream that goes to. At the end of a run that failed, the newline goes to
stdout (buffered, not flushed) and the summary to stderr (at once), so the
merged stream reads "...to-chk=0/8)rsync error: ... (code 23) ...\n". A line
that is nothing as a whole and BEGINS with a complete progress update is
therefore two events: the update, and the text after it classified as the line
of its own it was meant to be. Nothing else about classification changes; see
the Rust crate's docs for the measurements.

Self-check:  python3 reference/rsync_events_selfcheck.py
"""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass, field
from enum import Enum
from typing import Iterator, Optional, Union

__all__ = [
    "ChangeKind", "FileKind", "ItemizedChange", "Progress", "Stats",
    "Message", "StreamParser", "parse_itemize_line", "parse_progress_line",
    "parse_stats_block", "classify_exit",
]

# --------------------------------------------------------------------------
# Event types
# --------------------------------------------------------------------------

class ChangeKind(Enum):
    """UI-level grouping for the dry-run preview list."""
    CREATED = "created"
    UPDATED = "updated"
    DELETED = "deleted"
    ATTRS = "attributes"      # metadata-only change (perms/owner/times)
    UNCHANGED = "unchanged"


class FileKind(Enum):
    FILE = "f"
    DIRECTORY = "d"
    SYMLINK = "L"
    DEVICE = "D"
    SPECIAL = "S"
    UNKNOWN = "?"


#: itemize attribute positions 2..10 in the YXcstpoguax string
_ATTR_NAMES = ("checksum", "size", "mtime", "perms", "owner",
               "group", "atime", "acl", "xattr")


@dataclass(frozen=True)
class ItemizedChange:
    """One `%i %n%L` line, e.g. `>f.s....... readme.txt`."""
    raw_flags: str                       # the 11-char YXcstpoguax field
    path: str
    link_target: Optional[str] = None    # from %L: "name -> target"
    deleted: bool = False

    @property
    def file_kind(self) -> FileKind:
        if self.deleted:
            return FileKind.UNKNOWN      # rsync doesn't say what it deletes
        try:
            return FileKind(self.raw_flags[1])
        except ValueError:
            return FileKind.UNKNOWN

    @property
    def is_new(self) -> bool:
        return not self.deleted and self.raw_flags[2:].startswith("+")

    @property
    def changed_attrs(self) -> frozenset[str]:
        """Which attributes differ (empty for creations and deletions)."""
        if self.deleted or self.is_new:
            return frozenset()
        out = set()
        for pos, name in enumerate(_ATTR_NAMES, start=2):
            if pos < len(self.raw_flags) and self.raw_flags[pos] not in ".+ ":
                out.add(name)
        return frozenset(out)

    @property
    def kind(self) -> ChangeKind:
        if self.deleted:
            return ChangeKind.DELETED
        if self.is_new:
            return ChangeKind.CREATED
        attrs = self.changed_attrs
        if not attrs:
            return ChangeKind.UNCHANGED
        if attrs <= {"mtime", "perms", "owner", "group", "atime", "acl", "xattr"}:
            return ChangeKind.ATTRS
        return ChangeKind.UPDATED        # content changed (checksum/size)


@dataclass(frozen=True)
class Progress:
    """One `--info=progress2` update (arrives after `\\r`, not `\\n`)."""
    bytes_done: int
    percent: int
    rate_human: str                      # e.g. "247.96MB/s"
    elapsed: str                         # e.g. "0:00:12"
    xfr_index: Optional[int] = None      # (xfr#N, ...)
    check_phase: Optional[str] = None    # "to-chk" | "ir-chk" (still scanning)
    check_remaining: Optional[int] = None
    check_total: Optional[int] = None

    @property
    def scanning(self) -> bool:
        """True while incremental recursion is still enumerating files."""
        return self.check_phase == "ir-chk"


@dataclass(frozen=True)
class Stats:
    """The `--stats` summary block plus the sent/received trailer."""
    files_total: Optional[int] = None
    files_created: Optional[int] = None
    files_deleted: Optional[int] = None
    files_transferred: Optional[int] = None
    total_size: Optional[int] = None
    transferred_size: Optional[int] = None
    bytes_sent: Optional[int] = None
    bytes_received: Optional[int] = None
    speedup: Optional[float] = None


@dataclass(frozen=True)
class Message:
    """Anything we don't structure: rsync warnings/errors, verbatim."""
    text: str
    is_error: bool = False


@dataclass(frozen=True)
class FilterMatch:
    """One ``--debug=FILTER`` line: a filter rule matched a path.

    rsync stops at the first rule that matches, so these lines are the only
    evidence of which rules did anything; a rule that never appears matched
    nothing. ``pattern`` is the rule verbatim as given on the command line.
    ``action`` is one of hiding / showing / protecting / risking — hiding and
    protecting come from an exclude, the other two from an include."""
    action: str
    is_dir: bool
    path: str
    pattern: str

    @property
    def is_exclude(self) -> bool:
        return self.action in ("hiding", "protecting")


Event = Union[ItemizedChange, Progress, Stats, Message, FilterMatch]

# --------------------------------------------------------------------------
# Line parsers
# --------------------------------------------------------------------------

_ITEMIZE_RE = re.compile(
    r"^(?P<flags>[<>ch.*][fdLDS+?][.+cstpoguaxbn?+ ]{9})"
    r" (?P<path>.*?)(?: -> (?P<target>.*))?$"
)
_DELETING_RE = re.compile(r"^\*deleting\s+(?P<path>.*)$")

_PROGRESS_RE = re.compile(
    r"^\s*(?P<bytes>[\d,]+)\s+(?P<pct>\d+)%\s+"
    r"(?P<rate>[\d.,]+\S+/s)\s+(?P<elapsed>[\d:]+)"
    r"(?:\s+\(xfr#(?P<xfr>\d+),\s+(?P<phase>to-chk|ir-chk)="
    r"(?P<rem>\d+)/(?P<tot>\d+)\))?\s*$"
)

# A complete progress update at the start of a line that goes on. Stricter than
# _PROGRESS_RE because it has to say where the update ENDS: the elapsed time is
# h:mm:ss exactly, and the update closes with the (xfr#N, to-chk=R/T) trailer
# (and the spaces rsync pads it with) or with the two spaces that end a
# mid-file update. ASCII digits and spaces only, as rsync writes them.
_GLUED_PROGRESS_RE = re.compile(
    r"^ *[0-9,]+ +[0-9]+% +[0-9.,]+[^ ]+/s +[0-9]+:[0-9]{2}:[0-9]{2}"
    r"(?: +\(xfr#[0-9]+, +(?:to-chk|ir-chk)=[0-9]+/[0-9]+\) *|  +)")

# `[sender] hiding directory Photos/private because of pattern private`.
# `path` is greedy so the split lands on the LAST " because of pattern ".
_FILTER_RE = re.compile(
    r"^\[(?:sender|generator|receiver|server|client)\] "
    r"(?P<action>hiding|showing|protecting|risking) (?P<kind>file|directory) "
    r"(?P<path>.*) because of pattern (?P<pattern>.*)$")

_ERROR_RE = re.compile(r"^rsync(:| error:)")

# ssh's own diagnostics, which share the stream on a remote transfer and carry
# the *reason* it failed (rsync itself only says "unexplained error (code
# 255)"). A short list of known-fatal lines, not "anything scary": routine
# chatter such as `Warning: Permanently added …` must NOT be promoted.
_SSH_ERROR_RE = re.compile(
    r"^(Host key verification failed"
    r"|Host key for .+ has changed"
    r"|No .+ host key is known for"
    r"|Permission denied"
    r"|ssh:"
    r"|Connection closed by"
    r"|Connection timed out"
    r"|kex_exchange_identification:"
    r"|Bad configuration option:)"
    # Unanchored on purpose: ssh pads this inside its @-banner.
    r"|WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED"
    # `user@host: Permission denied (publickey).`
    r"|: Permission denied \(")

# rsync's own failure lines that do NOT start with `rsync:` — only rsyserr()
# adds the prefix; plain rprintf() output arrives bare whatever its log level.
# A short closed list, each line produced with rsync 3.5.0 and read back in its
# source. NOT on it, on purpose: `skipping non-regular file`, `skipping
# directory`, `cannot delete non-empty directory` (all seen on exit-0 runs),
# `file has vanished` / `rsync warning:` (exit 24 says it), and `WARNING:`
# (rsync retries). See UNPREFIXED_ERROR_RE in the Rust crate for the reasoning.
_UNPREFIXED_ERROR_RE = re.compile(
    r"^(IO error encountered -- skipping file deletion"
    r"|Deletions stopped due to --max-delete limit"
    r"|ERROR: "
    r"|symlink has no referent:"
    r"|could not make way for )")


def _int(s: str) -> int:
    return int(s.replace(",", ""))


def parse_itemize_line(line: str) -> Optional[ItemizedChange]:
    m = _DELETING_RE.match(line)
    if m:
        return ItemizedChange(raw_flags="*deleting", deleted=True,
                              path=m.group("path"))
    m = _ITEMIZE_RE.match(line)
    if m:
        return ItemizedChange(raw_flags=m.group("flags"),
                              path=m.group("path"),
                              link_target=m.group("target"))
    return None


def parse_filter_line(line: str) -> Optional[FilterMatch]:
    m = _FILTER_RE.match(line)
    if not m:
        return None
    return FilterMatch(action=m["action"], is_dir=m["kind"] == "directory",
                       path=m["path"], pattern=m["pattern"])


def parse_progress_line(line: str) -> Optional[Progress]:
    m = _PROGRESS_RE.match(line)
    if not m:
        return None
    return Progress(
        bytes_done=_int(m.group("bytes")),
        percent=int(m.group("pct")),
        rate_human=m.group("rate"),
        elapsed=m.group("elapsed"),
        xfr_index=int(m.group("xfr")) if m.group("xfr") else None,
        check_phase=m.group("phase"),
        check_remaining=_int(m.group("rem")) if m.group("rem") else None,
        check_total=_int(m.group("tot")) if m.group("tot") else None,
    )


def _split_glued_progress(line: str) -> Optional[tuple[Progress, str]]:
    """A line that BEGINS with a complete progress update and goes on: the
    update, and the text after it. None unless both are there."""
    m = _GLUED_PROGRESS_RE.match(line)
    if not m:
        return None
    update, rest = line[:m.end()], line[m.end():]
    if not rest.strip():
        return None
    progress = parse_progress_line(update)
    if progress is None:
        return None
    return progress, rest


_STATS_PATTERNS = {
    "files_total": re.compile(r"^Number of files: ([\d,]+)"),
    "files_created": re.compile(r"^Number of created files: ([\d,]+)"),
    "files_deleted": re.compile(r"^Number of deleted files: ([\d,]+)"),
    "files_transferred": re.compile(r"^Number of regular files transferred: ([\d,]+)"),
    "total_size": re.compile(r"^Total file size: ([\d,]+) bytes"),
    "transferred_size": re.compile(r"^Total transferred file size: ([\d,]+) bytes"),
    "bytes_sent": re.compile(r"^(?:Total bytes sent|sent) ([\d,]+) bytes"),
    "bytes_received": re.compile(r"received ([\d,]+) bytes"),
}
_SPEEDUP_RE = re.compile(r"speedup is ([\d.]+)")


def parse_stats_block(text: str) -> Stats:
    values: dict = {}
    for line in text.splitlines():
        line = line.strip()
        for key, rx in _STATS_PATTERNS.items():
            m = rx.search(line) if key == "bytes_received" else rx.match(line)
            if m and key not in values:
                values[key] = _int(m.group(1))
        m = _SPEEDUP_RE.search(line)
        if m:
            values["speedup"] = float(m.group(1))
    return Stats(**values)

# --------------------------------------------------------------------------
# Streaming parser — feed it raw stdout chunks from GSubprocess
# --------------------------------------------------------------------------

class StreamParser:
    """Incremental parser: handles the fact that progress updates end in
    ``\\r`` while everything else ends in ``\\n``, and that chunk boundaries
    can fall anywhere — mid-line, mid-number and mid-character.

    It buffers bytes and decodes a line only once the line is complete.
    ``feed_bytes`` is the parser; ``feed`` hands it the bytes of a ``str`` and
    is for text that is already text (a test's literal), never for process
    output."""

    def __init__(self) -> None:
        self._buf = b""

    def feed_bytes(self, chunk: bytes) -> Iterator[Event]:
        """Push a chunk exactly as it was read; yields every event completed
        by it. The events over a whole stream are the same however the stream
        was cut into chunks."""
        self._buf += chunk
        while True:
            # split on whichever terminator comes first
            idx_n = self._buf.find(b"\n")
            idx_r = self._buf.find(b"\r")
            if idx_n == -1 and idx_r == -1:
                return
            if idx_r != -1 and (idx_n == -1 or idx_r < idx_n):
                line, self._buf = self._buf[:idx_r], self._buf[idx_r + 1:]
            else:
                line, self._buf = self._buf[:idx_n], self._buf[idx_n + 1:]
            yield from self._parse_line(line.decode("utf-8", "replace"))

    def feed(self, chunk: str) -> Iterator[Event]:
        """``feed_bytes`` for text that is already text."""
        return self.feed_bytes(chunk.encode("utf-8"))

    def finish(self) -> Iterator[Event]:
        """Call after EOF to flush a final unterminated line."""
        rest, self._buf = self._buf, b""
        yield from self._parse_line(rest.decode("utf-8", "replace"))

    @classmethod
    def _parse_line(cls, line: str) -> list:
        """The events of one line: none for a blank one, one for nearly every
        other, and two for a progress update with a line glued to it."""
        if not line.strip():
            return []
        ev = cls._parse_structured(line)
        if ev is not None:
            return [ev]
        # Only now, and only for a line that is nothing as a whole.
        glued = _split_glued_progress(line)
        if glued is not None:
            progress, rest = glued
            ev = cls._parse_structured(rest)
            return [progress, ev if ev is not None else cls._message(rest)]
        return [cls._message(line)]

    @staticmethod
    def _parse_structured(line: str) -> Optional[Event]:
        """A line that is, whole, one of the formats this module structures."""
        ev: Optional[Event] = parse_progress_line(line)
        if ev is not None:
            return ev
        ev = parse_itemize_line(line)
        if ev is not None:
            return ev
        return parse_filter_line(line)

    @staticmethod
    def _message(line: str) -> Message:
        return Message(text=line.rstrip(),
                       is_error=bool(_ERROR_RE.match(line)
                                     or _UNPREFIXED_ERROR_RE.match(line)
                                     or _SSH_ERROR_RE.search(line)))

# --------------------------------------------------------------------------
# Exit-code translation for the UI
# --------------------------------------------------------------------------

_EXIT_MEANINGS = {
    0: ("success", "Sync completed."),
    1: ("error", "Syntax or usage error — the app built a bad command line."),
    2: ("error", "Protocol incompatibility between rsync versions."),
    3: ("error", "File selection error — a source or destination is invalid."),
    5: ("error", "Error starting the client-server protocol."),
    10: ("error", "Socket I/O error — check the network or remote host."),
    11: ("error", "File I/O error — check disk space and permissions."),
    12: ("error", "Protocol data stream error."),
    13: ("error", "Diagnostics error."),
    14: ("error", "IPC error."),
    20: ("cancelled", "Sync was interrupted."),
    23: ("partial", "Completed, but some files could not be transferred."),
    24: ("partial", "Completed, but some source files vanished mid-sync."),
    25: ("partial", "Stopped early: --max-delete limit reached."),
    30: ("error", "Timeout waiting for data."),
    35: ("error", "Timeout waiting for the remote to connect."),
    255: ("error", "The remote shell (ssh) failed — check host and keys."),
}


def classify_exit(code: int) -> tuple[str, str]:
    """Map an rsync exit code to (severity, human message).

    severity is one of: success | partial | cancelled | error.
    Exit 23/24 are *normal life* for big syncs — the UI must show them as
    warnings with the captured Message events attached, never as failure walls.
    """
    return _EXIT_MEANINGS.get(code, ("error", f"rsync exited with code {code}."))

# --------------------------------------------------------------------------
# CLI: python3 rsync_events.py <captured-output-file>
# --------------------------------------------------------------------------

if __name__ == "__main__":
    parser = StreamParser()
    with open(sys.argv[1], "rb") as fh:
        data = fh.read()
    # simulate arbitrary chunking to prove boundary handling: 7 BYTES at a
    # time, so a multibyte character in the file does get cut
    events = []
    for i in range(0, len(data), 7):
        events.extend(parser.feed_bytes(data[i:i + 7]))
    events.extend(parser.finish())
    for ev in events:
        print(f"{type(ev).__name__:16} {ev}")
