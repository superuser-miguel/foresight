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

/// A configured sync. The preview and the real run are built from the *same*
/// `Job`, so the dry run faithfully predicts what the transfer will do —
/// including deletions when [`delete`](Self::delete) is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub source: PathBuf,
    pub dest: PathBuf,
    /// Whether `source` is a directory. A directory source is passed with a
    /// trailing `/` (mirror its *contents* into dest); a single file is passed
    /// verbatim (rsync places it inside the dest directory).
    pub source_is_dir: bool,
    /// Mirror deletions (`--delete`). Off by default; a safety rail in the UI.
    pub delete: bool,
}

impl Job {
    /// Convenience constructor: a directory source, `delete` off. The app
    /// builds `Job` from window state directly; this is used by the unit tests.
    #[allow(dead_code)]
    pub fn new(source: impl Into<PathBuf>, dest: impl Into<PathBuf>) -> Self {
        Self {
            source: source.into(),
            dest: dest.into(),
            source_is_dir: true,
            delete: false,
        }
    }

    /// Build the exact argv for `mode`. The program name (`rsync`) is **not**
    /// included — the caller supplies the bundled binary path to the spawner.
    ///
    /// A directory source gets a trailing `/` so rsync copies its *contents*
    /// into the destination (mirror semantics) rather than nesting the source
    /// directory inside it. A single-file source is passed verbatim — with a
    /// trailing slash rsync would reject it as "not a directory" — so it lands
    /// as `dest/<filename>`.
    pub fn build_argv(&self, mode: Mode) -> Vec<OsString> {
        let mut argv: Vec<OsString> = Vec::new();
        argv.push(OsString::from("-a"));

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

        let src = if self.source_is_dir {
            with_trailing_slash(&self.source)
        } else {
            self.source.as_os_str().to_os_string()
        };
        argv.push(src);
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

    #[test]
    fn file_source_has_no_trailing_slash() {
        // A single-file source must be passed verbatim — a trailing slash makes
        // rsync reject it as "not a directory".
        let job = Job {
            source: PathBuf::from("/a/b/notes.txt"),
            dest: PathBuf::from("/x"),
            source_is_dir: false,
            delete: false,
        };
        let argv = job.build_argv(Mode::Sync);
        assert!(argv.iter().any(|s| s.as_bytes() == b"/a/b/notes.txt"));
        assert!(!argv.iter().any(|s| s.as_bytes() == b"/a/b/notes.txt/"));
    }

    #[test]
    fn dir_vs_file_source_differ_only_by_trailing_slash() {
        let dir = Job {
            source: PathBuf::from("/data/x"),
            dest: PathBuf::from("/d"),
            source_is_dir: true,
            delete: false,
        };
        let file = Job {
            source_is_dir: false,
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
    fn out_format_is_one_argv_element_not_shell_split() {
        // The space inside --out-format=%i %n%L must live in a SINGLE argv
        // element; a shell string would have split it into two args.
        let argv = Job::new("/s", "/d").build_argv(Mode::Sync);
        assert!(argv.iter().any(|a| a.as_bytes() == b"--out-format=%i %n%L"));
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

        let tmp = std::env::temp_dir().join(format!("rsyncgui-test-{}", std::process::id()));
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
            source: src.clone(),
            dest: dst.clone(),
            source_is_dir: true,
            delete: false,
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

        let tmp = std::env::temp_dir().join(format!("rsyncgui-file-{}", std::process::id()));
        let src_dir = tmp.join("src");
        let dst = tmp.join("dst");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        // Two files exist in the source dir, but we transfer only ONE of them.
        std::fs::write(src_dir.join("wanted.txt"), b"just me").unwrap();
        std::fs::write(src_dir.join("other.txt"), b"not me").unwrap();

        let completion: Rc<RefCell<Option<Completion>>> = Rc::new(RefCell::new(None));
        let job = Job {
            source: src_dir.join("wanted.txt"),
            dest: dst.clone(),
            source_is_dir: false,
            delete: false,
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

    /// Cancellation maps to `Severity::Cancelled`, not an error wall.
    #[test]
    fn cancel_maps_to_cancelled() {
        if !rsync_available() {
            eprintln!("skipping: rsync not on PATH");
            return;
        }

        let tmp = std::env::temp_dir().join(format!("rsyncgui-cancel-{}", std::process::id()));
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
