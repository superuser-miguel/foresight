//! The main window: a composite template bound to `src/ui/window.blp`.
//!
//! All layout lives in the Blueprint. Rust reaches widgets only through the
//! `#[template_child]` bindings below and drives them with signal handlers;
//! this file adds no widget tree of its own.
//!
//! Milestone 2: the Source and Destination rows open the portal folder picker
//! (`gtk::FileDialog::select_folder`, which GTK routes through the FileChooser
//! portal automatically inside the Flatpak) and accept dropped folders. The
//! path returned by the portal — often `/run/user/$UID/doc/…` for locations
//! outside the sandbox — is stored verbatim for argv and only *cosmetically*
//! shortened for display.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, gio, glib};

/// Which row a selection targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    Source,
    Dest,
}

mod imp {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/CHANGEME/RsyncGUI/window.ui")]
    pub struct RsyncGuiWindow {
        #[template_child]
        pub preview_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub main_stack: TemplateChild<adw::ViewStack>,
        #[template_child]
        pub source_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub dest_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub delete_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub preview_list: TemplateChild<gtk::ListView>,
        #[template_child]
        pub overall_progress: TemplateChild<gtk::ProgressBar>,
        #[template_child]
        pub current_file_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub log_view: TemplateChild<gtk::TextView>,

        /// The real paths handed to rsync argv (never lossy-converted).
        pub source: RefCell<Option<PathBuf>>,
        pub dest: RefCell<Option<PathBuf>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RsyncGuiWindow {
        const NAME: &'static str = "RsyncGuiWindow";
        type Type = super::RsyncGuiWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for RsyncGuiWindow {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup_rows();
        }
    }
    impl WidgetImpl for RsyncGuiWindow {}
    impl WindowImpl for RsyncGuiWindow {}
    impl ApplicationWindowImpl for RsyncGuiWindow {}
    impl AdwApplicationWindowImpl for RsyncGuiWindow {}
}

glib::wrapper! {
    pub struct RsyncGuiWindow(ObjectSubclass<imp::RsyncGuiWindow>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gtk::gio::ActionGroup, gtk::gio::ActionMap, gtk::Accessible,
                    gtk::Buildable, gtk::ConstraintTarget, gtk::Native, gtk::Root,
                    gtk::ShortcutManager;
}

impl RsyncGuiWindow {
    pub fn new(app: &adw::Application) -> Self {
        glib::Object::builder().property("application", app).build()
    }

    /// Wire row activation (portal picker), drag-and-drop, and initial state.
    fn setup_rows(&self) {
        let imp = self.imp();

        // Preview is meaningless until both endpoints are chosen.
        imp.preview_button.set_sensitive(false);

        for (row, endpoint) in [
            (imp.source_row.get(), Endpoint::Source),
            (imp.dest_row.get(), Endpoint::Dest),
        ] {
            // Click / Enter on the row -> portal folder picker.
            row.connect_activated(glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |_| win.choose_folder(endpoint)
            ));

            // Drop a folder onto the row -> select it.
            let drop = gtk::DropTarget::new(gio::File::static_type(), gdk::DragAction::COPY);
            drop.connect_drop(glib::clone!(
                #[weak(rename_to = win)]
                self,
                #[upgrade_or]
                false,
                move |_, value, _, _| {
                    if let Ok(file) = value.get::<gio::File>() {
                        if file
                            .query_file_type(gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
                            == gio::FileType::Directory
                        {
                            win.set_endpoint(endpoint, &file);
                            return true;
                        }
                    }
                    false
                }
            ));
            row.add_controller(drop);
        }
    }

    /// Open the async folder picker for `endpoint`.
    fn choose_folder(&self, endpoint: Endpoint) {
        let title = match endpoint {
            Endpoint::Source => "Select source folder",
            Endpoint::Dest => "Select destination folder",
        };
        let dialog = gtk::FileDialog::builder().title(title).modal(true).build();

        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                // Err = user dismissed the dialog; nothing to do.
                if let Ok(file) = dialog.select_folder_future(Some(&win)).await {
                    win.set_endpoint(endpoint, &file);
                }
            }
        ));
    }

    /// Store the chosen folder and reflect it in the row.
    fn set_endpoint(&self, endpoint: Endpoint, file: &gio::File) {
        let Some(path) = file.path() else {
            return;
        };
        let (subtitle, tooltip) = describe_path(&path);

        let imp = self.imp();
        let row = match endpoint {
            Endpoint::Source => imp.source_row.get(),
            Endpoint::Dest => imp.dest_row.get(),
        };
        row.set_subtitle(&subtitle);
        row.set_tooltip_text(Some(&tooltip));

        match endpoint {
            Endpoint::Source => *imp.source.borrow_mut() = Some(path),
            Endpoint::Dest => *imp.dest.borrow_mut() = Some(path),
        }

        let ready = imp.source.borrow().is_some() && imp.dest.borrow().is_some();
        imp.preview_button.set_sensitive(ready);
    }

    /// Build a [`Job`](crate::job::Job) from the current selection, if both
    /// endpoints are set. Consumed by the engine wiring in Milestone 3.
    #[allow(dead_code)] // wired in Milestone 3
    pub fn current_job(&self) -> Option<crate::job::Job> {
        let imp = self.imp();
        let source = imp.source.borrow().clone()?;
        let dest = imp.dest.borrow().clone()?;
        Some(crate::job::Job {
            source,
            dest,
            delete: imp.delete_row.is_active(),
        })
    }
}

/// Map a real path to `(subtitle, tooltip)`. Portal document paths
/// (`/run/user/$UID/doc/…`) are opaque, so show just the folder name and put
/// the full path in the tooltip; ordinary paths are shown in full.
fn describe_path(path: &std::path::Path) -> (String, String) {
    let full = path.display().to_string();
    let is_doc_portal = path
        .to_str()
        .is_some_and(|s| s.starts_with("/run/user/") && s.contains("/doc/"));

    let subtitle = if is_doc_portal {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| full.clone())
    } else {
        full.clone()
    };
    (subtitle, full)
}
