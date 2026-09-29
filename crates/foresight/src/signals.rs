//! What happens when Foresight itself is told to go: SIGTERM (`kill`, a
//! session ending), SIGINT (Ctrl+C in the terminal it was started from) and
//! SIGHUP (that terminal closing).
//!
//! Left alone, each of these ends the process on the spot. No `shutdown` is
//! emitted and no `Drop` runs, so an rsync that was transferring carries on
//! with nothing to stop it — and with Move or Mirror deletions on, carries on
//! deleting. Here they are turned into the ordinary way out instead: the
//! application quits, its shutdown stops every live run ([`stop_all`]), and
//! only then does the process end ([`die_of`]).
//!
//! **Nothing is done in signal context.** The signals are watched with GLib's
//! own unix-signal source: its handler only sets a flag and wakes GLib's
//! worker thread, and what is written here runs later as a main-loop callback,
//! like a timeout would. The gtk-rs bindings this crate is locked to (glib and
//! glib-sys 0.22.8) have no `unix_signal_*` and no `g_unix_signal_*` in them,
//! and a crate that does would be a new package, which the offline release
//! build rules out. The function itself is in the libglib this links against
//! whatever the bindings say, so it is declared here and nothing is added.
//!
//! **A second signal** while the runs are being stopped means "now": they are
//! killed rather than waited for, and the process follows as soon as they have
//! gone. It does not mean "leave rsync behind" — there is no signal that makes
//! Foresight exit with rsync running, short of the one that cannot be caught.
//!
//! **What this cannot cover.** SIGKILL, a crash, and a display connection that
//! goes away under GTK (which exits the process itself) run none of this. And
//! the wait below is bounded by [`STOP_GRACE`] + [`KILL_GRACE`], seven seconds
//! at worst, against a deadline that belongs to whoever sent the signal:
//! systemd allows a unit 90 s by default before it kills, which is ample, but
//! a session manager configured for less than it takes rsync to leave —
//! milliseconds normally, the full seven only for an rsync that ignores
//! SIGTERM or is stuck in the kernel — will kill Foresight mid-wait, and
//! whatever rsync had not yet exited is then on its own. Only the kernel can
//! close that gap (`PR_SET_PDEATHSIG` on the child), not this module.

use crate::job::{Runner, KILL_GRACE, STOP_GRACE};
use gtk::glib;
use gtk::glib::translate::from_glib_full;
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The signals that ask a process to end and can be answered.
pub const WATCHED: [i32; 3] = [libc::SIGTERM, libc::SIGINT, libc::SIGHUP];

/// The main loop's own latency, allowed on top of the two graces — the same
/// margin `Runner::stop_and_wait` allows.
const MARGIN: Duration = Duration::from_secs(1);

extern "C" {
    // In libglib-2.0 since 2.30; not declared by glib-sys 0.22.8. Linked
    // already: glib-sys links the library.
    fn g_unix_signal_source_new(signum: std::ffi::c_int) -> *mut glib::ffi::GSource;
}

/// The signals being watched, and what has arrived so far. Dropping it stops
/// the watching, which gives the signals their default meaning back.
pub struct Watch {
    first: Rc<Cell<Option<i32>>>,
    count: Rc<Cell<u32>>,
    sources: Vec<glib::Source>,
}

impl Watch {
    /// The signal that started the shutdown, if one did. It is the one the
    /// process should be seen to have ended by.
    pub fn received(&self) -> Option<i32> {
        self.first.get()
    }

    /// Asked more than once: stop waiting for rsync to leave by itself.
    pub fn repeated(&self) -> bool {
        self.count.get() > 1
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        for source in &self.sources {
            source.destroy();
        }
    }
}

/// Watch [`WATCHED`] on this thread's main context. `on_first` is called, from
/// the main loop, for the first signal to arrive; later ones are only counted
/// (see [`Watch::repeated`]).
///
/// A signal that was being ignored when the process started is left ignored:
/// that is how `nohup`, and a shell starting a job in the background, say that
/// the signal is not meant for this process, and a handler would overrule them.
pub fn watch(on_first: impl Fn(i32) + 'static) -> Watch {
    let first = Rc::new(Cell::new(None));
    let count = Rc::new(Cell::new(0u32));
    let on_first: Rc<dyn Fn(i32)> = Rc::new(on_first);
    let context = glib::MainContext::ref_thread_default();
    let sources = WATCHED
        .into_iter()
        .filter(|signum| !is_ignored(*signum))
        .map(|signum| {
            let source = signal_source(
                signum,
                glib::clone!(
                    #[strong]
                    first,
                    #[strong]
                    count,
                    #[strong]
                    on_first,
                    move || {
                        count.set(count.get().saturating_add(1));
                        if first.get().is_none() {
                            first.set(Some(signum));
                            on_first(signum);
                        }
                    }
                ),
            );
            source.attach(Some(&context));
            source
        })
        .collect();
    Watch {
        first,
        count,
        sources,
    }
}

