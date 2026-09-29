//! Where the reads fall must not change what comes out.
//!
//! rsync's output reaches the parser in whatever pieces the pipe delivers, and
//! a piece can end inside a multibyte character. These tests cut the same
//! input at every offset and in several chunk sizes and require the events to
//! be the ones the uncut input gives.
//!
//! `reference/rsync_events_selfcheck.py` asserts the same cases against the
//! Python spec. Change both or neither.

use rsync_events::*;
use std::path::PathBuf;

/// Two-, three- and four-byte characters; an itemized line, a symlink with its
/// `->` target, a deletion, a filter line and an error; a progress update
/// framed by `\r`; and a last line with no terminator, ending in a character.
const MIXED: &str = concat!(
    ">f+++++++++ año/ñandú.txt\n",
    "cL+++++++++ lien-é -> 日本語/ターゲット.txt\n",
    ">f+++++++++ emoji 😀🎉.bin\n",
    "\r      1,300,042  27%  247.96MB/s    0:00:00 (xfr#3, to-chk=1/8)\r",
    "*deleting   vieux/été.txt\n",
    "[sender] hiding directory Fotos/privé because of pattern privé/\n",
    "rsync: [sender] link_stat \"/nope/ñ\" failed: No such file or directory (2)\n",
    "dernière ligne — 終",
);

/// A name in Latin-1 between two good lines, as `-8` would write it.
const INVALID: &[u8] =
    b">f+++++++++ avant-\xc3\xb1.txt\n>f+++++++++ bad-\xe9-\xff.txt\n>f+++++++++ apr\xc3\xa8s.txt\n";

fn whole(input: &[u8]) -> Vec<Event> {
    let mut p = StreamParser::new();
    let mut out = p.feed_bytes(input);
    out.extend(p.finish());
    out
}

fn chunked(input: &[u8], size: usize) -> Vec<Event> {
    let mut p = StreamParser::new();
    let mut out = Vec::new();
    for piece in input.chunks(size) {
        out.extend(p.feed_bytes(piece));
    }
    out.extend(p.finish());
    out
}

fn split_at(input: &[u8], at: usize) -> Vec<Event> {
    let mut p = StreamParser::new();
    let mut out = p.feed_bytes(&input[..at]);
    out.extend(p.feed_bytes(&input[at..]));
    out.extend(p.finish());
    out
}

/// Every split offset, and every chunk size from one byte up to the input.
fn assert_cut_anywhere(name: &str, input: &[u8]) {
    let expected = whole(input);
    for at in 0..=input.len() {
        assert_eq!(split_at(input, at), expected, "{name}: split at byte {at}");
    }
    let sizes = (1..=16).chain([31, 64, 255, 4096, 8192, input.len().max(1)]);
    for size in sizes {
        assert_eq!(chunked(input, size), expected, "{name}: {size}-byte chunks");
    }
}

fn change(flags: &str, path: &str, target: Option<&str>) -> Event {
    Event::Change(ItemizedChange {
        raw_flags: flags.into(),
        path: path.into(),
        link_target: target.map(str::to_string),
        deleted: flags == "*deleting",
    })
}

/// What the uncut input gives — so that the comparisons below are against
/// something known to be right, not merely against themselves.
#[test]
fn the_mixed_input_parses_to_these_events() {
    assert_eq!(
        whole(MIXED.as_bytes()),
        [
            change(">f+++++++++", "año/ñandú.txt", None),
            change("cL+++++++++", "lien-é", Some("日本語/ターゲット.txt")),
            change(">f+++++++++", "emoji 😀🎉.bin", None),
            Event::Progress(Progress {
                bytes_done: 1_300_042,
                percent: 27,
                rate_human: "247.96MB/s".into(),
                elapsed: "0:00:00".into(),
                xfr_index: Some(3),
                check_phase: Some("to-chk".into()),
                check_remaining: Some(1),
                check_total: Some(8),
            }),
            change("*deleting", "vieux/été.txt", None),
            Event::Filter(FilterMatch {
                action: FilterAction::Hiding,
                is_dir: true,
                path: "Fotos/privé".into(),
                pattern: "privé/".into(),
            }),
            Event::Message(Message {
                text: "rsync: [sender] link_stat \"/nope/ñ\" failed: No such file or directory (2)"
                    .into(),
                is_error: true,
            }),
            Event::Message(Message {
                text: "dernière ligne — 終".into(),
                is_error: false,
            }),
        ]
    );
}

