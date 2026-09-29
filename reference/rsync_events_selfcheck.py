"""Self-check for rsync_events.py — where the reads fall must not change what
comes out.

    python3 reference/rsync_events_selfcheck.py

Plain asserts, stdlib only, no test framework; exits non-zero on the first
failure. The cases are the ones in crates/rsync-events/tests/
chunk_boundary_test.rs, with the same inputs and the same expected text.
Change both or neither.
"""

import sys

sys.dont_write_bytecode = True      # leave no __pycache__ beside the spec

from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from rsync_events import (  # noqa: E402
    FilterMatch, ItemizedChange, Message, Progress, StreamParser,
)

FIXTURES = HERE.parent / "tests" / "fixtures"

# Two-, three- and four-byte characters; an itemized line, a symlink with its
# `->` target, a deletion, a filter line and an error; a progress update framed
# by \r; and a last line with no terminator, ending in a character.
MIXED = (
    ">f+++++++++ año/ñandú.txt\n"
    "cL+++++++++ lien-é -> 日本語/ターゲット.txt\n"
    ">f+++++++++ emoji 😀🎉.bin\n"
    "\r      1,300,042  27%  247.96MB/s    0:00:00 (xfr#3, to-chk=1/8)\r"
    "*deleting   vieux/été.txt\n"
    "[sender] hiding directory Fotos/privé because of pattern privé/\n"
    "rsync: [sender] link_stat \"/nope/ñ\" failed: No such file or directory (2)\n"
    "dernière ligne — 終"
)

# A name in Latin-1 between two good lines, as -8 would write it.
INVALID = (b">f+++++++++ avant-\xc3\xb1.txt\n"
           b">f+++++++++ bad-\xe9-\xff.txt\n"
           b">f+++++++++ apr\xc3\xa8s.txt\n")

NEW = ">f+++++++++"


def whole(data: bytes) -> list:
    p = StreamParser()
    return list(p.feed_bytes(data)) + list(p.finish())


def chunked(data: bytes, size: int) -> list:
    p = StreamParser()
    out = []
    for i in range(0, len(data), size):
        out.extend(p.feed_bytes(data[i:i + size]))
    out.extend(p.finish())
    return out


def split_at(data: bytes, at: int) -> list:
    p = StreamParser()
    out = list(p.feed_bytes(data[:at]))
    out.extend(p.feed_bytes(data[at:]))
    out.extend(p.finish())
    return out


def assert_cut_anywhere(name: str, data: bytes) -> None:
    """Every split offset, and every chunk size from one byte up."""
    expected = whole(data)
    for at in range(len(data) + 1):
        got = split_at(data, at)
        assert got == expected, f"{name}: split at byte {at}: {got}"
    for size in [*range(1, 17), 31, 64, 255, 4096, 8192, max(len(data), 1)]:
        got = chunked(data, size)
        assert got == expected, f"{name}: {size}-byte chunks: {got}"


def check_the_mixed_input_parses_to_these_events() -> None:
    assert whole(MIXED.encode()) == [
        ItemizedChange(NEW, "año/ñandú.txt"),
        ItemizedChange("cL+++++++++", "lien-é", "日本語/ターゲット.txt"),
        ItemizedChange(NEW, "emoji 😀🎉.bin"),
        Progress(bytes_done=1_300_042, percent=27, rate_human="247.96MB/s",
                 elapsed="0:00:00", xfr_index=3, check_phase="to-chk",
                 check_remaining=1, check_total=8),
        ItemizedChange("*deleting", "vieux/été.txt", deleted=True),
        FilterMatch(action="hiding", is_dir=True, path="Fotos/privé",
                    pattern="privé/"),
        Message('rsync: [sender] link_stat "/nope/ñ" failed: '
                'No such file or directory (2)', is_error=True),
        Message("dernière ligne — 終", is_error=False),
    ], whole(MIXED.encode())


def check_multibyte_input_is_the_same_wherever_it_is_cut() -> None:
    assert_cut_anywhere("mixed", MIXED.encode())


def check_fixtures_are_the_same_wherever_they_are_cut() -> None:
    seen = 0
    for path in sorted(FIXTURES.iterdir()):
        if path.is_file():
            assert_cut_anywhere(path.name, path.read_bytes())
            seen += 1
    assert seen >= 7, f"found {seen} fixtures in {FIXTURES}"


def check_the_non_ascii_fixture_yields_the_names_rsync_printed() -> None:
    data = (FIXTURES / "progress2_non_ascii.raw").read_bytes()
    events = chunked(data, 1)
    changes = [(e.path, e.link_target) for e in events
               if isinstance(e, ItemizedChange)]
    assert changes == [
        (r"cr\#015name.txt", None),
        ("emoji 😀.bin", None),
        (r"latin1-\#351-\#377.txt", None),
        ("lien-é", "日本語.txt"),
        ("日本語.txt", None),
        ("año/", None),
        ("año/ñandú.txt", None),
    ], changes
    assert all(isinstance(e, (ItemizedChange, Progress)) for e in events)
    assert any(isinstance(e, Progress) for e in events)


