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
//! rsync -a --info=progress2 --out-format='%i %n%L' [--delete] SRC/ DST/   # Sync
//! rsync -a -n -i [--delete] SRC/ DST/                                     # Preview
//! ```

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
    /// Only offered for the single-directory "mirror" case (see [`is_mirror`]).
    ///
    /// [`is_mirror`]: Self::is_mirror
    pub delete: bool,

    // -- Advanced options (all optional; none change rsync's *reporting*
    //    format, so the rsync-events contract is unaffected) ---------------
    /// `--remove-source-files`: delete each source file after it transfers
    /// (turns a copy into a move). No effect during the `-n` dry run.
    pub remove_source_files: bool,
    /// `--bwlimit=<KB/s>`. `None`/`Some(0)` means unlimited (flag omitted).
    pub bwlimit: Option<u32>,
    /// One `--exclude=<pattern>` per entry (filters files; format unchanged).
    pub excludes: Vec<String>,
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

    /// The "mirror" case: exactly one source and it is a directory. Only then
    /// do we copy the source's *contents* into dest (trailing slash) and offer
    /// `--delete`. Any other shape (a file, or several sources) is a "collect"
    /// that drops each item *into* dest.
    pub fn is_mirror(&self) -> bool {
        self.sources.len() == 1 && self.sources[0].is_dir
    }

    /// Build the exact argv for `mode`. The program name (`rsync`) is **not**
    /// included — the caller supplies the bundled binary path to the spawner.
    ///
    /// - Mirror (one directory): the source gets a trailing `/` so rsync copies
    ///   its *contents* into dest rather than nesting the directory inside it.
    /// - Collect (a file, or multiple sources): each source is passed verbatim
    ///   — a trailing slash on a file would make rsync reject it as "not a
    ///   directory" — so a file lands as `dest/<name>` and a folder as
    ///   `dest/<dir>/…`.
    pub fn build_argv(&self, mode: Mode) -> Vec<OsString> {
        let mut argv: Vec<OsString> = Vec::new();
        argv.push(OsString::from("-a"));

        // Optional user flags go first so they can never override the reporting
        // flags below (rsync takes the last value for --info/--out-format).
        if self.remove_source_files {
            argv.push(OsString::from("--remove-source-files"));
        }
        if let Some(kb) = self.bwlimit {
            if kb > 0 {
                argv.push(OsString::from(format!("--bwlimit={kb}")));
            }
        }
        for pattern in &self.excludes {
            argv.push(OsString::from(format!("--exclude={pattern}")));
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

        if self.is_mirror() {
            argv.push(with_trailing_slash(&self.sources[0].path));
        } else {
            for s in &self.sources {
                argv.push(s.path.as_os_str().to_os_string());
            }
        }
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
        assert_eq!(
            as_strs(&argv),
            ["-a", "-n", "-i", "/data/src/", "/data/dst"]
        );
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
                "/data/src/",
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
            ["-a", "-n", "-i", "--delete", "/s/", "/d"]
        );
        assert_eq!(
            as_strs(&job.build_argv(Mode::Sync)),
            [
                "-a",
                "--info=progress2",
                "--out-format=%i %n%L",
                "--delete",
                "/s/",
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

    #[test]
    fn source_gets_single_trailing_slash() {
        // no slash -> one added
        let a = Job::new("/a/b", "/x").build_argv(Mode::Sync);
        assert!(a.iter().any(|s| s.as_bytes() == b"/a/b/"));
        // already slashed -> not doubled
        let b = Job::new("/a/b/", "/x").build_argv(Mode::Sync);
        assert!(b.iter().any(|s| s.as_bytes() == b"/a/b/"));
        assert!(!b.iter().any(|s| s.as_bytes() == b"/a/b//"));
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

    #[test]
    fn dir_vs_file_source_differ_only_by_trailing_slash() {
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
        assert!(dir
            .build_argv(Mode::Sync)
            .iter()
            .any(|s| s.as_bytes() == b"/data/x/"));
        assert!(file
            .build_argv(Mode::Sync)
            .iter()
            .any(|s| s.as_bytes() == b"/data/x"));
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
        assert!(!job.is_mirror());
    }

    #[test]
    fn single_dir_is_mirror_but_two_dirs_are_not() {
        assert!(Job::new("/one", "/d").is_mirror());
        let two = Job {
            sources: vec![dir_source("/one"), dir_source("/two")],
            dest: PathBuf::from("/d"),
            delete: false,
            ..Default::default()
        };
        assert!(!two.is_mirror());
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
            remove_source_files: true,
            bwlimit: Some(500),
            excludes: vec!["*.tmp".into(), ".git".into()],
            extra_args: vec!["--checksum".into(), "--partial".into()],
            ..Default::default()
        };
        let argv = job.build_argv(Mode::Sync);
        let has = |s: &str| argv.iter().any(|a| a.as_bytes() == s.as_bytes());
        assert!(has("--remove-source-files"));
        assert!(has("--bwlimit=500"));
        assert!(has("--exclude=*.tmp"));
        assert!(has("--exclude=.git"));
        assert!(has("--checksum"));
        assert!(has("--partial"));
    }

    #[test]
    fn bwlimit_zero_and_none_omit_the_flag() {
        for bw in [None, Some(0)] {
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
            excludes: vec!["*.bak".into()],
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
        let argv = Job::new(src, "/d").build_argv(Mode::Preview);
        // trailing slash appended, original bytes intact
        assert!(argv.iter().any(|a| a.as_bytes() == b"/bad/\xff\xfename/"));
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
            changes.iter().any(|p| p == "a.txt"),
            "expected a.txt in itemized changes, got {changes:?}"
        );
        assert!(saw_progress.get(), "expected at least one progress event");

        // The bytes really moved.
        assert_eq!(std::fs::read(dst.join("a.txt")).unwrap(), b"hello world");
        assert!(dst.join("sub/b.txt").exists());

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