#[test]
fn multibyte_input_is_the_same_wherever_it_is_cut() {
    assert_cut_anywhere("mixed", MIXED.as_bytes());
}

/// The committed fixtures are real rsync output; read as bytes, as the app
/// receives them.
#[test]
fn fixtures_are_the_same_wherever_they_are_cut() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let mut seen = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if !path.is_file() {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        assert_cut_anywhere(&path.display().to_string(), &bytes);
        seen += 1;
    }
    assert!(seen >= 7, "found {seen} fixtures in {}", dir.display());
}

/// Real rsync 3.5.0 output for names that are not ASCII, captured in a UTF-8
/// locale: valid UTF-8 arrives as it is, in the path and in the `%L` target;
/// what is not valid (a Latin-1 name) and a control character (a carriage
/// return in a name) arrive escaped by rsync as `\#ooo`, in plain ASCII.
#[test]
fn the_non_ascii_fixture_yields_the_names_rsync_printed() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/progress2_non_ascii.raw");
    let bytes = std::fs::read(&path).unwrap();
    // One byte at a time: every character in it is split.
    let events = chunked(&bytes, 1);
    let changes: Vec<(&str, Option<&str>)> = events
        .iter()
        .filter_map(|e| match e {
            Event::Change(c) => Some((c.path.as_str(), c.link_target.as_deref())),
            _ => None,
        })
        .collect();
    assert_eq!(
        changes,
        [
            (r"cr\#015name.txt", None),
            ("emoji 😀.bin", None),
            (r"latin1-\#351-\#377.txt", None),
            ("lien-é", Some("日本語.txt")),
            ("日本語.txt", None),
            ("año/", None),
            ("año/ñandú.txt", None),
        ]
    );
    // Everything else in it is a progress update.
    assert!(events
        .iter()
        .all(|e| matches!(e, Event::Change(_) | Event::Progress(_))));
    assert!(events.iter().any(|e| matches!(e, Event::Progress(_))));
}

/// For input that is valid UTF-8 the old entry point and the new one are the
/// same parser.
#[test]
fn feed_is_feed_bytes() {
    let mut p = StreamParser::new();
    let mut out = p.feed(MIXED);
    out.extend(p.finish());
    assert_eq!(out, whole(MIXED.as_bytes()));
}

#[test]
fn a_line_that_is_not_utf8_still_arrives_and_disturbs_nothing() {
    assert_eq!(
        whole(INVALID),
        [
            change(">f+++++++++", "avant-ñ.txt", None),
            change(">f+++++++++", "bad-\u{FFFD}-\u{FFFD}.txt", None),
            change(">f+++++++++", "après.txt", None),
        ]
    );
    assert_cut_anywhere("invalid", INVALID);
}

/// How many U+FFFD an invalid sequence becomes — one per maximal invalid
/// subpart, which is what both `String::from_utf8_lossy` and Python's
/// `errors="replace"` do. Pinned because the two implementations must agree on
/// the text, not only on the event count.
#[test]
fn invalid_sequences_are_replaced_the_same_way_every_time() {
    let cases: [(&[u8], &str); 6] = [
        (b"\xe9", "\u{FFFD}"),                         // Latin-1 é
        (b"\xe2\x82", "\u{FFFD}"),                     // € cut short
        (b"\xf0\x9f\x98", "\u{FFFD}"),                 // 😀 cut short
        (b"\xc0\xaf", "\u{FFFD}\u{FFFD}"),             // overlong '/'
        (b"\xed\xa0\x80", "\u{FFFD}\u{FFFD}\u{FFFD}"), // a surrogate
        (b"\x80\xe2\x82\xac", "\u{FFFD}€"),            // stray continuation, then €
    ];
    for (bad, shown) in cases {
        let mut line = b">f+++++++++ a".to_vec();
        line.extend_from_slice(bad);
        line.extend_from_slice(b"z.txt\n");
        assert_eq!(
            whole(&line),
            [change(">f+++++++++", &format!("a{shown}z.txt"), None)],
            "{bad:x?}"
        );
        assert_cut_anywhere(&format!("{bad:x?}"), &line);
    }
}

