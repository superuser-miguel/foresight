//! What happens to a run when Foresight itself goes: by quitting, by a signal
//! (SIGTERM from `kill` or a session ending, SIGINT from Ctrl+C in the
//! terminal it was started from, SIGHUP from that terminal closing), by being
//! killed outright, or by crashing.
//!
//! There are two mechanisms, and between them nothing is left running:
//!
//! - **Quitting** is ours. The application's shutdown stops every live run
//!   and waits for it ([`stop_all`]), with each run's own bounded escalation.
//! - **Everything else** is the kernel's. A signal ends the process on the
//!   spot: no `shutdown` is emitted and no `Drop` runs. But every rsync was
//!   started with `PR_SET_PDEATHSIG` (`job::tie_to_this_process`), so the
//!   moment this process is gone the kernel sends rsync SIGTERM, on which
//!   rsync removes its temporary file, takes its own children with it, and
//!   exits.
//!
//! An earlier version handled SIGTERM, SIGINT and SIGHUP itself, through a
//! GLib signal source declared by hand, and ended the process by re-raising
//! the signal. It was removed once the kernel's mechanism was in: with the
//! handlers disabled, every test that signals a process holding a real rsync
//! still passed. What the handlers added was the wait, and a SIGKILL for a
//! run that ignores SIGTERM — which rsync does not. What they cost was ten
//! `unsafe` blocks. The one case they covered and this does not is kept as a
//! test below, so that it stays a known limit rather than a forgotten one.
//!
//! The rest of this module is the means of checking all that from outside: a
//! process is started holding a run, signalled by pid, and what is left of its
//! process tree is looked for in `/proc`.

use crate::job::{Runner, KILL_GRACE, STOP_GRACE};
use gtk::glib;
use std::time::{Duration, Instant};

/// The main loop's own latency, allowed on top of the two graces — the same
/// margin `Runner::stop_and_wait` allows.
const MARGIN: Duration = Duration::from_secs(1);

