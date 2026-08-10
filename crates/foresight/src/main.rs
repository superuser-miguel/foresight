//! foresight — GTK4/libadwaita frontend for the bundled rsync engine.
//!
//! Entry point: register the compiled GResource, start an `adw::Application`,
//! and present the composite-template window. Milestone 3 wires the bundled
//! rsync engine to the Preview and Transfer pages.

mod capabilities;
mod change_object;
mod help;
mod job;
mod log_object;
mod profiles;
// Host-key trust for remote sync. Nothing calls it yet: it lands ahead of the
// endpoint UI (M5 part 3) because it is the part that can be verified against a
// real sshd on its own, and its own tests do exactly that. Drop this allow the
// moment `current_job` can produce a remote job.
#[allow(dead_code)]
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

    app.connect_startup(|app| {
        setup_actions(app);
        load_css();
    });
    app.connect_activate(|app| {
        let window = ForesightWindow::new(app);
        if config::PROFILE == "development" {
            // libadwaita renders the striped "devel" header for unreleased builds.
            window.add_css_class("devel");
        }
        window.present();
        #[cfg(feature = "selftest")]
        {
            let (pass, fail) = window.run_selftest();
            println!("\n----- selftest: {pass} passed, {fail} failed -----");
            SELFTEST_FAILED.store(fail > 0, std::sync::atomic::Ordering::SeqCst);
            app.quit();
        }
    });

    let code = app.run();

    // A non-zero exit is what makes CI notice a widget regression.
    #[cfg(feature = "selftest")]
    if SELFTEST_FAILED.load(std::sync::atomic::Ordering::SeqCst) {
        return glib::ExitCode::FAILURE;
    }
    code
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
}
