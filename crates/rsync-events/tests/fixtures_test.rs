//! Integration tests against transcripts captured from rsync 3.4.4.
//!
//! Regenerate fixtures with `scripts/capture_fixtures.sh` whenever the
//! bundled rsync version is bumped — the version pin IS the format contract.

use rsync_events::*;
use std::path::PathBuf;

fn load(name: &str) -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Run text through StreamParser in deliberately awkward chunk sizes to
/// prove chunk-boundary handling (mirrors what gio::Subprocess delivers).
fn events_of(text: &str, chunk: usize) -> Vec<Event> {
    let mut p = StreamParser::new();
    let mut out = Vec::new();
    let bytes: Vec<char> = text.chars().collect();
    for piece in bytes.chunks(chunk) {
        out.extend(p.feed(&piece.iter().collect::<String>()));
    }
    out.extend(p.finish());
    out
}

fn changes(evs: &[Event]) -> Vec<&ItemizedChange> {
    evs.iter()
        .filter_map(|e| match e {
            Event::Change(c) => Some(c),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------- itemize

#[test]
fn dry_run_delta_classification() {
    let text = load("dry_run_itemize.txt");
    let evs = events_of(&text, 11);
    let ch = changes(&evs);
    let by_path = |p: &str| *ch.iter().find(|c| c.path == p).unwrap();

    let updated = by_path("readme.txt"); // >f.s.......
    assert_eq!(updated.kind(), ChangeKind::Updated);
    assert!(updated.changed_attrs().contains("size"));
    assert_eq!(updated.file_kind(), FileKind::File);

    let deleted = by_path("media/video_part2.bin"); // *deleting
    assert_eq!(deleted.kind(), ChangeKind::Deleted);

    let attrs = by_path("docs/"); // .d...p.....
    assert_eq!(attrs.kind(), ChangeKind::Attrs);
    assert_eq!(attrs.changed_attrs().into_iter().collect::<Vec<_>>(), vec!["perms"]);
    assert_eq!(attrs.file_kind(), FileKind::Directory);

    let created = by_path("docs/new_chapter.odt"); // >f+++++++++
    assert_eq!(created.kind(), ChangeKind::Created);
    assert!(created.is_new());
}

#[test]
fn fresh_sync_symlink_target() {
    let text = load("dry_run_fresh.txt");
    let evs = events_of(&text, 11);
    let ch = changes(&evs);

    let link = ch.iter().find(|c| c.file_kind() == FileKind::Symlink).unwrap();
    assert_eq!(link.path, "latest-thesis");
    assert_eq!(link.link_target.as_deref(), Some("docs/thesis.pdf"));
    assert_eq!(link.kind(), ChangeKind::Created);
    assert!(ch.iter().all(|c| c.kind() == ChangeKind::Created));
}

#[test]
fn itemize_line_roundtrip() {
    let c = parse_itemize_line(">f..t...... media/video_part1.bin").unwrap();
    assert_eq!(c.kind(), ChangeKind::Attrs);
    assert_eq!(c.changed_attrs().into_iter().collect::<Vec<_>>(), vec!["mtime"]);
}

// ---------------------------------------------------------------- progress2

#[test]
fn progress_stream_with_carriage_returns() {
    let text = load("progress2_run.raw");
    let evs = events_of(&text, 5);
    let progress: Vec<&Progress> = evs
        .iter()
        .filter_map(|e| match e {
            Event::Progress(p) => Some(p),
            _ => None,
        })
        .collect();

    assert!(!progress.is_empty(), "no progress events parsed");
    assert!(!changes(&evs).is_empty(), "no per-file events parsed");

    let last = progress.last().unwrap();
    // Real rsync 3.4.4 behavior: the last progress line can read 99% due to
    // integer truncation. Completion is signaled by process exit + to-chk=0,
    // NEVER by percent == 100. The UI must not wait for 100.
    assert!(last.percent >= 99);
    assert!(last.bytes_done > 4_000_000);
    assert_eq!(last.check_phase.as_deref(), Some("to-chk"));
    assert_eq!(last.check_remaining, Some(0));
    assert!(!last.scanning());

    let first_tagged = progress.iter().find(|p| p.xfr_index.is_some()).unwrap();
    assert_eq!(first_tagged.xfr_index, Some(1));
}

#[test]
fn progress_line_variants() {
    let p = parse_progress_line("      1,300,042  27%  247.96MB/s    0:00:00 (xfr#3, to-chk=1/8)")
        .unwrap();
    assert_eq!(p.bytes_done, 1_300_042);
    assert_eq!(p.percent, 27);
    assert_eq!(p.rate_human, "247.96MB/s");
    assert_eq!(p.xfr_index, Some(3));

    let scanning = parse_progress_line(
        "     52,428,800   3%   99.85MB/s    0:00:14 (xfr#12, ir-chk=1041/2688)",
    )
    .unwrap();
    assert!(scanning.scanning());
    assert_eq!(scanning.check_total, Some(2688));

    let bare = parse_progress_line("             42   0%    0.00kB/s    0:00:00  ").unwrap();
    assert_eq!(bare.xfr_index, None);
}

// ---------------------------------------------------------------- stats

#[test]
fn stats_block() {
    let st = parse_stats_block(&load("stats_run.txt"));
    assert_eq!(st.files_total, Some(9));
    assert_eq!(st.files_created, Some(1));
    assert_eq!(st.files_transferred, Some(1));
    assert_eq!(st.total_size, Some(5_400_057));
    assert_eq!(st.transferred_size, Some(600_000));
    assert_eq!(st.bytes_sent, Some(600_471));
    assert_eq!(st.speedup, Some(8.99));
}

// ---------------------------------------------------------------- errors

#[test]
fn error_transcript_and_exit_code() {
    let text = load("error_missing_source.txt");
    let evs = events_of(&text, 11);
    let errors: Vec<&Message> = evs
        .iter()
        .filter_map(|e| match e {
            Event::Message(m) if m.is_error => Some(m),
            _ => None,
        })
        .collect();
    assert!(errors.len() >= 2);
    assert!(errors[0].text.contains("does-not-exist"));

    let (sev, human) = classify_exit(23);
    assert_eq!(sev, Severity::Partial);
    assert!(human.contains("some files"));

    assert_eq!(classify_exit(0).0, Severity::Success);
    assert_eq!(classify_exit(255).0, Severity::Error);
}