/// Stop every run and do not return until they have all ended: what the
/// application's shutdown does.
///
/// All of them are asked at once, so the wait is one bounded wait — about
/// [`STOP_GRACE`] + [`KILL_GRACE`] whatever the number of windows — rather
/// than one per run. The escalation is each `Runner`'s own; this only keeps
/// the loop turning for it, as `Runner::stop_and_wait` does for one.
///
/// `on_done` fires for each run from inside this call. The caller holds the
/// runs (they were taken out of their windows), so nothing a completion
/// handler borrows is borrowed here.
pub fn stop_all(runs: &[Runner]) {
    for run in runs {
        run.stop();
    }
    let context = glib::MainContext::ref_thread_default();
    let deadline = Instant::now() + STOP_GRACE + KILL_GRACE + MARGIN;
    while runs.iter().any(Runner::is_live) && Instant::now() < deadline {
        if !context.iteration(false) {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Signalling a process from outside and looking at what is left: the half of
/// the tests that is the same whether the process is the real application on
/// a headless display (the widget checks) or a run held on a bare main loop
/// (`cargo test`).
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

    /// Said by the process under test once its run has been started.
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

    /// By `kill(1)` rather than `kill(2)`: the program does what the call
    /// does, and std has no way to send a signal other than SIGKILL. Only to
    /// pids this module started and has not yet waited for, so the number
    /// cannot have been given to anything else.
    pub fn send(pid: u32, signum: i32) {
        let _ = Command::new("kill")
            .arg(format!("-{signum}"))
            .arg(pid.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    /// `program`, started with SIGTERM, SIGINT and SIGHUP meaning what they
    /// mean by default. A test runner may itself have been started with some
    /// of them ignored — a shell starts a background job with SIGINT ignored —
    /// and a process inherits that; one that ignores the signal it is about to
    /// be sent has nothing to show. `env` resets them and then becomes the
    /// program, so the pid is the program's.
    pub fn command(program: &std::ffi::OsStr) -> Command {
        let mut cmd = Command::new("env");
        cmd.arg("--default-signal=TERM,INT,HUP").arg(program);
        cmd
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
                    // Apart, so that each is a signal of its own.
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
/// what is under test is the application itself, holding a run in a window the
/// way it does in use. Each check starts another `foresight` on the same
/// (headless) display and signals it.
#[cfg(feature = "selftest")]
pub(crate) fn selftest() -> (u32, u32) {
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
        let cmd = drive::command(exe.as_os_str());
        drive::signalled(cmd, holding, &base.join(format!("signal-{tag}")), signals)
    };
    let stopped = |o: &drive::Outcome, signum: i32, processes: usize| {
        o.ready && o.tree.len() >= processes && o.left.is_empty() && o.died_of() == Some(signum)
    };

    for (name, signum) in [
        ("SIGTERM", libc::SIGTERM),
        ("SIGINT", libc::SIGINT),
        ("SIGHUP", libc::SIGHUP),
        ("SIGKILL", libc::SIGKILL),
    ] {
        let o = run(name, Holding::Transfer, &[signum]);
        check(
            &format!("{name} to foresight with a transfer live leaves no rsync"),
            stopped(&o, signum, 3),
            format!("{o:?}"),
        );
    }
    let o = run("dry-run", Holding::DryRun, &[libc::SIGTERM]);
    check(
        "SIGTERM to foresight with a dry run live leaves no rsync",
        stopped(&o, libc::SIGTERM, 3),
        format!("{o:?}"),
    );
    let o = run("idle", Holding::Nothing, &[libc::SIGTERM]);
    check(
        "SIGTERM with nothing running ends foresight at once",
        stopped(&o, libc::SIGTERM, 1) && o.took < std::time::Duration::from_secs(2),
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
    /// started again with [`drive::HOLD`] set, holding a run on a bare main
    /// loop and doing nothing about any signal — as the application does
    /// nothing. Without the variable it returns at once, which is all
    /// `cargo test` sees of it.
    #[test]
    fn the_process_under_test() {
        let Some((holding, dir)) = Holding::from_env() else {
            return;
        };
        let ctx = glib::MainContext::new();
        ctx.with_thread_default(|| {
            let main_loop = glib::MainLoop::new(Some(&ctx), false);
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
            stop_all(&runs);
        })
        .expect("run with thread-default context");
        // Reached only if nothing signalled it.
        std::process::exit(3)
    }

    fn signal(tag: &str, holding: Holding, signals: &[i32]) -> Outcome {
        let dir =
            std::env::temp_dir().join(format!("foresight-signal-{tag}-{}", std::process::id()));
        let exe = std::env::current_exe().expect("own path");
        let mut cmd = drive::command(exe.as_os_str());
        cmd.args([
            "signals::tests::the_process_under_test",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ]);
        drive::signalled(cmd, holding, &dir, signals)
    }

    /// Nothing of ours runs when the process is signalled — no handler, no
    /// shutdown, no `Drop` — so whatever stops rsync is not code in this
    /// process. It is the kernel, asked to by `tie_to_this_process` when the
    /// run was spawned.
    fn a_live_transfer_does_not_outlive(tag: &str, signum: i32) {
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
        assert_eq!(o.died_of(), Some(signum), "{o:?}");
        assert!(
            o.left.is_empty(),
            "rsync outlived the process that started it: {o:?}"
        );
    }

    #[test]
    fn a_transfer_does_not_outlive_a_process_sent_sigterm() {
        a_live_transfer_does_not_outlive("term", libc::SIGTERM);
    }

    #[test]
    fn a_transfer_does_not_outlive_a_process_sent_sigint() {
        a_live_transfer_does_not_outlive("int", libc::SIGINT);
    }

    #[test]
    fn a_transfer_does_not_outlive_a_process_sent_sighup() {
        a_live_transfer_does_not_outlive("hup", libc::SIGHUP);
    }

    /// The one no handler could ever have seen coming.
    #[test]
    fn a_transfer_does_not_outlive_a_process_killed_outright() {
        a_live_transfer_does_not_outlive("kill", libc::SIGKILL);
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

    /// The known limit, kept as a test so that it stays known. What the kernel
    /// sends the run is SIGTERM, and a process that ignores SIGTERM is not
    /// stopped by it. rsync does not ignore it; this stand-in does, and is
    /// still there afterwards. (Closing the window is another matter: that
    /// path is ours, and escalates to SIGKILL — see `Runner::stop`.)
    ///
    /// If this ever fails, the limit has gone and the module's opening
    /// comment should say so.
    #[test]
    fn a_run_that_ignores_sigterm_is_the_one_thing_left_behind() {
        let o = signal("deaf", Holding::Deaf, &[libc::SIGTERM]);
        assert!(o.ready, "the stand-in never started: {o:?}");
        assert_eq!(o.died_of(), Some(libc::SIGTERM), "{o:?}");
        assert!(
            !o.left.is_empty(),
            "a process that ignores SIGTERM was stopped all the same: {o:?}"
        );
    }
}
