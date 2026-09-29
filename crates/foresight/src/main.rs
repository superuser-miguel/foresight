//! foresight — GTK4/libadwaita frontend for the bundled rsync engine.
//!
//! Entry point: register the compiled GResource, start an `adw::Application`,
//! and present the composite-template window. Milestone 3 wires the bundled
//! rsync engine to the Preview and Transfer pages.

// One function in this crate is allowed `unsafe`, and says so where it is:
// `job::tie_to_this_process`. Anything else has to ask.
#![deny(unsafe_code)]

mod capabilities;
mod change_object;
mod endpoint;
mod help;
mod job;
mod log_object;
mod profiles;
mod remote_dialog;
mod shortcuts;
mod signals;
mod ssh;
mod window;

mod config {
    include!(concat!(env!("OUT_DIR"), "/config.rs"));
}

use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use std::path::PathBuf;
use window::ForesightWindow;

fn main() -> glib::ExitCode {
    register_resources();

    let app = adw::Application::builder()
        .application_id(config::APP_ID)
        .build();
    // A process started by the signal checks to be signalled. It is an
    // application of its own, not a second window of the one running them.
    #[cfg(feature = "selftest")]
    let held = signals::drive::Holding::from_env();
    #[cfg(feature = "selftest")]
    if held.is_some() {
        app.set_flags(gio::ApplicationFlags::NON_UNIQUE);
    }

    app.connect_startup(|app| {
        setup_actions(app);
        shortcuts::register(app);
        load_css();
    });
    app.connect_activate(move |app| {
        let window = ForesightWindow::new(app);
        if config::PROFILE == "development" {
            // libadwaita renders the striped "devel" header for unreleased builds.
            window.add_css_class("devel");
        }
        window.present();
        #[cfg(feature = "selftest")]
        {
            if let Some((holding, dir)) = &held {
                hold_for_the_signal_checks(app, &window, *holding, dir);
                return;
            }
            let (mut pass, mut fail) = window.run_selftest();
            let (p, f) = signals::selftest();
            pass += p;
            fail += f;
            println!("\n----- selftest: {pass} passed, {fail} failed -----");
            SELFTEST_FAILED.store(fail > 0, std::sync::atomic::Ordering::SeqCst);
            app.quit();
        }
    });

    // Quitting the application closes no window, so it asks nothing and
    // passes no guard: `close-request` never fires. A run that is still live
    // is stopped before the process that started it goes. (A signal, a kill
    // and a crash do not come through here or anywhere else of ours; what
    // stops rsync then is the kernel — see `signals`.)
    app.connect_shutdown(|app| {
        let runs: Vec<job::Runner> = app
            .windows()
            .into_iter()
            .filter_map(|window| window.downcast::<ForesightWindow>().ok())
            .filter_map(|window| window.take_run_for_shutdown())
            .collect();
        signals::stop_all(&runs);
    });
    let code = app.run();

    // A non-zero exit is what makes CI notice a widget regression.
    #[cfg(feature = "selftest")]
    if SELFTEST_FAILED.load(std::sync::atomic::Ordering::SeqCst) {
        return glib::ExitCode::FAILURE;
    }
    code
}

/// What a process started by the signal checks does instead of running the
/// checks: holds a run in its window, says so, and waits to be signalled.
#[cfg(feature = "selftest")]
fn hold_for_the_signal_checks(
    app: &adw::Application,
    window: &ForesightWindow,
    holding: signals::drive::Holding,
    dir: &std::path::Path,
) {
    use signals::drive::{self, Holding};
    let plain = job::RunKind::Transfer {
        moves: false,
        deletes: false,
    };
    match holding {
        Holding::Nothing => {}
        Holding::DryRun => window.hold_run_for_selftest(job::RunKind::DryRun, drive::argv(dir)),
        Holding::Transfer | Holding::Deaf => window.hold_run_for_selftest(plain, drive::argv(dir)),
    }
    drive::say_ready(dir);
    // Nothing here may outlive the checks, signalled or not.
    glib::timeout_add_seconds_local_once(
        60,
        glib::clone!(
            #[weak]
            app,
            move || app.quit()
        ),
    );
}

#[cfg(feature = "selftest")]
static SELFTEST_FAILED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Load `foresight.gresource`. In an installed build it lives in `PKGDATADIR`;
/// for host dev runs, point `FORESIGHT_GRESOURCE` at the file Meson built.
fn register_resources() {
    let path = std::env::var_os("FORESIGHT_GRESOURCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(config::PKGDATADIR).join("foresight.gresource"));

    let resource = gio::Resource::load(&path)
        .unwrap_or_else(|e| panic!("failed to load GResource at {}: {e}", path.display()));
    gio::resources_register(&resource);
}

/// App-wide styling: a chunkier transfer progress bar with a larger, bolder
/// percentage/time readout above it.
fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(
        "progressbar.foresight-progress > trough,
         progressbar.foresight-progress > trough > progress { min-height: 30px; }
         progressbar.foresight-progress > trough > progress { border-radius: 8px; }
         progressbar.foresight-progress > text {
             font-size: 1.2em;
             font-weight: bold;
             margin-bottom: 4px;
         }",
    );
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn setup_actions(app: &adw::Application) {
    let about = gio::SimpleAction::new("about", None);
    about.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| {
            let window = app.active_window();
            adw::AboutDialog::builder()
                .application_name("Foresight")
                .application_icon(config::APP_ID)
                .version(config::VERSION)
                .developer_name("The Foresight contributors")
                .license_type(gtk::License::Gpl30)
                .build()
                .present(window.as_ref());
        }
    ));
    app.add_action(&about);

    // Quit closes the windows rather than calling `app.quit()`, which tears
    // the application down without asking them. Closing is what the window's
    // own close button does, so whatever a window comes to say about being
    // closed — mid-transfer, say — applies to Ctrl+Q as well.
    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| {
            for window in app.windows() {
                window.close();
            }
        }
    ));
    app.add_action(&quit);
}
