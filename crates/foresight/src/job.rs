//! Job model and the single source of truth for rsync argv construction.
//!
//! Every rsync invocation in the app goes through [`Job::build_argv`]. No inline
//! argv vectors elsewhere, no shell strings ever: the subprocess is always
//! spawned with an argv vector, so no user-supplied string is shell-interpreted.
//! Paths are carried as [`PathBuf`]/[`OsString`] end to end and never
//! lossy-converted to UTF-8 before reaching argv.
//!
//! The two command lines this produces are the reporting contract that
//! `rsync-events` is tested against (see that crate's docs):
//!
//! ```text
//! rsync -a --info=progress2 --out-format='%i %n%L' [--delete] SRC DST   # Sync
//! rsync -a -n -i [--delete] SRC DST                                     # Preview
//! ```
//!
//! `SRC` is passed **verbatim** by default, so a selected folder lands inside
//! the destination as `DST/<folder>/` — what dragging a folder onto the app
//! visibly promises. A trailing `/` (rsync's "contents of", which spills the
//! folder's children directly into `DST`) is only ever appended when the user
//! explicitly turns on [`Job::sync_contents`]. See [`Job::source_args`].

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

/// Which of the two contract command lines to build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `-n -i`: a dry run that itemizes changes to populate the preview list.
    Preview,
    /// `--info=progress2 --out-format='%i %n%L'`: the real transfer.
    Sync,
}

/// One selected source: a path and whether it is a directory.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Source {
    pub path: PathBuf,
    pub is_dir: bool,
}

/// Which way a filter rule decides: keep what it matches, or skip it.
///
/// `Exclude` is the default because it is the rule people reach for first, and
/// because every rule that existed before include rules was an exclude — so a
/// preset loaded without a recorded kind means exactly that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FilterKind {
    /// `--include=<pattern>`: keep a match even if a *later* rule would skip it.
    Include,
    /// `--exclude=<pattern>`: skip a match.
    #[default]
    Exclude,
}

impl FilterKind {
    /// The argv flag this kind emits, without its `=value`.
    pub fn flag(self) -> &'static str {
        match self {
            Self::Include => "--include",
            Self::Exclude => "--exclude",
        }
    }

    /// Human label for the UI.
    pub fn label(self) -> &'static str {
        match self {
            Self::Include => "Include",
            Self::Exclude => "Exclude",
        }
    }

    /// Stable token for on-disk presets. Not the label: the label is display
    /// text and may be reworded or translated, while this is a storage format.
    pub fn as_key(self) -> &'static str {
        match self {
            Self::Include => "include",
            Self::Exclude => "exclude",
        }
    }

    /// Parse [`as_key`]. Anything unrecognised — including a key written by a
    /// future version — reads as `Exclude`, the conservative answer: an
    /// unreadable rule then skips files rather than silently letting them
    /// through a filter set that was meant to be restrictive.
    ///
    /// [`as_key`]: Self::as_key
    pub fn from_key(key: &str) -> Self {
        match key {
            "include" => Self::Include,
            _ => Self::Exclude,
        }
    }
}

/// One filter rule: a pattern and what to do with what it matches.
///
/// Rules are an **ordered** list, not two sets. rsync walks the filter rules in
/// argv order and the *first* one that matches a path decides it, so position
/// is meaning: `--include=*.jpg` above `--exclude=*` keeps the JPEGs, while the
/// same two rules swapped keep nothing. Both orders are expressible on purpose
/// — see [`Job::filters`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FilterRule {
    pub kind: FilterKind,
    /// Stored verbatim. It becomes one argv element, so it may contain spaces.
    pub pattern: String,
}

impl FilterRule {
    pub fn include(pattern: impl Into<String>) -> Self {
        Self {
            kind: FilterKind::Include,
            pattern: pattern.into(),
        }
    }

    pub fn exclude(pattern: impl Into<String>) -> Self {
        Self {
            kind: FilterKind::Exclude,
            pattern: pattern.into(),
        }
    }

    /// The exact argv element this rule contributes.
    fn to_arg(&self) -> OsString {
        OsString::from(format!("{}={}", self.kind.flag(), self.pattern))
    }
}

/// A configured sync. The preview and the real run are built from the *same*
/// `Job`, so the dry run faithfully predicts what the transfer will do —
/// including deletions when [`delete`](Self::delete) is on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Job {
    /// One or more sources — files and/or folders, possibly from different
    /// locations. rsync accepts them as `src1 src2 … dst`.
    pub sources: Vec<Source>,
    pub dest: PathBuf,
    /// Mirror deletions (`--delete`). Off by default; a safety rail in the UI.
    /// Only offered for the single-directory case (see [`is_single_dir`]).
    ///
    /// [`is_single_dir`]: Self::is_single_dir
    pub delete: bool,
    /// Copy the *contents* of a single selected folder into the destination
    /// (rsync's trailing-slash form) instead of the folder itself.
    ///
    /// Off by default: a folder named `Photos` normally lands as
    /// `dest/Photos/…`. With this on it becomes `dest/…`, spilling its children
    /// straight into the destination. Only meaningful when [`is_single_dir`] —
    /// a trailing slash on a file makes rsync reject it as "not a directory".
    ///
    /// [`is_single_dir`]: Self::is_single_dir
    pub sync_contents: bool,

    // -- Advanced options (all optional; none change rsync's *reporting*
    //    format, so the rsync-events contract is unaffected) ---------------
    /// `-v`: verbose. Adds informational lines (file list, per-file names,
    /// stats trailer) that the parser surfaces as `Message` events — shown
    /// verbatim in the transfer log. Does not alter the `%i %n%L` format.
    pub verbose: bool,
    /// `--remove-source-files`: delete each source file after it transfers
    /// (turns a copy into a move). No effect during the `-n` dry run.
    pub remove_source_files: bool,
    /// `--bwlimit=<RATE>`, where RATE is an rsync rate token with a unit suffix
    /// (`"85M"`, `"500K"`, `"2G"` — rsync's default unit is KiB/s). `None` or an
    /// empty string means unlimited (the flag is omitted).
    pub bwlimit: Option<String>,
    /// Filter rules in the order the user arranged them — one `--include=` or
    /// `--exclude=` each, emitted in exactly this order because rsync stops at
    /// the first rule that matches a path. Filtering never changes rsync's
    /// *reporting* format, so the `rsync-events` contract is unaffected.
    pub filters: Vec<FilterRule>,
    /// `-e <command>`: the remote shell rsync should use, for a job with a
    /// remote endpoint. `None` for a purely local job — which is every job
    /// today, until the endpoint UI lands.
    ///
    /// Built by [`crate::ssh::rsh_command`], not here: this module composes
    /// argv and holds no policy about *which* ssh options are right. Note this
    /// is the one value rsync re-tokenises itself, so it arrives pre-quoted —
    /// see that module for the rules it has to obey.
    pub remote_shell: Option<String>,
    /// Extra rsync arguments, already tokenised (never shell-interpreted).
    pub extra_args: Vec<String>,
}

