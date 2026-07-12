//! rsyncgui — GTK4/libadwaita frontend for the bundled rsync engine.
//!
//! Milestone 1 replaces this stub with the `adw::Application` entry point and
//! the `window.blp` composite template. For now it is a tiny argv smoke check
//! so the crate has a runnable `main` and the argv contract stays exercised
//! outside the unit tests.

mod job;

use job::{argv_display, Job, Mode};

fn main() {
    let job = Job::new("/example/source", "/example/dest");
    println!(
        "preview: rsync {}",
        argv_display(&job.build_argv(Mode::Preview))
    );
    println!(
        "sync:    rsync {}",
        argv_display(&job.build_argv(Mode::Sync))
    );
}