#[test]
fn finish_flushes_a_last_line_that_has_no_terminator() {
    let mut p = StreamParser::new();
    assert!(p.feed_bytes("*deleting   vieux/ét".as_bytes()).is_empty());
    assert!(p.feed_bytes("é.txt".as_bytes()).is_empty());
    assert_eq!(p.finish(), [change("*deleting", "vieux/été.txt", None)]);
    // Flushed means gone.
    assert!(p.finish().is_empty());
    assert!(p.feed_bytes(b"\n").is_empty());
}

/// A stream that ends inside a character: the line arrives, with one U+FFFD
/// where the character would have been.
#[test]
fn finish_on_a_stream_cut_inside_a_character() {
    let line = ">f+++++++++ 日本語".as_bytes();
    let mut p = StreamParser::new();
    assert!(p.feed_bytes(&line[..line.len() - 1]).is_empty());
    assert_eq!(p.finish(), [change(">f+++++++++", "日本\u{FFFD}", None)]);
}

#[test]
fn finish_on_nothing_but_whitespace_yields_nothing() {
    let mut p = StreamParser::new();
    assert!(p.feed_bytes(b"\n\r\n  \t ").is_empty());
    assert!(p.finish().is_empty());
}

// -- a progress update with a line glued to it ------------------------------
//
// rsync does not always end a progress update before it prints something
// else; see "One line, two events" in the crate docs. The cases below are in
// `reference/rsync_events_selfcheck.py` too, with the same text.

const SUMMARY: &str = "rsync error: some files/attrs were not transferred (see previous errors) (code 23) at main.c(1394) [sender=3.5.0-g483b5efc]";
const LAST_UPDATE: &str = "         70,002 100%   47.69MB/s    0:00:00 (xfr#4, to-chk=0/6)";

fn progress(bytes_done: u64, percent: u8, rate: &str, elapsed: &str) -> Progress {
    Progress {
        bytes_done,
        percent,
        rate_human: rate.into(),
        elapsed: elapsed.into(),
        xfr_index: None,
        check_phase: None,
        check_remaining: None,
        check_total: None,
    }
}

fn last_update() -> Event {
    Event::Progress(Progress {
        xfr_index: Some(4),
        check_phase: Some("to-chk".into()),
        check_remaining: Some(0),
        check_total: Some(6),
        ..progress(70_002, 100, "47.69MB/s", "0:00:00")
    })
}

fn message(text: &str, is_error: bool) -> Event {
    Event::Message(Message {
        text: text.into(),
        is_error,
    })
}

/// The end of a run that exits 23, byte for byte as rsync 3.5.0 wrote it: the
/// update, the summary with nothing between them, and the newline that should
/// have parted them arriving last.
#[test]
fn a_summary_glued_to_the_last_update_is_an_update_and_an_error() {
    let input = format!("\r{LAST_UPDATE}\r{LAST_UPDATE}{SUMMARY}\n\n");
    assert_eq!(
        whole(input.as_bytes()),
        [last_update(), last_update(), message(SUMMARY, true)]
    );
    assert_cut_anywhere("glued summary", input.as_bytes());
}

/// The line that says why, in the same place. The path holds `)`, `%`, digits
/// and the look of a trailer: the split is at the end of the update and
/// nowhere after it.
#[test]
fn a_cause_glued_to_an_update_is_an_update_and_an_error() {
    let cause = "rsync: [sender] send_files failed to open \"/x/50% (xfr#9, to-chk=1/2) (copy).bin\": Permission denied (13)";
    let input = format!("\r{LAST_UPDATE}{cause}\n");
    assert_eq!(
        whole(input.as_bytes()),
        [last_update(), message(cause, true)]
    );
    assert_cut_anywhere("glued cause", input.as_bytes());
}

