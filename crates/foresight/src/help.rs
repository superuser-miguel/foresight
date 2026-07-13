//! The "What Foresight Can Do" Help surface.
//!
//! A dialog rendered entirely from [`crate::capabilities`] — an honest,
//! per-release inventory of the rsync flags Foresight exposes, the concepts
//! behind Dry Run and Mirror deletions, the boundary of what is *not* exposed
//! (with a pointer to the Extra-arguments escape hatch), and a stamp of the app
//! and **bundled** rsync versions (the version pin is the behavior contract).
//!
//! Nothing here is hand-maintained prose about which flags exist: the
//! capability rows come from the registry, and "Full rsync options" shells the
//! bundled `rsync --help` so the reference is accurate to the shipped engine.

use adw::prelude::*;
use gtk::glib;

use crate::capabilities::{Group, CAPABILITIES, NOT_EXPOSED};

/// Build and present the capabilities dialog over `parent`.
pub fn present(parent: &impl IsA<gtk::Widget>) {
    let dialog = adw::PreferencesDialog::builder()
        .title("What Foresight Can Do")
        .build();

    let page = adw::PreferencesPage::builder()
        .title("Capabilities")
        .icon_name("dialog-information-symbolic")
        .build();

    // One group per registry category, capability rows rendered from the table.
    for group in Group::ORDER {
        let pg = adw::PreferencesGroup::builder().title(group.title()).build();
        for cap in CAPABILITIES.iter().filter(|c| c.group == group) {
            let row = adw::ActionRow::builder()
                .title(cap.name)
                .subtitle(format!(
                    "{}\nvia {} · man {}",
                    cap.description, cap.control, cap.man_option
                ))
                .subtitle_lines(0)
                .build();

            let flags = gtk::Label::builder()
                .label(cap.flags.join(" "))
                .css_classes(["monospace", "dim-label"])
                .valign(gtk::Align::Center)
                .build();
            row.add_suffix(&flags);
            pg.add(&row);
        }
        page.add(&pg);
    }

    // The two concepts worth a plain-language explanation (folded in per plan).
    let concepts = adw::PreferencesGroup::builder()
        .title("Good to know")
        .build();
    concepts.add(&concept_row(
        "Dry Run is always safe",
        "Dry Run really runs rsync, but with --dry-run: nothing is written, moved, \
         or deleted — even Move files and Mirror deletions are inert. It just lists \
         the plan and records any deletions for the confirmation step.",
    ));
    concepts.add(&concept_row(
        "How Mirror deletions stays safe",
        "Mirror deletions (--delete) removes destination files that aren't in the \
         source. It's off by default, offered only for a single-folder mirror, and \
         starting a sync with it on always re-runs a fresh dry run so the \
         confirmation lists exactly what this transfer would remove.",
    ));
    page.add(&concepts);

    // The honest boundary: what isn't a dedicated control yet.
    let boundary = adw::PreferencesGroup::builder()
        .title("Not yet a dedicated control")
        .description("Type any of these in Advanced → Extra arguments; Foresight \
                      passes them straight through to rsync.")
        .build();
    for (flag, desc) in NOT_EXPOSED {
        let row = adw::ActionRow::builder()
            .title(*flag)
            .subtitle(*desc)
            .subtitle_lines(0)
            .build();
        row.add_css_class("property");
        boundary.add(&row);
    }
    page.add(&boundary);

    // Version stamp + the bundled-engine reference action.
    let about = adw::PreferencesGroup::builder()
        .title("This release")
        .description("The pinned rsync version is the behavior contract — these \
                      capabilities describe exactly this build.")
        .build();

    let app_row = adw::ActionRow::builder()
        .title("Foresight")
        .subtitle(crate::config::VERSION)
        .subtitle_selectable(true)
        .build();
    about.add(&app_row);

    let rsync_row = adw::ActionRow::builder()
        .title("Bundled rsync")
        .subtitle(bundled_rsync_version())
        .subtitle_selectable(true)
        .build();
    about.add(&rsync_row);

    let full = adw::ActionRow::builder()
        .title("Full rsync options")
        .subtitle("Show `rsync --help` from the bundled engine")
        .activatable(true)
        .build();
    full.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    full.connect_activated(glib::clone!(
        #[weak]
        dialog,
        move |_| present_full_options(&dialog)
    ));
    about.add(&full);

    page.add(&about);

    dialog.add(&page);
    dialog.present(Some(parent));
}

/// An explanatory row: a title with a wrapping, multi-line body.
fn concept_row(title: &str, body: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(title)
        .subtitle(body)
        .subtitle_lines(0)
        .build()
}

/// First line of the bundled `rsync --version`, e.g. "rsync  version 3.4.4 …".
fn bundled_rsync_version() -> String {
    let out = run_rsync("--version");
    out.lines()
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown")
        .to_string()
}

/// Run the bundled rsync with one argument and return its output (stdout, or
/// stderr / an error note if stdout is empty). rsync resolves via PATH to
/// `/app/bin` inside the sandbox.
fn run_rsync(arg: &str) -> String {
    match std::process::Command::new("rsync").arg(arg).output() {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            if stdout.trim().is_empty() {
                String::from_utf8_lossy(&o.stderr).into_owned()
            } else {
                stdout.into_owned()
            }
        }
        Err(e) => format!("Could not run the bundled rsync: {e}"),
    }
}

/// Show the bundled `rsync --help` verbatim in a scrollable sub-dialog.
fn present_full_options(parent: &impl IsA<gtk::Widget>) {
    let text_view = gtk::TextView::builder()
        .editable(false)
        .monospace(true)
        .left_margin(12)
        .right_margin(12)
        .top_margin(12)
        .bottom_margin(12)
        .build();
    text_view.buffer().set_text(&run_rsync("--help"));

    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .child(&text_view)
        .build();

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&scroll));

    let dialog = adw::Dialog::builder()
        .title("Full rsync options")
        .content_width(720)
        .content_height(600)
        .child(&toolbar)
        .build();
    dialog.present(Some(parent));
}
