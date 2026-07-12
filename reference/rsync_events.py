"""rsync_events — parse rsync 3.4.x output into structured events.

Designed as the pure, UI-free core of a GTK4/libadwaita rsync frontend.

The app must invoke the *bundled* rsync with this exact reporting contract:

    rsync -a --info=progress2 --out-format='%i %n%L' SRC/ DST/     # real run
    rsync -a -n -i --delete SRC/ DST/                              # dry-run preview

Pinning the bundled rsync version pins these formats; this module is tested
against captured transcripts from rsync 3.4.4 (see tests/fixtures/).

Stdlib only. No GTK imports here — keep this importable and testable anywhere.

Typical wiring inside the app (GLib main loop):

    parser = StreamParser()
    def on_stdout_chunk(chunk: bytes):
        for event in parser.feed(chunk.decode("utf-8", "replace")):
            dispatch(event)          # update progress bars / change list
    ...
    for event in parser.finish():
        dispatch(event)
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


Event = Union[ItemizedChange, Progress, Stats, Message]

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

_ERROR_RE = re.compile(r"^rsync(:| error:)")


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
    can fall anywhere."""

    def __init__(self) -> None:
        self._buf = ""

    def feed(self, chunk: str) -> Iterator[Event]:
        self._buf += chunk
        while True:
            # split on whichever terminator comes first
            idx_n = self._buf.find("\n")
            idx_r = self._buf.find("\r")
            if idx_n == -1 and idx_r == -1:
                return
            if idx_r != -1 and (idx_n == -1 or idx_r < idx_n):
                line, self._buf = self._buf[:idx_r], self._buf[idx_r + 1:]
            else:
                line, self._buf = self._buf[:idx_n], self._buf[idx_n + 1:]
            ev = self._parse_line(line)
            if ev is not None:
                yield ev

    def finish(self) -> Iterator[Event]:
        """Call after EOF to flush a final unterminated line."""
        if self._buf.strip():
            ev = self._parse_line(self._buf)
            if ev is not None:
                yield ev
        self._buf = ""

    @staticmethod
    def _parse_line(line: str) -> Optional[Event]:
        if not line.strip():
            return None
        ev: Optional[Event] = parse_progress_line(line)
        if ev is not None:
            return ev
        ev = parse_itemize_line(line)
        if ev is not None:
            return ev
        return Message(text=line.rstrip(),
                       is_error=bool(_ERROR_RE.match(line)))

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
    with open(sys.argv[1], encoding="utf-8", errors="replace") as fh:
        data = fh.read()
    # simulate arbitrary chunking to prove boundary handling
    events = []
    for i in range(0, len(data), 7):
        events.extend(parser.feed(data[i:i + 7]))
    events.extend(parser.finish())
    for ev in events:
        print(f"{type(ev).__name__:16} {ev}")