impl Job {
    /// Convenience constructor: a single directory source, everything else
    /// default. The app builds `Job` from window state directly; used by tests.
    #[allow(dead_code)]
    pub fn new(source: impl Into<PathBuf>, dest: impl Into<PathBuf>) -> Self {
        Self {
            sources: vec![Source {
                path: source.into(),
                is_dir: true,
            }],
            dest: dest.into(),
            ..Self::default()
        }
    }

    /// Exactly one source and it is a directory. Only this shape may offer
    /// `--delete` and the [`sync_contents`] choice; any other shape (a file, or
    /// several sources) is a "collect" that drops each item *into* dest.
    ///
    /// [`sync_contents`]: Self::sync_contents
    pub fn is_single_dir(&self) -> bool {
        self.sources.len() == 1 && self.sources[0].is_dir
    }

    /// The source operands, in argv order.
    ///
    /// Every source is passed verbatim — a folder therefore nests as
    /// `dest/<folder>/` and a file lands as `dest/<name>`. The single exception
    /// is an explicit [`sync_contents`] request on a lone folder, which appends
    /// the trailing `/` that tells rsync "the contents of this directory".
    ///
    /// [`sync_contents`]: Self::sync_contents
    fn source_args(&self) -> Vec<OsString> {
        if self.sync_contents && self.is_single_dir() {
            return vec![with_trailing_slash(&self.sources[0].path)];
        }
        self.sources
            .iter()
            .map(|s| s.path.as_os_str().to_os_string())
            .collect()
    }

    /// Build the exact argv for `mode`. The program name (`rsync`) is **not**
    /// included — the caller supplies the bundled binary path to the spawner.
    ///
    /// Sources are laid out by [`source_args`]: verbatim by default (a folder
    /// nests as `dest/<dir>/`, a file lands as `dest/<name>`), with the
    /// trailing-slash "contents of" form reserved for an explicit
    /// [`sync_contents`] request on a lone folder.
    ///
    /// [`source_args`]: Self::source_args
    /// [`sync_contents`]: Self::sync_contents
    pub fn build_argv(&self, mode: Mode) -> Vec<OsString> {
        let mut argv: Vec<OsString> = Vec::new();
        argv.push(OsString::from("-a"));

        // Optional user flags go first so they can never override the reporting
        // flags below (rsync takes the last value for --info/--out-format).
        if self.verbose {
            argv.push(OsString::from("-v"));
        }
        if self.remove_source_files {
            argv.push(OsString::from("--remove-source-files"));
        }
        if let Some(rate) = &self.bwlimit {
            if !rate.is_empty() {
                argv.push(OsString::from(format!("--bwlimit={rate}")));
            }
        }
        // Order within this loop is load-bearing: rsync applies the first
        // matching filter rule and ignores the rest, so the list order the user
        // arranged in the UI is the precedence they get.
        for rule in &self.filters {
            argv.push(rule.to_arg());
        }
        // Emitted before extra_args so a user-supplied -e still wins (rsync
        // takes the last), keeping the escape hatch an escape hatch.
        if let Some(shell) = &self.remote_shell {
            argv.push(OsString::from("-e"));
            argv.push(OsString::from(shell));
        }
        for token in &self.extra_args {
            argv.push(OsString::from(token));
        }

        // Reporting flags — the rsync-events contract. Always last among flags.
        match mode {
            Mode::Preview => {
                argv.push(OsString::from("-n"));
                argv.push(OsString::from("-i"));
            }
            Mode::Sync => {
                argv.push(OsString::from("--info=progress2"));
                argv.push(OsString::from("--out-format=%i %n%L"));
            }
        }

        if self.delete {
            argv.push(OsString::from("--delete"));
        }

        argv.extend(self.source_args());
        argv.push(self.dest.as_os_str().to_os_string());
        argv
    }
}

/// Return `path` as an `OsString` guaranteed to end in a single `/`, operating
/// on raw bytes so non-UTF-8 paths survive untouched.
fn with_trailing_slash(path: &Path) -> OsString {
    let mut bytes = path.as_os_str().as_bytes().to_vec();
    if bytes.last() != Some(&b'/') {
        bytes.push(b'/');
    }
    OsString::from_vec(bytes)
}