/// A run that is stopped: the update is a mid-file one, which has no trailer
/// and ends in two spaces. As captured from 3.5.0.
#[test]
fn a_line_glued_to_a_mid_file_update_is_split_after_its_two_spaces() {
    let stopped = "rsync error: received SIGINT, SIGTERM, or SIGHUP (code 20) at rsync.c(874) [sender=3.5.0-g483b5efc]";
    let input = format!("\r      1,081,344  36%  500.73kB/s    0:00:03  {stopped}\n");
    assert_eq!(
        whole(input.as_bytes()),
        [
            Event::Progress(progress(1_081_344, 36, "500.73kB/s", "0:00:03")),
            message(stopped, true),
        ]
    );
    assert_cut_anywhere("glued to a mid-file update", input.as_bytes());
}

/// rsync pads a trailer that is shorter than one it printed before. The
/// padding belongs to the update.
#[test]
fn the_padding_after_a_trailer_is_not_part_of_the_line_that_follows() {
    let input = format!("{LAST_UPDATE}   {SUMMARY}\n");
    assert_eq!(
        whole(input.as_bytes()),
        [last_update(), message(SUMMARY, true)]
    );
    assert_cut_anywhere("glued after padding", input.as_bytes());
}

/// What follows the update is classified as any line is: by what it starts
/// with. Glued text that is not an error does not become one, whatever it
/// contains, and an itemized line is an itemized line (this is what stdout
/// holds when the two streams are read apart).
#[test]
fn what_follows_the_update_is_classified_as_a_line_of_its_own() {
    for (rest, expected) in [
        (
            "sent 125 bytes  received 33 bytes  316.00 bytes/sec",
            message("sent 125 bytes  received 33 bytes  316.00 bytes/sec", false),
        ),
        (
            "note: rsync error: is not at the start",
            message("note: rsync error: is not at the start", false),
        ),
        (
            "rsync warning: some files vanished before they could be transferred (code 24) at main.c(1394) [sender=3.5.0-g483b5efc]",
            message("rsync warning: some files vanished before they could be transferred (code 24) at main.c(1394) [sender=3.5.0-g483b5efc]", false),
        ),
        (
            "user@nas.local: Permission denied (publickey).",
            message("user@nas.local: Permission denied (publickey).", true),
        ),
        (
            "cd+++++++++ locked (100%)/",
            change("cd+++++++++", "locked (100%)/", None),
        ),
        (
            "*deleting   old/été.txt",
            change("*deleting", "old/été.txt", None),
        ),
    ] {
        let input = format!("\r{LAST_UPDATE}{rest}\n");
        assert_eq!(
            whole(input.as_bytes()),
            [last_update(), expected],
            "{rest:?}"
        );
        assert_cut_anywhere(rest, input.as_bytes());
    }
}

/// Lines that parsed before parse as they did: a progress update alone, with
/// and without padding, and an error on a line of its own.
#[test]
fn whole_lines_are_what_they_were() {
    let padded = format!("{LAST_UPDATE}   ");
    for line in [LAST_UPDATE, padded.as_str()] {
        assert_eq!(whole(format!("\r{line}\r").as_bytes()), [last_update()]);
        assert_eq!(whole(format!("{line}\n").as_bytes()), [last_update()]);
        assert_eq!(whole(line.as_bytes()), [last_update()]);
    }
    assert_eq!(
        whole(b"\r         20,000  28%    0.00kB/s    0:00:00  \r"),
        [Event::Progress(progress(20_000, 28, "0.00kB/s", "0:00:00"))]
    );
    assert_eq!(
        whole(format!("{SUMMARY}\n").as_bytes()),
        [message(SUMMARY, true)]
    );
}

