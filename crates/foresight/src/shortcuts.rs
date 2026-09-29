//! The keyboard-shortcut table — the single source for *which* keys Foresight
//! answers to.
//!
//! Three things read it and nothing else decides a key: [`register`] hands the
//! accelerators to the application, [`present`] renders the Keyboard Shortcuts
//! window, and [`tooltip`] puts the key on the button it stands for. A shortcut
//! that is registered but not listed, or listed but not registered, is therefore
//! not something that can be written — the same arrangement as
//! [`crate::capabilities`] and `build_argv`.
//!
//! Every shortcut is an *action*, never a key handler. The window keeps each
//! action's `enabled` in step with the button it mirrors (one function:
//! `refresh_action_sensitivity`), and a disabled action does not answer its
//! accelerator, so a key can never do what its greyed-out button would not.

use adw::prelude::*;
use gtk::glib;

/// Where a shortcut is listed in the Keyboard Shortcuts window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    /// Running, stopping and resetting a job.
    Job,
    /// Filling in the Configure page.
    Setup,
    /// Moving between the three pages.
    Views,
    /// What every GNOME app has.
    General,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Group::Job => "Job",
            Group::Setup => "Sources and Destination",
            Group::Views => "Views",
            Group::General => "General",
        }
    }

    /// Listing order.
    pub const ORDER: [Group; 4] = [Group::Job, Group::Setup, Group::Views, Group::General];
}

/// One shortcut: the action it activates, the key(s), and how it is listed.
pub struct Shortcut {
    /// Detailed action name, as `set_accels_for_action` takes it
    /// (`win.new-job`, `win.show-page::preview`). `None` for a key GTK
    /// provides by itself — listed so the window is complete, but there is
    /// nothing for us to register.
    pub action: Option<&'static str>,
    /// GTK accelerator strings. The first is the one shown; any others are
    /// the same key by another name (keypad Enter) and are registered but not
    /// listed, since listing them would read as a second shortcut.
    pub accels: &'static [&'static str],
    pub title: &'static str,
    pub group: Group,
}

/// The table. Keys are chosen to stay clear of what a focused entry already
/// does with them: application accelerators are matched *before* the focused
/// widget sees the key, so Ctrl+A/C/V/X/Z, Ctrl+. (emoji), plain Return or
/// Escape here would take those keys away from every text field and popover.
pub const SHORTCUTS: &[Shortcut] = &[
    Shortcut {
        action: Some("win.dry-run"),
        accels: &["<Control>d"],
        title: "Dry Run",
        group: Group::Job,
    },
    // Deliberately not Shift+<the Dry Run key>: a missed Shift must not be the
    // difference between rehearsing a sync and performing it.
    Shortcut {
        action: Some("win.start-sync"),
        accels: &["<Control>Return", "<Control>KP_Enter"],
        title: "Start Sync",
        group: Group::Job,
    },
    // Not plain Escape: it is the key that closes menus and dialogs, and the
    // one people press by reflex. Cancelling a transfer should take a chord.
    Shortcut {
        action: Some("win.cancel-run"),
        accels: &["<Shift>Escape"],
        title: "Cancel the Running Job",
        group: Group::Job,
    },
    Shortcut {
        action: Some("win.new-job"),
        accels: &["<Control>n"],
        title: "New Job",
        group: Group::Job,
    },
    Shortcut {
        action: Some("win.add-folder"),
        accels: &["<Control>o"],
        title: "Add Source Folders",
        group: Group::Setup,
    },
    Shortcut {
        action: Some("win.add-file"),
        accels: &["<Control><Shift>o"],
        title: "Add Source Files",
        group: Group::Setup,
    },
    Shortcut {
        action: Some("win.remote-source"),
        accels: &["<Control>r"],
        title: "Remote Source",
        group: Group::Setup,
    },
    Shortcut {
        action: Some("win.choose-destination"),
        accels: &["<Control>t"],
        title: "Destination Folder",
        group: Group::Setup,
    },
    Shortcut {
        action: Some("win.remote-destination"),
        accels: &["<Control><Shift>r"],
        title: "Remote Destination",
        group: Group::Setup,
    },
    Shortcut {
        action: Some("win.add-filter-rule"),
        accels: &["<Control>l"],
        title: "Add a Filter Rule",
        group: Group::Setup,
    },
    Shortcut {
        action: Some("win.save-preset"),
        accels: &["<Control>s"],
        title: "Save Options as a Preset",
        group: Group::Setup,
    },
    Shortcut {
        action: Some("win.show-page::configure"),
        accels: &["<Alt>1"],
        title: "Configure",
        group: Group::Views,
    },
    Shortcut {
        action: Some("win.show-page::preview"),
        accels: &["<Alt>2"],
        title: "Preview",
        group: Group::Views,
    },
    Shortcut {
        action: Some("win.show-page::transfer"),
        accels: &["<Alt>3"],
        title: "Transfer",
        group: Group::Views,
    },
    Shortcut {
        action: Some("win.capabilities"),
        accels: &["F1"],
        title: "What Foresight Can Do",
        group: Group::General,
    },
    Shortcut {
        action: Some("win.show-help-overlay"),
        accels: &["<Control>question"],
        title: "Keyboard Shortcuts",
        group: Group::General,
    },
    // GTK opens a `primary` menu button on F10 by itself.
    Shortcut {
        action: None,
        accels: &["F10"],
        title: "Main Menu",
        group: Group::General,
    },
    Shortcut {
        action: Some("win.close"),
        accels: &["<Control>w"],
        title: "Close Window",
        group: Group::General,
    },
    Shortcut {
        action: Some("app.quit"),
        accels: &["<Control>q"],
        title: "Quit",
        group: Group::General,
    },
];