fn is_ignored(signum: i32) -> bool {
    // SAFETY: a null `act` makes this a query; `old` is ours to be written to.
    unsafe {
        let mut old: libc::sigaction = std::mem::zeroed();
        libc::sigaction(signum, std::ptr::null(), &mut old) == 0
            && old.sa_sigaction == libc::SIG_IGN
    }
}

/// A GLib source that calls `f` on the main loop it is attached to each time
/// `signum` has been delivered to the process. GLib coalesces: several
/// deliveries before the loop comes round are one call.
fn signal_source<F: Fn() + 'static>(signum: i32, f: F) -> glib::Source {
    use glib::thread_guard::ThreadGuard;

    unsafe extern "C" fn call<F: Fn() + 'static>(data: glib::ffi::gpointer) -> glib::ffi::gboolean {
        // SAFETY: `data` is the box made below, alive until `free`.
        let f = unsafe { &*(data as *const ThreadGuard<F>) };
        (f.get_ref())();
        glib::ffi::G_SOURCE_CONTINUE
    }
    unsafe extern "C" fn free<F: Fn() + 'static>(data: glib::ffi::gpointer) {
        // SAFETY: called once by GLib, with the pointer it was given.
        drop(unsafe { Box::from_raw(data as *mut ThreadGuard<F>) });
    }

    // The closure is not `Send`, and need not be: the source is attached to
    // this thread's context and dispatched there. The guard turns a mistake
    // about that into a panic instead of a data race.
    let data = Box::into_raw(Box::new(ThreadGuard::new(f)));
    // SAFETY: the source is new and owned here; the callback and its data
    // have the types `call` and `free` expect.
    unsafe {
        let source = g_unix_signal_source_new(signum);
        glib::ffi::g_source_set_callback(
            source,
            Some(call::<F>),
            data as glib::ffi::gpointer,
            Some(free::<F>),
        );
        from_glib_full(source)
    }
}