/// Not split, and not errors: the words in the middle of a line, a name that
/// holds them, and lines that only resemble the start of an update — one
/// without its rate, one whose time is not `h:mm:ss`, one that ends in a
/// single space where rsync writes the trailer or two.
#[test]
fn a_line_that_only_contains_the_words_is_one_event_and_no_error() {
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
    ] {
        assert_eq!(
            whole(format!("{line}\n").as_bytes()),
            [message(line.trim_end(), false)],
            "{line:?}"
        );
    }
    // A name is part of an itemized line, which is recognised by its flags
    // before anything is looked for inside it.
    let name = "70,002 100%  47.69MB/s  0:00:00 (xfr#4, to-chk=0/6)rsync error: x).txt";
    assert_eq!(
        whole(format!(">f+++++++++ {name}\n").as_bytes()),
        [change(">f+++++++++", name, None)]
    );
    assert_eq!(
        whole(format!("*deleting   {name}\n").as_bytes()),
        [change("*deleting", name, None)]
    );
}

/// An update and blanks after it, of any kind, is an update and no more.
#[test]
fn an_update_followed_by_nothing_is_not_split() {
    assert_eq!(
        whole(format!("{LAST_UPDATE} \t \n").as_bytes()),
        [last_update()]
    );
}

/// `finish` flushes both halves of a glued line that has no terminator.
#[test]
fn finish_flushes_both_events_of_a_glued_line() {
    let mut p = StreamParser::new();
    assert!(p
        .feed_bytes(format!("\r{LAST_UPDATE}{SUMMARY}").as_bytes())
        .is_empty());
    assert_eq!(p.finish(), [last_update(), message(SUMMARY, true)]);
    assert!(p.finish().is_empty());
}

/// Real rsync 3.5.0 output, stderr merged into stdout, for a run that could
/// not read one file and exited 23 (`scripts/capture_fixtures.sh`, step 9).
/// The cause arrives on a line of its own; the summary arrives glued to the
/// last progress update.
#[test]
fn the_glued_error_fixture_yields_the_summary_and_the_cause() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/progress2_glued_error.raw");
    let bytes = std::fs::read(&path).unwrap();
    // The junction this fixture exists for is in it.
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("to-chk=0/6)rsync error: "), "{text:?}");

    let expected = whole(&bytes);
    for size in [1, 2, 3, 7, 64, 4096, 8192] {
        assert_eq!(chunked(&bytes, size), expected, "{size}-byte chunks");
    }

    let errors: Vec<&str> = expected
        .iter()
        .filter_map(|e| match e {
            Event::Message(m) if m.is_error => Some(m.text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(
        errors[0].starts_with("rsync: [sender] send_files failed to open \"")
            && errors[0].ends_with("/g/src/locked.bin\": Permission denied (13)"),
        "{errors:?}"
    );
    assert_eq!(errors[1], SUMMARY);

    // Nothing in it is left over as chatter: every line is accounted for.
    assert!(
        !expected
            .iter()
            .any(|e| matches!(e, Event::Message(m) if !m.is_error)),
        "{expected:?}"
    );
    let changes: Vec<&str> = expected
        .iter()
        .filter_map(|e| match e {
            Event::Change(c) => Some(c.path.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        changes,
        [
            "src/",
            "src/a.bin",
            "src/sub/",
            "src/sub/50% (xfr#1, to-chk=0) done.txt",
            "src/sub/z.bin",
        ]
    );
    // The update the summary was glued to is the last event before it.
    let n = expected.len();
    assert_eq!(expected[n - 1], message(SUMMARY, true));
    match &expected[n - 2] {
        Event::Progress(p) => {
            assert_eq!(p.check_remaining, Some(0));
            assert_eq!(p.check_total, Some(6));
            assert_eq!(p.xfr_index, Some(4));
        }
        other => panic!("expected the last progress update, got {other:?}"),
    }
    let updates = expected
        .iter()
        .filter(|e| matches!(e, Event::Progress(_)))
        .count();
    assert_eq!(updates, 7);
}