/// Hand every accelerator in the table to the application.
pub fn register(app: &impl IsA<gtk::Application>) {
    for s in SHORTCUTS {
        if let Some(action) = s.action {
            app.set_accels_for_action(action, s.accels);
        }
    }
}

/// The key for `action` as the user reads it ("Ctrl+D"), if it has one.
pub fn label(action: &str) -> Option<glib::GString> {
    let shortcut = SHORTCUTS.iter().find(|s| s.action == Some(action))?;
    let (key, mods) = gtk::accelerator_parse(*shortcut.accels.first()?)?;
    Some(gtk::accelerator_get_label(key, mods))
}

/// `text` with the key for `action` appended, the way GNOME tooltips carry it.
pub fn tooltip(text: &str, action: &str) -> String {
    match label(action) {
        Some(key) => format!("{text} ({key})"),
        None => text.to_string(),
    }
}

/// What [`build`] produced. They are presented differently, so the caller has
/// to know which it got.
pub enum Overlay {
    /// `AdwShortcutsDialog`, shown inside the window.
    Dialog(adw::Dialog),
    /// `GtkShortcutsWindow`, a window of its own.
    Window(gtk::Window),
}

/// Whether the libadwaita we are *running* against has `AdwShortcutsDialog`.
fn has_shortcuts_dialog() -> bool {
    (adw::major_version(), adw::minor_version()) >= (1, 8)
}

/// Build the Keyboard Shortcuts window from the table.
///
/// `AdwShortcutsDialog` where the running libadwaita has it (1.8, so every
/// Flatpak build), `GtkShortcutsWindow` — deprecated since GTK 4.18 but still
/// present — where it does not. The choice is made at run time and both are
/// built through `GtkBuilder` by class name, because the crate is compiled
/// against libadwaita 1.5 / GTK 4.12: naming either type in Rust would mean
/// raising those floors, and CI's distribution is below them.
pub fn build() -> Result<Overlay, glib::Error> {
    if has_shortcuts_dialog() {
        build_dialog().map(Overlay::Dialog)
    } else {
        build_window().map(Overlay::Window)
    }
}

fn object<T: IsA<glib::Object>>(xml: &str) -> Result<T, glib::Error> {
    let builder = gtk::Builder::new();
    builder.add_from_string(xml)?;
    builder.object::<T>("overlay").ok_or_else(|| {
        glib::Error::new(
            gtk::BuilderError::InvalidId,
            "the shortcuts overlay is not the expected type",
        )
    })
}

pub fn build_dialog() -> Result<adw::Dialog, glib::Error> {
    object(&definition(
        "AdwShortcutsDialog",
        "",
        "AdwShortcutsSection",
        "AdwShortcutsItem",
    ))
}

pub fn build_window() -> Result<gtk::Window, glib::Error> {
    // GtkShortcutsWindow wants its groups inside a section; one is enough.
    object(&definition(
        "GtkShortcutsWindow",
        "<property name=\"modal\">true</property>\
         <child><object class=\"GtkShortcutsSection\">\
         <property name=\"section-name\">shortcuts</property>",
        "GtkShortcutsGroup",
        "GtkShortcutsShortcut",
    ))
}