/// Stop every run and do not return until they have all ended: what the
/// application's shutdown does, signal or no signal.
///
/// All of them are asked at once, so the wait is one bounded wait — about
/// [`STOP_GRACE`] + [`KILL_GRACE`] whatever the number of windows — rather
/// than one per run. The escalation is each `Runner`'s own; this only keeps
/// the loop turning for it, as `Runner::stop_and_wait` does for one.
///
/// When `hurried` turns true the runs still live are killed at once, and the
/// wait that is left is cut to what a kill is given to show results.
///
/// `on_done` fires for each run from inside this call. The caller holds the
/// runs (they were taken out of their windows), so nothing a completion
/// handler borrows is borrowed here.
pub fn stop_all(runs: &[Runner], hurried: &dyn Fn() -> bool) {
    for run in runs {
        run.stop();
    }
    let context = glib::MainContext::ref_thread_default();
    let mut deadline = Instant::now() + STOP_GRACE + KILL_GRACE + MARGIN;
    let mut killed = false;
    while runs.iter().any(Runner::is_live) && Instant::now() < deadline {
        if !killed && hurried() {
            killed = true;
            for run in runs {
                run.kill();
            }
            deadline = deadline.min(Instant::now() + KILL_GRACE);
        }
        if !context.iteration(false) {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// End the process as having died of `signum`, which is what happened.
///
/// By the signal itself, with its default meaning restored, rather than by an
/// exit status: whoever is waiting — a shell, systemd, `flatpak run` — then
/// sees a process that was terminated, not one that finished its work (0) or
/// failed at it (1). A shell needs exactly this to stop a script on Ctrl+C,
/// and systemd counts a unit that dies of the SIGTERM it sent as stopped
/// cleanly. Nothing that starts the app reads more into it: D-Bus activation
/// and the desktop launcher do not look at how it ended.
///
/// Call it only once there is nothing left to stop. It does not return; if
/// the signal somehow does not end the process (as PID 1 of a namespace, where
/// default actions do not apply), the exit status says the same thing the way
/// a shell would: 128 + the signal's number.
pub fn die_of(signum: i32) -> ! {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    // SAFETY: plain libc calls on a set that is ours; from here on the
    // process is ending and nothing else is running on this thread.
    unsafe {
        libc::signal(signum, libc::SIG_DFL);
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, signum);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
        libc::raise(signum);
    }
    std::process::exit(128 + signum)
}

/// Signalling a process from outside and looking at what is left: the half of
/// the tests that is the same whether the process is the real application on
/// a headless display (the widget checks) or the handlers alone on a bare main
/// loop (`cargo test`).
#[cfg(any(test, feature = "selftest"))]
pub(crate) mod drive {
    use crate::job::procs;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitStatus, Stdio};
    use std::time::{Duration, Instant};

    /// Tells the process under test what to hold, and where.
    pub const HOLD: &str = "FORESIGHT_SIGNAL_HOLD";
    pub const DIR: &str = "FORESIGHT_SIGNAL_DIR";
    const READY: &str = "ready";

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Holding {
        Nothing,
        /// Real rsync, throttled: in the middle of writing whatever happens.
        Transfer,
        /// The same process, held as a dry run is held. (A real `-n` run is
        /// over in milliseconds; what differs on this path is only how the
        /// window takes the result.)
        DryRun,
        /// In rsync's place, by PATH: a process that ignores SIGTERM.
        Deaf,
    }

    impl Holding {
        fn key(self) -> &'static str {
            match self {
                Holding::Nothing => "nothing",
                Holding::Transfer => "transfer",
                Holding::DryRun => "dry-run",
                Holding::Deaf => "deaf",
            }
        }

        /// What this process was started to hold, if it is one under test.
        pub fn from_env() -> Option<(Self, PathBuf)> {
            let key = std::env::var(HOLD).ok()?;
            let dir = PathBuf::from(std::env::var_os(DIR)?);
            [
                Holding::Nothing,
                Holding::Transfer,
                Holding::DryRun,
                Holding::Deaf,
            ]
            .into_iter()
            .find(|h| h.key() == key)
            .map(|h| (h, dir))
        }
    }

    /// The transfer [`prepare`] laid out in `dir`: 4 MiB at 100 KB/s, about
    /// forty seconds if nothing stops it.
    pub fn argv(dir: &Path) -> Vec<OsString> {
        let mut src = dir.join("src").into_os_string();
        src.push("/");
        vec![
            "-a".into(),
            "--bwlimit=100".into(),
            "--info=progress2".into(),
            src,
            dir.join("dst").into_os_string(),
        ]
    }

    /// Said by the process under test once its handlers are in place and its
    /// run has been started.
    pub fn say_ready(dir: &Path) {
        let _ = std::fs::write(dir.join(READY), b"");
    }

    fn prepare(dir: &Path, holding: Holding, cmd: &mut Command) -> std::io::Result<()> {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir.join("src"))?;
        std::fs::create_dir_all(dir.join("dst"))?;
        std::fs::write(dir.join("src/big.bin"), vec![0u8; 4 * 1024 * 1024])?;
        if holding == Holding::Deaf {
            use std::os::unix::fs::PermissionsExt;
            let bin = dir.join("bin");
            std::fs::create_dir_all(&bin)?;
            let stand_in = bin.join("rsync");
            // `exec` keeps it one process; an ignored signal stays ignored
            // across it.
            std::fs::write(
                &stand_in,
                "#!/bin/sh\ntrap '' TERM\necho ready\nexec sleep 600\n",
            )?;
            std::fs::set_permissions(&stand_in, std::fs::Permissions::from_mode(0o755))?;
            let mut path = bin.into_os_string();
            if let Some(rest) = std::env::var_os("PATH") {
                path.push(":");
                path.push(rest);
            }
            cmd.env("PATH", path);
        }
        cmd.env(HOLD, holding.key()).env(DIR, dir);
        Ok(())
    }

    fn comm(pid: u32) -> String {
        std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    }

    /// The run is really under way, judged from outside: rsync has forked its
    /// other half, or the stand-in has become the `sleep` it ends as — which
    /// it does only after it has started ignoring SIGTERM.
    fn holds(pid: u32, holding: Holding) -> bool {
        let named = |name: &str| {
            procs::tree(pid)
                .into_iter()
                .skip(1)
                .filter(|p| comm(*p) == name)
                .count()
        };
        match holding {
            Holding::Nothing => true,
            Holding::Transfer | Holding::DryRun => named("rsync") >= 2,
            Holding::Deaf => named("sleep") >= 1,
        }
    }

    pub fn send(pid: u32, signum: i32) {
        // SAFETY: a pid this module started, and has not yet waited for — so
        // the number cannot have been given to anything else.
        unsafe {
            libc::kill(pid as libc::pid_t, signum);
        }
    }

    #[derive(Debug)]
    pub struct Outcome {
        /// The process said it was ready and was seen holding what it should.
        pub ready: bool,
        /// How it ended; `None` if it had to be killed by the test.
        pub status: Option<ExitStatus>,
        /// From the first signal to its exit.
        pub took: Duration,
        /// It and everything under it, as they were just before the signal.
        pub tree: Vec<u32>,
        /// Those of `tree` still alive a moment after it exited.
        pub left: Vec<u32>,
    }

    impl Outcome {
        pub fn died_of(&self) -> Option<i32> {
            use std::os::unix::process::ExitStatusExt;
            self.status.and_then(|s| s.signal())
        }
    }

    /// Start `cmd` holding `holding`, wait until it is, send it `signals` in
    /// turn, and report. Whatever happens, nothing it started is left running
    /// and `dir` is removed.
    pub fn signalled(mut cmd: Command, holding: Holding, dir: &Path, signals: &[i32]) -> Outcome {
        prepare(dir, holding, &mut cmd).expect("lay out the transfer");
        let mut child = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .expect("start the process under test");
        let pid = child.id();

        let ready_by = Instant::now() + Duration::from_secs(30);
        let mut ready = false;
        while Instant::now() < ready_by && matches!(child.try_wait(), Ok(None)) {
            if dir.join(READY).exists() && holds(pid, holding) {
                ready = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let tree = procs::tree(pid);

        let sent = Instant::now();
        let mut status = None;
        if ready {
            for (i, signum) in signals.iter().enumerate() {
                if i > 0 {
                    // Apart, so that each is a signal of its own: GLib (like
                    // the kernel) makes one of several that arrive together.
                    std::thread::sleep(Duration::from_millis(400));
                }
                send(pid, *signum);
            }
            let exit_by = sent + Duration::from_secs(20);
            while Instant::now() < exit_by {
                if let Ok(Some(s)) = child.try_wait() {
                    status = Some(s);
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let took = sent.elapsed();
        if status.is_none() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let left = procs::survivors(&tree, Duration::from_secs(3));
        for pid in &left {
            send(*pid, libc::SIGKILL);
        }
        procs::survivors(&left, Duration::from_secs(3));
        let _ = std::fs::remove_dir_all(dir);
        Outcome {
            ready,
            status,
            took,
            tree,
            left,
        }
    }
}

/// The real application, signalled: what `cargo test` cannot reach, because
/// what is under test is `main` — the signal leading to `quit`, `quit` to the
/// shutdown, the shutdown to the windows' runs, and the process ending by the
/// signal once they have gone. Each check starts another `foresight` on the
/// same (headless) display, holding a run in a window of its own.
#[cfg(feature = "selftest")]
pub(crate) fn selftest() -> (u32, u32) {
    use crate::job::{KILL_GRACE, STOP_GRACE};
    use drive::Holding;

    let (mut pass, mut fail) = (0u32, 0u32);
    let mut check = |name: &str, cond: bool, detail: String| {
        if cond {
            pass += 1;
            println!("PASS  {name}");
        } else {
            fail += 1;
            println!("FAIL  {name}  ({detail})");
        }
    };
    let base = std::path::PathBuf::from(
        std::env::var("FORESIGHT_SELFTEST_DIR").expect("FORESIGHT_SELFTEST_DIR"),
    );
    let exe = std::env::current_exe().expect("own path");
    let run = |tag: &str, holding: Holding, signals: &[i32]| {
        let cmd = std::process::Command::new(&exe);
        drive::signalled(cmd, holding, &base.join(format!("signal-{tag}")), signals)
    };
    let stopped = |o: &drive::Outcome, signum: i32, processes: usize| {
        o.ready && o.tree.len() >= processes && o.left.is_empty() && o.died_of() == Some(signum)
    };

    for (name, signum) in [
        ("SIGTERM", libc::SIGTERM),
        ("SIGINT", libc::SIGINT),
        ("SIGHUP", libc::SIGHUP),
    ] {
        let o = run(name, Holding::Transfer, &[signum]);
        check(
            &format!("{name} with a transfer live stops rsync, then ends foresight"),
            stopped(&o, signum, 3) && o.took < STOP_GRACE,
            format!("{o:?}"),
        );
    }
    let o = run("dry-run", Holding::DryRun, &[libc::SIGTERM]);
    check(
        "SIGTERM with a dry run live stops rsync, then ends foresight",
        stopped(&o, libc::SIGTERM, 3) && o.took < STOP_GRACE,
        format!("{o:?}"),
    );
    let o = run("idle", Holding::Nothing, &[libc::SIGTERM]);
    check(
        "SIGTERM with nothing running ends foresight at once",
        stopped(&o, libc::SIGTERM, 1) && o.took < std::time::Duration::from_secs(2),
        format!("{o:?}"),
    );
    let o = run("deaf", Holding::Deaf, &[libc::SIGTERM]);
    check(
        "a run that ignores SIGTERM is killed after the grace, and foresight ends",
        stopped(&o, libc::SIGTERM, 2)
            && o.took >= STOP_GRACE
            && o.took < STOP_GRACE + KILL_GRACE + MARGIN,
        format!("{o:?}"),
    );
    let o = run("twice", Holding::Deaf, &[libc::SIGINT, libc::SIGINT]);
    check(
        "a second signal kills the run instead of waiting for it",
        stopped(&o, libc::SIGINT, 2) && o.took < KILL_GRACE,
        format!("{o:?}"),
    );
    (pass, fail)
}

#[cfg(test)]
mod tests {
    use super::drive::{self, Holding, Outcome};
    use super::*;
    use crate::job::spawn_rsync;

    fn rsync_available() -> bool {
        std::process::Command::new("rsync")
            .arg("--version")
            .output()
            .is_ok()
    }

    /// Not a test: the process the tests below signal. It is this test binary
    /// started again with [`drive::HOLD`] set, and does what `main` does with
    /// the application taken out — the handlers on a bare main loop, a run
    /// held, and the same three steps on the way out. Without the variable it
    /// returns at once, which is all `cargo test` sees of it.
    #[test]
    fn the_process_under_test() {
        let Some((holding, dir)) = Holding::from_env() else {
            return;
        };
        // The test runner may itself have been started with some of these
        // ignored (a background job has SIGINT ignored); the process under
        // test must not inherit that, or there is nothing to test.
        for signum in WATCHED {
            // SAFETY: restoring a default disposition.
            unsafe {
                libc::signal(signum, libc::SIG_DFL);
            }
        }
        let ctx = glib::MainContext::new();
        let received = ctx
            .with_thread_default(|| {
                let main_loop = glib::MainLoop::new(Some(&ctx), false);
                let watch = watch(glib::clone!(
                    #[strong]
                    main_loop,
                    move |_| main_loop.quit()
                ));
                let runs: Vec<Runner> = match holding {
                    Holding::Nothing => Vec::new(),
                    _ => vec![spawn_rsync(drive::argv(&dir), |_| {}, |_| {}).expect("spawn")],
                };
                drive::say_ready(&dir);
                // Nothing here may outlive the suite, signalled or not.
                glib::spawn_future_local(glib::clone!(
                    #[strong]
                    main_loop,
                    async move {
                        glib::timeout_future(Duration::from_secs(60)).await;
                        main_loop.quit();
                    }
                ));
                main_loop.run();
                stop_all(&runs, &|| watch.repeated());
                watch.received()
            })
            .expect("run with thread-default context");
        match received {
            Some(signum) => die_of(signum),
            None => std::process::exit(3),
        }
    }

    fn signal(tag: &str, holding: Holding, signals: &[i32]) -> Outcome {
        let dir =
            std::env::temp_dir().join(format!("foresight-signal-{tag}-{}", std::process::id()));
        let mut cmd = std::process::Command::new(std::env::current_exe().expect("own path"));
        cmd.args([
            "signals::tests::the_process_under_test",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ]);
        drive::signalled(cmd, holding, &dir, signals)
    }

    fn a_live_transfer_is_stopped_by(tag: &str, signum: i32) {
        if !rsync_available() {
            eprintln!("skipping: rsync not on PATH");
            return;
        }
        let o = signal(tag, Holding::Transfer, &[signum]);
        assert!(o.ready, "the transfer never got going: {o:?}");
        assert!(
            o.tree.len() >= 3,
            "the process and a local transfer's two, found {:?}",
            o.tree
        );
        assert!(o.left.is_empty(), "still running: {o:?}");
        assert_eq!(o.died_of(), Some(signum), "{o:?}");
        assert!(o.took < STOP_GRACE, "rsync should leave on SIGTERM: {o:?}");
    }

    /// What no handler can see coming: the process killed outright. Nothing
    /// of ours runs — no handler, no shutdown, no `Drop` — so whatever stops
    /// rsync here is not code in this process. It is the kernel, asked to by
    /// `tie_to_this_process` when the run was spawned.
    #[test]
    fn a_hard_kill_of_the_process_still_stops_its_transfer() {
        if !rsync_available() {
            eprintln!("skipping: rsync not on PATH");
            return;
        }
        let o = signal("hard-kill", Holding::Transfer, &[libc::SIGKILL]);
        assert!(o.ready, "the transfer never got going: {o:?}");
        assert!(
            o.tree.len() >= 3,
            "the process and a local transfer's two, found {:?}",
            o.tree
        );
        assert_eq!(o.died_of(), Some(libc::SIGKILL), "{o:?}");
        assert!(
            o.left.is_empty(),
            "rsync outlived the process that started it: {o:?}"
        );
    }

    #[test]
    fn sigterm_stops_a_live_transfer_before_the_process_ends() {
        a_live_transfer_is_stopped_by("term", libc::SIGTERM);
    }

    #[test]
    fn sigint_stops_a_live_transfer_before_the_process_ends() {
        a_live_transfer_is_stopped_by("int", libc::SIGINT);
    }

    #[test]
    fn sighup_stops_a_live_transfer_before_the_process_ends() {
        a_live_transfer_is_stopped_by("hup", libc::SIGHUP);
    }

    #[test]
    fn with_nothing_running_a_signal_ends_the_process_at_once() {
        for (tag, signum) in [
            ("idle-term", libc::SIGTERM),
            ("idle-int", libc::SIGINT),
            ("idle-hup", libc::SIGHUP),
        ] {
            let o = signal(tag, Holding::Nothing, &[signum]);
            assert!(o.ready, "{o:?}");
            assert_eq!(o.died_of(), Some(signum), "{o:?}");
            assert!(o.took < Duration::from_secs(2), "{o:?}");
            assert!(o.left.is_empty(), "{o:?}");
        }
    }

    /// The escalation is still there behind a signal: the run is killed when
    /// the grace is up — not before — and the process ends inside the bound.
    #[test]
    fn a_run_that_ignores_sigterm_is_killed_and_the_process_still_ends() {
        let o = signal("deaf", Holding::Deaf, &[libc::SIGTERM]);
        assert!(o.ready, "the stand-in never started: {o:?}");
        assert!(o.tree.len() >= 2, "{o:?}");
        assert!(o.left.is_empty(), "still running: {o:?}");
        assert_eq!(o.died_of(), Some(libc::SIGTERM), "{o:?}");
        assert!(
            o.took >= STOP_GRACE,
            "gone before the grace was up — SIGTERM was not ignored? {o:?}"
        );
        assert!(o.took < STOP_GRACE + KILL_GRACE + MARGIN, "{o:?}");
    }

    /// Ctrl+C, twice: the run is killed there and then. The process reports
    /// the signal that started it on its way.
    #[test]
    fn a_second_signal_kills_the_run_instead_of_waiting() {
        let o = signal("twice", Holding::Deaf, &[libc::SIGINT, libc::SIGTERM]);
        assert!(o.ready, "the stand-in never started: {o:?}");
        assert!(o.left.is_empty(), "still running: {o:?}");
        assert_eq!(o.died_of(), Some(libc::SIGINT), "{o:?}");
        assert!(o.took < KILL_GRACE, "waited out the grace: {o:?}");
    }
}
