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


if __name__ == "__main__":
    checks = [(name, fn) for name, fn in sorted(globals().items())
              if name.startswith("check_") and callable(fn)]
    for name, fn in checks:
        fn()
        print(f"ok  {name[len('check_'):]}")
    print(f"{len(checks)} checks passed")
