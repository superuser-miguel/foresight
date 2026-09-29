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