/// The builder XML for either widget. They differ in class names and in the
/// extra level `GtkShortcutsWindow` nests its groups in; the rows — a title
/// and an accelerator per table entry — are the same.
fn definition(overlay: &str, open: &str, group_class: &str, item_class: &str) -> String {
    let mut xml = format!("<interface><object class=\"{overlay}\" id=\"overlay\">{open}");
    for group in Group::ORDER {
        xml.push_str(&format!(
            "<child><object class=\"{group_class}\">\
             <property name=\"title\">{}</property>",
            glib::markup_escape_text(group.title())
        ));
        for s in SHORTCUTS.iter().filter(|s| s.group == group) {
            xml.push_str(&format!(
                "<child><object class=\"{item_class}\">\
                 <property name=\"title\">{}</property>\
                 <property name=\"accelerator\">{}</property>\
                 </object></child>",
                glib::markup_escape_text(s.title),
                glib::markup_escape_text(s.accels[0]),
            ));
        }
        xml.push_str("</object></child>");
    }
    if !open.is_empty() {
        xml.push_str("</object></child>");
    }
    xml.push_str("</object></interface>");
    xml
}

/// Build the overlay and show it over `parent`. Returns what was shown, or
/// `None` if it could not be built.
pub fn present(parent: &gtk::Window) -> Option<Overlay> {
    let overlay = match build() {
        Ok(overlay) => overlay,
        Err(e) => {
            glib::g_warning!("foresight", "cannot build the shortcuts window: {e}");
            return None;
        }
    };
    match &overlay {
        Overlay::Dialog(dialog) => dialog.present(Some(parent)),
        Overlay::Window(window) => {
            window.set_transient_for(Some(parent));
            window.present();
        }
    }
    Some(overlay)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// An accelerator reduced to something comparable: modifiers sorted, the
    /// key lower-cased, so `<Shift><Control>O` and `<Control><Shift>o` are one.
    fn normalised(accel: &str) -> String {
        let lower = accel.to_lowercase();
        let (mods, key) = match lower.rfind('>') {
            Some(i) => lower.split_at(i + 1),
            None => ("", lower.as_str()),
        };
        let mut mods: Vec<&str> = mods.split_inclusive('>').collect();
        mods.sort_unstable();
        format!("{}{key}", mods.concat())
    }

    #[test]
    fn no_accelerator_is_assigned_twice() {
        let mut seen: BTreeMap<String, &str> = BTreeMap::new();
        for s in SHORTCUTS {
            for accel in s.accels {
                if let Some(other) = seen.insert(normalised(accel), s.title) {
                    panic!("{accel} is bound to both “{other}” and “{}”", s.title);
                }
            }
        }
    }

    #[test]
    fn no_action_is_listed_twice() {
        let mut seen = std::collections::BTreeSet::new();
        for action in SHORTCUTS.iter().filter_map(|s| s.action) {
            assert!(seen.insert(action), "{action} has two table entries");
        }
    }

    #[test]
    fn every_shortcut_has_a_key_and_a_title() {
        for s in SHORTCUTS {
            assert!(!s.accels.is_empty(), "{} has no accelerator", s.title);
            assert!(!s.title.is_empty(), "{:?} has no title", s.action);
        }
    }

    /// The keys a focused entry, a popover or a dialog already owns. An
    /// application accelerator wins over all of them, so none may appear here.
    #[test]
    fn no_shortcut_takes_a_key_text_editing_needs() {
        const RESERVED: &[&str] = &[
            "<control>a",
            "<control>c",
            "<control>v",
            "<control>x",
            "<control>z",
            "<control><shift>z",
            "<control>period",
            "<control>semicolon",
            "<control>slash",
            "<control>backslash",
            "<control>backspace",
            "<control>delete",
            "return",
            "kp_enter",
            "escape",
            "delete",
            "backspace",
            "tab",
            "space",
            // GTK's inspector.
            "<control><shift>d",
            "<control><shift>i",
        ];
        for s in SHORTCUTS {
            for accel in s.accels {
                assert!(
                    !RESERVED.contains(&normalised(accel).as_str()),
                    "{accel} (“{}”) belongs to GTK",
                    s.title
                );
            }
        }
    }

    #[test]
    fn modifier_order_and_case_do_not_hide_a_duplicate() {
        assert_eq!(
            normalised("<Shift><Control>O"),
            normalised("<Control><Shift>o")
        );
        assert_ne!(normalised("<Control>o"), normalised("<Control><Shift>o"));
        assert_eq!(normalised("F10"), "f10");
    }
}