def check_feed_is_feed_bytes() -> None:
    p = StreamParser()
    out = list(p.feed(MIXED)) + list(p.finish())
    assert out == whole(MIXED.encode())


def check_a_line_that_is_not_utf8_still_arrives_and_disturbs_nothing() -> None:
    assert whole(INVALID) == [
        ItemizedChange(NEW, "avant-ñ.txt"),
        ItemizedChange(NEW, "bad-�-�.txt"),
        ItemizedChange(NEW, "après.txt"),
    ], whole(INVALID)
    assert_cut_anywhere("invalid", INVALID)


def check_invalid_sequences_are_replaced_the_same_way_every_time() -> None:
    cases = [
        (b"\xe9", "�"),                        # Latin-1 é
        (b"\xe2\x82", "�"),                    # € cut short
        (b"\xf0\x9f\x98", "�"),                # 😀 cut short
        (b"\xc0\xaf", "��"),              # overlong '/'
        (b"\xed\xa0\x80", "���"),    # a surrogate
        (b"\x80\xe2\x82\xac", "�€"),           # stray continuation, then €
    ]
    for bad, shown in cases:
        line = b">f+++++++++ a" + bad + b"z.txt\n"
        assert whole(line) == [ItemizedChange(NEW, f"a{shown}z.txt")], \
            (bad, whole(line))
        assert_cut_anywhere(repr(bad), line)


def check_finish_flushes_a_last_line_that_has_no_terminator() -> None:
    p = StreamParser()
    assert list(p.feed_bytes("*deleting   vieux/ét".encode())) == []
    assert list(p.feed_bytes("é.txt".encode())) == []
    assert list(p.finish()) == [
        ItemizedChange("*deleting", "vieux/été.txt", deleted=True)]
    # Flushed means gone.
    assert list(p.finish()) == []
    assert list(p.feed_bytes(b"\n")) == []


def check_finish_on_a_stream_cut_inside_a_character() -> None:
    line = ">f+++++++++ 日本語".encode()
    p = StreamParser()
    assert list(p.feed_bytes(line[:-1])) == []
    assert list(p.finish()) == [ItemizedChange(NEW, "日本�")]


def check_finish_on_nothing_but_whitespace_yields_nothing() -> None:
    p = StreamParser()
    assert list(p.feed_bytes(b"\n\r\n  \t ")) == []
    assert list(p.finish()) == []


# -- a progress update with a line glued to it -------------------------------
#
# The cases of the same names in chunk_boundary_test.rs, with the same text.

SUMMARY = ("rsync error: some files/attrs were not transferred (see previous "
           "errors) (code 23) at main.c(1394) [sender=3.5.0-g483b5efc]")
LAST_UPDATE = "         70,002 100%   47.69MB/s    0:00:00 (xfr#4, to-chk=0/6)"
LAST = Progress(bytes_done=70_002, percent=100, rate_human="47.69MB/s",
                elapsed="0:00:00", xfr_index=4, check_phase="to-chk",
                check_remaining=0, check_total=6)


def check_a_summary_glued_to_the_last_update_is_an_update_and_an_error() -> None:
    data = f"\r{LAST_UPDATE}\r{LAST_UPDATE}{SUMMARY}\n\n".encode()
    assert whole(data) == [LAST, LAST, Message(SUMMARY, True)], whole(data)
    assert_cut_anywhere("glued summary", data)


def check_a_cause_glued_to_an_update_is_an_update_and_an_error() -> None:
    cause = ('rsync: [sender] send_files failed to open '
             '"/x/50% (xfr#9, to-chk=1/2) (copy).bin": Permission denied (13)')
    data = f"\r{LAST_UPDATE}{cause}\n".encode()
    assert whole(data) == [LAST, Message(cause, True)], whole(data)
    assert_cut_anywhere("glued cause", data)


def check_a_line_glued_to_a_mid_file_update_is_split_after_its_two_spaces() -> None:
    stopped = ("rsync error: received SIGINT, SIGTERM, or SIGHUP (code 20) "
               "at rsync.c(874) [sender=3.5.0-g483b5efc]")
    data = (f"\r      1,081,344  36%  500.73kB/s    0:00:03  {stopped}\n"
            ).encode()
    assert whole(data) == [
        Progress(bytes_done=1_081_344, percent=36, rate_human="500.73kB/s",
                 elapsed="0:00:03"),
        Message(stopped, True),
    ], whole(data)
    assert_cut_anywhere("glued to a mid-file update", data)


def check_the_padding_after_a_trailer_is_not_part_of_the_line_that_follows() -> None:
    data = f"{LAST_UPDATE}   {SUMMARY}\n".encode()
    assert whole(data) == [LAST, Message(SUMMARY, True)], whole(data)
    assert_cut_anywhere("glued after padding", data)