/// Convenience for logging/display only — never feed this back into argv.
pub fn argv_display(argv: &[OsString]) -> String {
    argv.iter()
        .map(|a| OsStr::to_string_lossy(a).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// Engine runner — spawn the bundled rsync and stream events (Milestone 3)
// ---------------------------------------------------------------------------

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use rsync_events::{classify_exit, Event, Severity, StreamParser};
use std::cell::Cell;
use std::rc::Rc;

/// POSIX SIGTERM — asks rsync to stop cleanly (it exits ~20 → "cancelled").
const SIGTERM: i32 = 15;

/// The outcome of a run, mapped through [`classify_exit`].
#[derive(Debug, Clone)]
pub struct Completion {
    pub severity: Severity,
    pub message: String,
    /// rsync's exit code, or `None` when the process was signalled/cancelled.
    pub code: Option<i32>,
}

/// A live rsync process. Hold it to cancel; drop it once complete.
#[derive(Debug)]
pub struct Runner {
    proc: gio::Subprocess,
    cancelled: Rc<Cell<bool>>,
}

impl Runner {
    /// Ask rsync to stop. The completion arrives as [`Severity::Cancelled`].
    pub fn cancel(&self) {
        self.cancelled.set(true);
        self.proc.send_signal(SIGTERM);
    }
}

/// Spawn `rsync argv…` (bundled rsync resolved via PATH → `/app/bin` in the
/// sandbox), streaming its output on the main context. `on_event` fires for
/// every parsed [`Event`] as bytes arrive; `on_done` fires once at exit.
///
/// stderr is merged into stdout so a single [`StreamParser`] sees rsync's error
/// lines too. Output is read incrementally and never collected in full first,
/// so the main loop is never blocked.
pub fn spawn_rsync<F, D>(
    argv: Vec<OsString>,
    on_event: F,
    on_done: D,
) -> Result<Runner, glib::Error>
where
    F: Fn(Event) + 'static,
    D: FnOnce(Completion) + 'static,
{
    let mut full: Vec<OsString> = Vec::with_capacity(argv.len() + 1);
    full.push(OsString::from("rsync"));
    full.extend(argv);
    let full_refs: Vec<&OsStr> = full.iter().map(OsString::as_os_str).collect();

    let proc = gio::Subprocess::newv(
        &full_refs,
        gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_MERGE,
    )?;
    let stdout = proc.stdout_pipe().expect("STDOUT_PIPE requested");
    let cancelled = Rc::new(Cell::new(false));

    glib::spawn_future_local(glib::clone!(
        #[strong]
        proc,
        #[strong]
        cancelled,
        async move {
            let mut parser = StreamParser::new();
            loop {
                match stdout
                    .read_bytes_future(8192, glib::Priority::DEFAULT)
                    .await
                {
                    Ok(bytes) if bytes.is_empty() => break, // EOF
                    Ok(bytes) => {
                        let chunk = String::from_utf8_lossy(&bytes);
                        for ev in parser.feed(&chunk) {
                            on_event(ev);
                        }
                    }
                    Err(_) => break,
                }
            }
            for ev in parser.finish() {
                on_event(ev);
            }

            let _ = proc.wait_future().await;
            let completion = if cancelled.get() || proc.has_signaled() {
                Completion {
                    severity: Severity::Cancelled,
                    message: "Sync was cancelled.".to_string(),
                    code: None,
                }
            } else {
                let code = proc.exit_status();
                let (severity, message) = classify_exit(code);
                Completion {
                    severity,
                    message,
                    code: Some(code),
                }
            };
            on_done(completion);
        }
    ));

    Ok(Runner { proc, cancelled })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::os::unix::ffi::OsStrExt;

    fn as_strs(argv: &[OsString]) -> Vec<&str> {
        argv.iter().map(|a| a.to_str().unwrap()).collect()
    }

    #[test]
    fn preview_matches_contract() {
        let job = Job::new("/data/src", "/data/dst");
        let argv = job.build_argv(Mode::Preview);
        assert_eq!(as_strs(&argv), ["-a", "-n", "-i", "/data/src", "/data/dst"]);
    }

    #[test]
    fn sync_matches_contract() {
        let job = Job::new("/data/src", "/data/dst");
        let argv = job.build_argv(Mode::Sync);
        assert_eq!(
            as_strs(&argv),
            [
                "-a",
                "--info=progress2",
                "--out-format=%i %n%L",
                "/data/src",
                "/data/dst",
            ]
        );
    }

    #[test]
    fn delete_toggle_inserted_before_paths_in_both_modes() {
        let mut job = Job::new("/s", "/d");
        job.delete = true;
        assert_eq!(
            as_strs(&job.build_argv(Mode::Preview)),
            ["-a", "-n", "-i", "--delete", "/s", "/d"]
        );
        assert_eq!(
            as_strs(&job.build_argv(Mode::Sync)),
            [
                "-a",
                "--info=progress2",
                "--out-format=%i %n%L",
                "--delete",
                "/s",
                "/d"
            ]
        );
    }

    #[test]
    fn delete_off_by_default() {
        let job = Job::new("/s", "/d");
        assert!(!job.delete);
        assert!(!job.build_argv(Mode::Sync).iter().any(|a| a == "--delete"));
    }

    /// The reported bug: a dropped folder must arrive *as a folder*, not have
    /// its children spill loose into the destination. That means no trailing
    /// slash unless the user explicitly asks for one.
    #[test]
    fn a_lone_folder_is_passed_verbatim_so_it_nests_in_dest() {
        let argv = Job::new("/a/b", "/x").build_argv(Mode::Sync);
        assert!(argv.iter().any(|s| s.as_bytes() == b"/a/b"));
        assert!(
            !argv.iter().any(|s| s.as_bytes() == b"/a/b/"),
            "a trailing slash would spill b's contents into /x: {argv:?}"
        );
    }

    #[test]
    fn sync_contents_opts_into_a_single_trailing_slash() {
        let contents = |p: &str| Job {
            sync_contents: true,
            ..Job::new(p, "/x")
        };
        // no slash -> exactly one added
        let a = contents("/a/b").build_argv(Mode::Sync);
        assert!(a.iter().any(|s| s.as_bytes() == b"/a/b/"));
        // already slashed -> not doubled
        let b = contents("/a/b/").build_argv(Mode::Sync);
        assert!(b.iter().any(|s| s.as_bytes() == b"/a/b/"));
        assert!(!b.iter().any(|s| s.as_bytes() == b"/a/b//"));
    }

    /// `sync_contents` is a single-folder concept: a trailing slash on a file
    /// makes rsync reject it, and on a multi-source collect it is meaningless.
    #[test]
    fn sync_contents_is_ignored_unless_the_source_is_a_lone_folder() {
        let file = Job {
            sources: vec![file_source("/a/notes.txt")],
            dest: PathBuf::from("/x"),
            sync_contents: true,
            ..Default::default()
        };
        let argv = file.build_argv(Mode::Sync);
        assert!(argv.iter().any(|s| s.as_bytes() == b"/a/notes.txt"));
        assert!(!argv.iter().any(|s| s.as_bytes() == b"/a/notes.txt/"));

        let two = Job {
            sources: vec![dir_source("/a/one"), dir_source("/a/two")],
            dest: PathBuf::from("/x"),
            sync_contents: true,
            ..Default::default()
        };
        let argv = two.build_argv(Mode::Sync);
        assert!(argv.iter().any(|s| s.as_bytes() == b"/a/one"));
        assert!(argv.iter().any(|s| s.as_bytes() == b"/a/two"));
        assert!(!argv.iter().any(|s| s.as_bytes().ends_with(b"/one/")));
    }

    #[test]
    fn dest_is_passed_verbatim_without_trailing_slash() {
        let argv = Job::new("/s", "/d/e").build_argv(Mode::Preview);
        assert_eq!(argv.last().unwrap().as_bytes(), b"/d/e");
    }

    fn file_source(path: &str) -> Source {
        Source {
            path: PathBuf::from(path),
            is_dir: false,
        }
    }
    fn dir_source(path: &str) -> Source {
        Source {
            path: PathBuf::from(path),
            is_dir: true,
        }
    }

    #[test]
    fn file_source_has_no_trailing_slash() {
        // A single-file source must be passed verbatim — a trailing slash makes
        // rsync reject it as "not a directory".
        let job = Job {
            sources: vec![file_source("/a/b/notes.txt")],
            dest: PathBuf::from("/x"),
            delete: false,
            ..Default::default()
        };
        let argv = job.build_argv(Mode::Sync);
        assert!(argv.iter().any(|s| s.as_bytes() == b"/a/b/notes.txt"));
        assert!(!argv.iter().any(|s| s.as_bytes() == b"/a/b/notes.txt/"));
    }

    /// By default a folder and a file source look identical in argv — both
    /// verbatim, both landing *inside* dest. Only `sync_contents` separates them.
    #[test]
    fn dir_and_file_sources_are_both_verbatim_by_default() {
        let dir = Job {
            sources: vec![dir_source("/data/x")],
            dest: PathBuf::from("/d"),
            delete: false,
            ..Default::default()
        };
        let file = Job {
            sources: vec![file_source("/data/x")],
            ..dir.clone()
        };
        for job in [&dir, &file] {
            assert!(job
                .build_argv(Mode::Sync)
                .iter()
                .any(|s| s.as_bytes() == b"/data/x"));
        }
        let contents = Job {
            sync_contents: true,
            ..dir
        };
        assert!(contents
            .build_argv(Mode::Sync)
            .iter()
            .any(|s| s.as_bytes() == b"/data/x/"));
    }

    #[test]
    fn multiple_sources_are_each_verbatim_before_dest() {
        // Two items from different locations -> collected into dest; no source
        // gets a trailing slash (even the directory nests as dest/dl/).
        let job = Job {
            sources: vec![
                file_source("/home/u/Downloads/a.txt"),
                dir_source("/home/u/Documents/dl"),
            ],
            dest: PathBuf::from("/backup"),
            delete: false,
            ..Default::default()
        };
        let argv = job.build_argv(Mode::Sync);
        // reversed tail: [dest, source2, source1]
        let tail: Vec<&[u8]> = argv.iter().rev().take(3).map(|s| s.as_bytes()).collect();
        assert_eq!(tail[0], b"/backup");
        assert_eq!(tail[1], b"/home/u/Documents/dl");
        assert_eq!(tail[2], b"/home/u/Downloads/a.txt");
        assert!(!job.is_single_dir());
    }

    #[test]
    fn single_dir_predicate_rejects_two_dirs_and_files() {
        assert!(Job::new("/one", "/d").is_single_dir());
        let two = Job {
            sources: vec![dir_source("/one"), dir_source("/two")],
            dest: PathBuf::from("/d"),
            delete: false,
            ..Default::default()
        };
        assert!(!two.is_single_dir());
        let file = Job {
            sources: vec![file_source("/one")],
            dest: PathBuf::from("/d"),
            ..Default::default()
        };
        assert!(!file.is_single_dir());
    }

    #[test]
    fn out_format_is_one_argv_element_not_shell_split() {
        // The space inside --out-format=%i %n%L must live in a SINGLE argv
        // element; a shell string would have split it into two args.
        let argv = Job::new("/s", "/d").build_argv(Mode::Sync);
        assert!(argv.iter().any(|a| a.as_bytes() == b"--out-format=%i %n%L"));
    }

    #[test]
    fn advanced_flags_are_emitted() {
        let job = Job {
            sources: vec![dir_source("/s")],
            dest: PathBuf::from("/d"),
            verbose: true,
            remove_source_files: true,
            bwlimit: Some("85M".into()),
            filters: vec![FilterRule::exclude("*.tmp"), FilterRule::exclude(".git")],
            extra_args: vec!["--checksum".into(), "--partial".into()],
            ..Default::default()
        };
        let argv = job.build_argv(Mode::Sync);
        let has = |s: &str| argv.iter().any(|a| a.as_bytes() == s.as_bytes());
        assert!(has("-v"));
        assert!(has("--remove-source-files"));
        assert!(has("--bwlimit=85M"));
        assert!(has("--exclude=*.tmp"));
        assert!(has("--exclude=.git"));
        assert!(has("--checksum"));
        assert!(has("--partial"));
    }

    /// The whole reason filter rules are a list rather than a text field: a
    /// pattern with a space must reach rsync as ONE argument. There is no shell
    /// here, so `--exclude=My Documents/` is unambiguous — but only if nothing
    /// upstream split it first.
    #[test]
    fn a_filter_rule_containing_spaces_stays_a_single_argument() {
        let job = Job {
            sources: vec![dir_source("/s")],
            dest: PathBuf::from("/d"),
            filters: vec![
                FilterRule::exclude("My Documents/"),
                FilterRule::include("Old Backups/**"),
            ],
            ..Default::default()
        };
        let argv = job.build_argv(Mode::Sync);
        let rules: Vec<_> = argv
            .iter()
            .filter(|a| {
                a.as_bytes().starts_with(b"--exclude=") || a.as_bytes().starts_with(b"--include=")
            })
            .collect();
        assert_eq!(rules.len(), 2, "one argument per rule, not per word");
        assert_eq!(rules[0].as_bytes(), b"--exclude=My Documents/");
        assert_eq!(rules[1].as_bytes(), b"--include=Old Backups/**");
    }

    /// Each kind emits its own flag; nothing else in argv changes.
    #[test]
    fn each_filter_kind_emits_its_own_flag() {
        let job = Job {
            sources: vec![dir_source("/s")],
            dest: PathBuf::from("/d"),
            filters: vec![FilterRule::include("*.jpg"), FilterRule::exclude("*")],
            ..Default::default()
        };
        let argv = job.build_argv(Mode::Sync);
        let has = |s: &str| argv.iter().any(|a| a.as_bytes() == s.as_bytes());
        assert!(has("--include=*.jpg"));
        assert!(has("--exclude=*"));
    }

    /// The point of an ordered list rather than an includes set plus an
    /// excludes set: rsync takes the FIRST rule that matches, so the two orders
    /// below mean opposite things and both must be expressible. A two-list UI
    /// (all includes, then all excludes) could only ever produce the first.
    #[test]
    fn filter_rules_reach_argv_in_list_order() {
        let filters_of = |argv: &[OsString]| -> Vec<String> {
            argv.iter()
                .map(|a| a.to_string_lossy().into_owned())
                .filter(|a| a.starts_with("--include=") || a.starts_with("--exclude="))
                .collect()
        };

        // "Only JPEGs" — the include must precede the catch-all exclude.
        let only_jpegs = Job {
            sources: vec![dir_source("/s")],
            dest: PathBuf::from("/d"),
            filters: vec![
                FilterRule::include("*/"),
                FilterRule::include("*.jpg"),
                FilterRule::exclude("*"),
            ],
            ..Default::default()
        };
        assert_eq!(
            filters_of(&only_jpegs.build_argv(Mode::Sync)),
            vec!["--include=*/", "--include=*.jpg", "--exclude=*"]
        );

        // "JPEGs, but nothing under build/" — the exclude must precede the
        // include, which is precisely what an includes-first model cannot say.
        let not_in_build = Job {
            sources: vec![dir_source("/s")],
            dest: PathBuf::from("/d"),
            filters: vec![
                FilterRule::exclude("build/"),
                FilterRule::include("*.jpg"),
                FilterRule::exclude("*"),
            ],
            ..Default::default()
        };
        assert_eq!(
            filters_of(&not_in_build.build_argv(Mode::Sync)),
            vec!["--exclude=build/", "--include=*.jpg", "--exclude=*"]
        );
    }

    /// Preview and Sync must filter identically, or the dry run stops
    /// predicting the transfer — the app's whole premise.
    #[test]
    fn both_modes_get_the_same_filter_rules() {
        let job = Job {
            sources: vec![dir_source("/s")],
            dest: PathBuf::from("/d"),
            filters: vec![FilterRule::include("*.jpg"), FilterRule::exclude("*")],
            ..Default::default()
        };
        let rules = |mode| -> Vec<String> {
            job.build_argv(mode)
                .iter()
                .map(|a| a.to_string_lossy().into_owned())
                .filter(|a| a.starts_with("--include=") || a.starts_with("--exclude="))
                .collect()
        };
        assert_eq!(rules(Mode::Preview), rules(Mode::Sync));
    }

    /// `-e` is two argv elements, and the command stays whole in the second.
    /// rsync re-tokenises that string itself, so it must arrive exactly as
    /// `ssh.rs` quoted it — anything splitting it here would silently drop the
    /// host-key policy and fall back to ssh's defaults.
    #[test]
    fn the_remote_shell_reaches_argv_as_one_element() {
        let shell = "ssh -o 'UserKnownHostsFile=/a b/known_hosts' -o StrictHostKeyChecking=yes";
        let job = Job {
            sources: vec![dir_source("/s")],
            dest: PathBuf::from("/d"),
            remote_shell: Some(shell.into()),
            ..Default::default()
        };
        let argv = job.build_argv(Mode::Sync);
        let at = argv
            .iter()
            .position(|a| a.as_bytes() == b"-e")
            .expect("-e emitted");
        assert_eq!(argv[at + 1].as_bytes(), shell.as_bytes());
    }

    #[test]
    fn no_remote_shell_means_no_dash_e() {
        let argv = Job::new("/s", "/d").build_argv(Mode::Sync);
        assert!(!argv.iter().any(|a| a.as_bytes() == b"-e"));
    }

    /// The escape hatch has to stay an escape hatch: a `-e` typed into Extra
    /// arguments is emitted later, and rsync takes the last one.
    #[test]
    fn a_user_supplied_remote_shell_still_wins() {
        let job = Job {
            sources: vec![dir_source("/s")],
            dest: PathBuf::from("/d"),
            remote_shell: Some("ssh -o StrictHostKeyChecking=yes".into()),
            extra_args: vec!["-e".into(), "ssh -p 2222".into()],
            ..Default::default()
        };
        let argv = job.build_argv(Mode::Sync);
        let positions: Vec<_> = argv
            .iter()
            .enumerate()
            .filter(|(_, a)| a.as_bytes() == b"-e")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(positions.len(), 2, "both are emitted: {argv:?}");
        assert_eq!(argv[positions[1] + 1].as_bytes(), b"ssh -p 2222");
    }

    #[test]
    fn bwlimit_none_or_empty_omits_the_flag() {
        for bw in [None, Some(String::new())] {
            let job = Job {
                sources: vec![dir_source("/s")],
                dest: PathBuf::from("/d"),
                bwlimit: bw,
                ..Default::default()
            };
            assert!(!job
                .build_argv(Mode::Sync)
                .iter()
                .any(|a| a.as_bytes().starts_with(b"--bwlimit")));
        }
    }

    #[test]
    fn bwlimit_carries_the_unit_suffix() {
        // 85 MB/s must reach rsync as --bwlimit=85M, not a raw KB number.
        let job = Job {
            sources: vec![dir_source("/s")],
            dest: PathBuf::from("/d"),
            bwlimit: Some("85M".into()),
            ..Default::default()
        };
        assert!(job
            .build_argv(Mode::Sync)
            .iter()
            .any(|a| a.as_bytes() == b"--bwlimit=85M"));
    }

    #[test]
    fn reporting_flags_come_after_user_flags_so_they_win() {
        // A user who types --info=... in extra args must not defeat our
        // --info=progress2: ours is emitted later, and rsync takes the last.
        let job = Job {
            sources: vec![dir_source("/s")],
            dest: PathBuf::from("/d"),
            extra_args: vec!["--info=flist2".into()],
            ..Default::default()
        };
        let argv = job.build_argv(Mode::Sync);
        let user = argv.iter().position(|a| a.as_bytes() == b"--info=flist2");
        let ours = argv
            .iter()
            .position(|a| a.as_bytes() == b"--info=progress2");
        assert!(user.unwrap() < ours.unwrap(), "ours must be last: {argv:?}");
    }

    #[test]
    fn advanced_flags_precede_the_paths() {
        let job = Job {
            sources: vec![file_source("/s/a.txt")],
            dest: PathBuf::from("/d"),
            filters: vec![FilterRule::exclude("*.bak")],
            ..Default::default()
        };
        let argv = job.build_argv(Mode::Sync);
        let exclude = argv.iter().position(|a| a.as_bytes() == b"--exclude=*.bak");
        let src = argv.iter().position(|a| a.as_bytes() == b"/s/a.txt");
        assert!(exclude.unwrap() < src.unwrap());
    }

    #[test]
    fn non_utf8_path_is_preserved_byte_for_byte() {
        use std::os::unix::ffi::OsStrExt;
        let src = PathBuf::from(OsStr::from_bytes(b"/bad/\xff\xfename"));
        let argv = Job::new(&src, "/d").build_argv(Mode::Preview);
        assert!(argv.iter().any(|a| a.as_bytes() == b"/bad/\xff\xfename"));
        // ...and the opt-in slash is appended to the raw bytes, not to a lossy
        // UTF-8 round-trip of them.
        let contents = Job {
            sync_contents: true,
            ..Job::new(&src, "/d")
        };
        assert!(contents
            .build_argv(Mode::Preview)
            .iter()
            .any(|a| a.as_bytes() == b"/bad/\xff\xfename/"));
    }

    // -- engine runner: drives real rsync through spawn_rsync ---------------

    fn rsync_available() -> bool {
        std::process::Command::new("rsync")
            .arg("--version")
            .output()
            .is_ok()
    }

    /// End-to-end: spawn real rsync via `spawn_rsync`, pump a glib main loop,
    /// and assert the streamed events, the mapped completion, and the actual
    /// file copy. Exercises the incremental reader + StreamParser wiring.
    #[test]
    fn spawn_rsync_streams_events_and_copies() {
        if !rsync_available() {
            eprintln!("skipping: rsync not on PATH");
            return;
        }

        let tmp = std::env::temp_dir().join(format!("foresight-test-{}", std::process::id()));
        let src = tmp.join("src");
        let dst = tmp.join("dst");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("a.txt"), b"hello world").unwrap();
        std::fs::write(src.join("sub/b.txt"), vec![b'x'; 4096]).unwrap();

        let changes: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let saw_progress = Rc::new(Cell::new(false));
        let completion: Rc<RefCell<Option<Completion>>> = Rc::new(RefCell::new(None));

        let job = Job {
            sources: vec![Source {
                path: src.clone(),
                is_dir: true,
            }],
            dest: dst.clone(),
            delete: false,
            ..Default::default()
        };

        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let main_loop = glib::MainLoop::new(Some(&ctx), false);
            {
                let changes = changes.clone();
                let saw_progress = saw_progress.clone();
                let completion = completion.clone();
                let ml = main_loop.clone();
                let on_event = move |ev: Event| match ev {
                    Event::Change(c) => changes.borrow_mut().push(c.path),
                    Event::Progress(_) => saw_progress.set(true),
                    Event::Message(_) => {}
                };
                let on_done = move |c: Completion| {
                    *completion.borrow_mut() = Some(c);
                    ml.quit();
                };
                spawn_rsync(job.build_argv(Mode::Sync), on_event, on_done).expect("spawn rsync");
            }

            // Safety valve so a hung child can't wedge the test suite.
            let ml_timeout = main_loop.clone();
            glib::timeout_add_seconds_local_once(30, move || ml_timeout.quit());
            main_loop.run();
        })
        .expect("run with thread-default context");

        let completion = completion.borrow().clone().expect("on_done fired");
        assert_eq!(completion.severity, Severity::Success, "{completion:?}");
        assert_eq!(completion.code, Some(0));

        let changes = changes.borrow();
        assert!(
            changes.iter().any(|p| p == "src/a.txt"),
            "expected src/a.txt in itemized changes, got {changes:?}"
        );
        assert!(saw_progress.get(), "expected at least one progress event");

        // The folder arrived as a folder: dst/src/…, not dst/… .
        assert_eq!(
            std::fs::read(dst.join("src/a.txt")).unwrap(),
            b"hello world"
        );
        assert!(dst.join("src/sub/b.txt").exists());
        assert!(
            !dst.join("a.txt").exists(),
            "source contents must not spill loose into the destination"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The opt-in inverse: `sync_contents` makes dest a copy of the folder's
    /// children, which is what `--delete` mirroring wants.
    #[test]
    fn spawn_rsync_sync_contents_spills_children_into_dest() {
        if !rsync_available() {
            eprintln!("skipping: rsync not on PATH");
            return;
        }

        let tmp = std::env::temp_dir().join(format!("foresight-contents-{}", std::process::id()));
        let src = tmp.join("src");
        let dst = tmp.join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("a.txt"), b"hello world").unwrap();

        let completion: Rc<RefCell<Option<Completion>>> = Rc::new(RefCell::new(None));
        let job = Job {
            sources: vec![Source {
                path: src.clone(),
                is_dir: true,
            }],
            dest: dst.clone(),
            sync_contents: true,
            ..Default::default()
        };

        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let main_loop = glib::MainLoop::new(Some(&ctx), false);
            let ml = main_loop.clone();
            let comp = completion.clone();
            spawn_rsync(
                job.build_argv(Mode::Sync),
                |_ev| {},
                move |c: Completion| {
                    *comp.borrow_mut() = Some(c);
                    ml.quit();
                },
            )
            .expect("spawn rsync");
            main_loop.run();
        })
        .expect("run with thread-default context");

        let completion = completion.borrow().clone().expect("on_done fired");
        assert_eq!(completion.severity, Severity::Success, "{completion:?}");
        assert_eq!(std::fs::read(dst.join("a.txt")).unwrap(), b"hello world");
        assert!(!dst.join("src").exists(), "contents mode must not nest");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A single-file source (`source_is_dir: false`) lands as `dest/<file>`.
    #[test]
    fn spawn_rsync_copies_a_single_file() {
        if !rsync_available() {
            eprintln!("skipping: rsync not on PATH");
            return;
        }

        let tmp = std::env::temp_dir().join(format!("foresight-file-{}", std::process::id()));
        let src_dir = tmp.join("src");
        let dst = tmp.join("dst");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        // Two files exist in the source dir, but we transfer only ONE of them.
        std::fs::write(src_dir.join("wanted.txt"), b"just me").unwrap();
        std::fs::write(src_dir.join("other.txt"), b"not me").unwrap();

        let completion: Rc<RefCell<Option<Completion>>> = Rc::new(RefCell::new(None));
        let job = Job {
            sources: vec![Source {
                path: src_dir.join("wanted.txt"),
                is_dir: false,
            }],
            dest: dst.clone(),
            delete: false,
            ..Default::default()
        };

        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let main_loop = glib::MainLoop::new(Some(&ctx), false);
            let ml = main_loop.clone();
            let comp = completion.clone();
            spawn_rsync(
                job.build_argv(Mode::Sync),
                |_ev| {},
                move |c: Completion| {
                    *comp.borrow_mut() = Some(c);
                    ml.quit();
                },
            )
            .expect("spawn rsync");
            main_loop.run();
        })
        .expect("run with thread-default context");

        let completion = completion.borrow().clone().expect("on_done fired");
        assert_eq!(completion.severity, Severity::Success, "{completion:?}");

        // The one file landed at dest/wanted.txt; the sibling did NOT come along.
        assert_eq!(std::fs::read(dst.join("wanted.txt")).unwrap(), b"just me");
        assert!(
            !dst.join("other.txt").exists(),
            "single-file transfer must not pull in siblings"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Two files from *different* source directories both land in one dest.
    #[test]
    fn spawn_rsync_collects_files_from_two_locations() {
        if !rsync_available() {
            eprintln!("skipping: rsync not on PATH");
            return;
        }

        let tmp = std::env::temp_dir().join(format!("foresight-multi-{}", std::process::id()));
        let loc_a = tmp.join("downloads");
        let loc_b = tmp.join("documents");
        let dst = tmp.join("dst");
        std::fs::create_dir_all(&loc_a).unwrap();
        std::fs::create_dir_all(&loc_b).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(loc_a.join("from_downloads.txt"), b"A").unwrap();
        std::fs::write(loc_b.join("from_documents.txt"), b"B").unwrap();

        let completion: Rc<RefCell<Option<Completion>>> = Rc::new(RefCell::new(None));
        let job = Job {
            sources: vec![
                Source {
                    path: loc_a.join("from_downloads.txt"),
                    is_dir: false,
                },
                Source {
                    path: loc_b.join("from_documents.txt"),
                    is_dir: false,
                },
            ],
            dest: dst.clone(),
            delete: false,
            ..Default::default()
        };

        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let main_loop = glib::MainLoop::new(Some(&ctx), false);
            let ml = main_loop.clone();
            let comp = completion.clone();
            spawn_rsync(
                job.build_argv(Mode::Sync),
                |_ev| {},
                move |c: Completion| {
                    *comp.borrow_mut() = Some(c);
                    ml.quit();
                },
            )
            .expect("spawn rsync");
            main_loop.run();
        })
        .expect("run with thread-default context");

        let completion = completion.borrow().clone().expect("on_done fired");
        assert_eq!(completion.severity, Severity::Success, "{completion:?}");
        // Both files, from two different locations, are now in dest.
        assert_eq!(std::fs::read(dst.join("from_downloads.txt")).unwrap(), b"A");
        assert_eq!(std::fs::read(dst.join("from_documents.txt")).unwrap(), b"B");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Filter rules against the real engine, not just the argv we build.
    ///
    /// Runs the same tree twice, changing only where `--exclude=build/` sits
    /// relative to `--include=*.jpg`. rsync obeys the first rule that matches,
    /// so the two orders must land different files — this is the behaviour the
    /// ordered list exists to give users, and the assertion that would fail if
    /// anything upstream ever sorted or grouped the rules.
    #[test]
    fn filter_rule_order_decides_what_real_rsync_copies() {
        if !rsync_available() {
            eprintln!("skipping: rsync not on PATH");
            return;
        }

        let tmp = std::env::temp_dir().join(format!("foresight-filters-{}", std::process::id()));
        let src = tmp.join("src");
        std::fs::create_dir_all(src.join("photos")).unwrap();
        std::fs::create_dir_all(src.join("build")).unwrap();
        std::fs::write(src.join("keep.jpg"), b"jpg").unwrap();
        std::fs::write(src.join("notes.txt"), b"txt").unwrap();
        std::fs::write(src.join("photos/a.jpg"), b"jpg").unwrap();
        std::fs::write(src.join("photos/b.txt"), b"txt").unwrap();
        std::fs::write(src.join("build/gen.jpg"), b"jpg").unwrap();

        let run = |dst: &Path, filters: Vec<FilterRule>| {
            std::fs::create_dir_all(dst).unwrap();
            let job = Job {
                sources: vec![Source {
                    path: src.clone(),
                    is_dir: true,
                }],
                dest: dst.to_path_buf(),
                sync_contents: true, // children land directly in dst
                filters,
                ..Default::default()
            };
            let completion: Rc<RefCell<Option<Completion>>> = Rc::new(RefCell::new(None));
            let ctx = glib::MainContext::new();
            ctx.with_thread_default(|| {
                let main_loop = glib::MainLoop::new(Some(&ctx), false);
                let ml = main_loop.clone();
                let comp = completion.clone();
                spawn_rsync(
                    job.build_argv(Mode::Sync),
                    |_ev| {},
                    move |c: Completion| {
                        *comp.borrow_mut() = Some(c);
                        ml.quit();
                    },
                )
                .expect("spawn rsync");
                main_loop.run();
            })
            .expect("run with thread-default context");
            let completion = completion.borrow().clone().expect("on_done fired");
            assert_eq!(completion.severity, Severity::Success, "{completion:?}");
        };

        // "Every JPEG except anything under build/" — the exclude is first, so
        // it decides build/ before the include ever sees what is inside it.
        let guarded = tmp.join("guarded");
        run(
            &guarded,
            vec![
                FilterRule::exclude("build/"),
                FilterRule::include("*/"),
                FilterRule::include("*.jpg"),
                FilterRule::exclude("*"),
            ],
        );
        assert!(guarded.join("keep.jpg").exists(), "a JPEG at the top level");
        assert!(
            guarded.join("photos/a.jpg").exists(),
            "a JPEG in a subfolder"
        );
        assert!(!guarded.join("notes.txt").exists(), "--exclude=* drops it");
        assert!(
            !guarded.join("photos/b.txt").exists(),
            "--exclude=* drops it"
        );
        assert!(
            !guarded.join("build").exists(),
            "the exclude runs before the include, so build/ never opens"
        );

        // The same four rules with the exclude moved last: now --include=*.jpg
        // matches build/gen.jpg first and the file comes through. Nothing but
        // rule order differs between the two runs.
        let open = tmp.join("open");
        run(
            &open,
            vec![
                FilterRule::include("*/"),
                FilterRule::include("*.jpg"),
                FilterRule::exclude("build/"),
                FilterRule::exclude("*"),
            ],
        );
        assert!(
            open.join("build/gen.jpg").exists(),
            "the include now wins, so the same tree yields a different result"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Cancellation maps to `Severity::Cancelled`, not an error wall.
    #[test]
    fn cancel_maps_to_cancelled() {
        if !rsync_available() {
            eprintln!("skipping: rsync not on PATH");
            return;
        }

        let tmp = std::env::temp_dir().join(format!("foresight-cancel-{}", std::process::id()));
        let src = tmp.join("src");
        let dst = tmp.join("dst");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("big.bin"), vec![0u8; 4 * 1024 * 1024]).unwrap();

        // Throttle (100 KB/s) so the single-file transfer streams progress for
        // seconds; we cancel on the first Progress event — deterministically
        // mid-transfer, without depending on wall-clock timers.
        let src_arg = {
            let mut s = src.as_os_str().to_os_string();
            s.push("/");
            s
        };
        let argv: Vec<OsString> = vec![
            OsString::from("-a"),
            OsString::from("--bwlimit=100"),
            OsString::from("--info=progress2"),
            src_arg,
            dst.as_os_str().to_os_string(),
        ];

        let completion: Rc<RefCell<Option<Completion>>> = Rc::new(RefCell::new(None));
        // Shared so the event handler can cancel the runner it belongs to.
        let runner_slot: Rc<RefCell<Option<Runner>>> = Rc::new(RefCell::new(None));

        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let main_loop = glib::MainLoop::new(Some(&ctx), false);
            let ml = main_loop.clone();
            let comp = completion.clone();

            let slot_for_event = runner_slot.clone();
            let on_event = move |ev: Event| {
                if let Event::Progress(_) = ev {
                    if let Some(runner) = slot_for_event.borrow().as_ref() {
                        runner.cancel();
                    }
                }
            };
            let on_done = move |c: Completion| {
                *comp.borrow_mut() = Some(c);
                ml.quit();
            };
            let runner = spawn_rsync(argv, on_event, on_done).expect("spawn rsync");
            *runner_slot.borrow_mut() = Some(runner);
            main_loop.run();
        })
        .expect("run with thread-default context");

        let completion = completion.borrow().clone().expect("on_done fired");
        assert_eq!(completion.severity, Severity::Cancelled, "{completion:?}");

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