def check_what_follows_the_update_is_classified_as_a_line_of_its_own() -> None:
    vanished = ("rsync warning: some files vanished before they could be "
                "transferred (code 24) at main.c(1394) "
                "[sender=3.5.0-g483b5efc]")
    cases = [
        ("sent 125 bytes  received 33 bytes  316.00 bytes/sec",
         Message("sent 125 bytes  received 33 bytes  316.00 bytes/sec", False)),
        ("note: rsync error: is not at the start",
         Message("note: rsync error: is not at the start", False)),
        (vanished, Message(vanished, False)),
        ("user@nas.local: Permission denied (publickey).",
         Message("user@nas.local: Permission denied (publickey).", True)),
        ("cd+++++++++ locked (100%)/",
         ItemizedChange("cd+++++++++", "locked (100%)/")),
        ("*deleting   old/été.txt",
         ItemizedChange("*deleting", "old/été.txt", deleted=True)),
    ]
    for rest, expected in cases:
        data = f"\r{LAST_UPDATE}{rest}\n".encode()
        assert whole(data) == [LAST, expected], (rest, whole(data))
        assert_cut_anywhere(rest, data)


def check_whole_lines_are_what_they_were() -> None:
    for line in (LAST_UPDATE, LAST_UPDATE + "   "):
        assert whole(f"\r{line}\r".encode()) == [LAST]
        assert whole(f"{line}\n".encode()) == [LAST]
        assert whole(line.encode()) == [LAST]
    assert whole(b"\r         20,000  28%    0.00kB/s    0:00:00  \r") == [
        Progress(bytes_done=20_000, percent=28, rate_human="0.00kB/s",
                 elapsed="0:00:00")]
    assert whole(f"{SUMMARY}\n".encode()) == [Message(SUMMARY, True)]


def check_a_line_that_only_contains_the_words_is_one_event_and_no_error() -> None:
    for line in [
        "note: rsync error: is not at the start",
        "building file list ... rsync: done",
        "100% rsync error: no",
        "  1,000  50% done rsync error: no",
        "  1,000  50%  0.00kB/s  0:00 rsync error: no",
        "  1,000  50%  0.00kB/s  0:00:00 rsync error: no",
        "  1,000  50%  0.00kB/s  0:00:00rsync error: no",
        "  1,000  50%  0.00kB/s  0:00:007 rsync error: no",
        "x 1,000  50%  0.00kB/s  0:00:00  rsync error: no",
    ]:
        got = whole(f"{line}\n".encode())
        assert got == [Message(line.rstrip(), False)], (line, got)
    name = ("70,002 100%  47.69MB/s  0:00:00 (xfr#4, to-chk=0/6)"
            "rsync error: x).txt")
    assert whole(f"{NEW} {name}\n".encode()) == [ItemizedChange(NEW, name)]
    assert whole(f"*deleting   {name}\n".encode()) == [
        ItemizedChange("*deleting", name, deleted=True)]


def check_an_update_followed_by_nothing_is_not_split() -> None:
    assert whole(f"{LAST_UPDATE} \t \n".encode()) == [LAST]


def check_finish_flushes_both_events_of_a_glued_line() -> None:
    p = StreamParser()
    assert list(p.feed_bytes(f"\r{LAST_UPDATE}{SUMMARY}".encode())) == []
    assert list(p.finish()) == [LAST, Message(SUMMARY, True)]
    assert list(p.finish()) == []


def check_the_glued_error_fixture_yields_the_summary_and_the_cause() -> None:
    data = (FIXTURES / "progress2_glued_error.raw").read_bytes()
    assert b"to-chk=0/6)rsync error: " in data, data

    expected = whole(data)
    for size in (1, 2, 3, 7, 64, 4096, 8192):
        assert chunked(data, size) == expected, f"{size}-byte chunks"

    errors = [e.text for e in expected
              if isinstance(e, Message) and e.is_error]
    assert len(errors) == 2, errors
    assert errors[0].startswith('rsync: [sender] send_files failed to open "')
    assert errors[0].endswith('/g/src/locked.bin": Permission denied (13)')
    assert errors[1] == SUMMARY, errors

    assert not any(isinstance(e, Message) and not e.is_error
                   for e in expected), expected
    changes = [e.path for e in expected if isinstance(e, ItemizedChange)]
    assert changes == [
        "src/",
        "src/a.bin",
        "src/sub/",
        "src/sub/50% (xfr#1, to-chk=0) done.txt",
        "src/sub/z.bin",
    ], changes
    assert expected[-1] == Message(SUMMARY, True)
    last = expected[-2]
    assert isinstance(last, Progress), last
    assert (last.check_remaining, last.check_total, last.xfr_index) == (0, 6, 4)
    assert sum(isinstance(e, Progress) for e in expected) == 7


if __name__ == "__main__":
    checks = [(name, fn) for name, fn in sorted(globals().items())
              if name.startswith("check_") and callable(fn)]
    for name, fn in checks:
        fn()
        print(f"ok  {name[len('check_'):]}")
    print(f"{len(checks)} checks passed")
