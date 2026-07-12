//! The main window: a composite template bound to `src/ui/window.blp`.
//!
//! All layout lives in the Blueprint. Rust reaches widgets only through the
//! `#[template_child]` bindings below and drives them with signal handlers;
//! this file adds no widget tree of its own.
//!
//! Milestone 2 wired the portal folder pickers. Milestone 3 wires the engine:
//! the Preview button runs a dry run and fills the grouped change list; the
//! Start button runs the real sync with live progress; `--delete` requires an
//! explicit confirmation listing the deletions taken from the dry run; and the
//! run can be cancelled.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, gio, glib};

use std::path::PathBuf;

use crate::change_object::ChangeObject;
use crate::job::{argv_display, spawn_rsync, Completion, Job, Mode, Runner, Source};
use rsync_events::{Event, Severity};

/// One source shown in the list: its real path, whether it is a directory,
/// and the row widget representing it (kept so it can be removed).
#[derive(Clone, Debug)]
pub struct SourceEntry {
    path: PathBuf,
    is_dir: bool,
    row: adw::ActionRow,
}

mod imp {
    use super::*;
    use std::cell::{OnceCell, RefCell};

    #[derive(Debug, Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/superuser_miguel/Foresight/window.ui")]
    pub struct ForesightWindow {
        #[template_child]
        pub toast_overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub preview_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub start_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub cancel_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub result_banner: TemplateChild<adw::Banner>,
        #[template_child]
        pub main_stack: TemplateChild<adw::ViewStack>,
        #[template_child]
        pub sources_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub sources_placeholder: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub add_folder_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub add_file_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub dest_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub delete_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub remove_source_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub bwlimit_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub exclude_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub extra_args_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub preview_list: TemplateChild<gtk::ListView>,
        #[template_child]
        pub overall_progress: TemplateChild<gtk::ProgressBar>,
        #[template_child]
        pub current_file_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub log_view: TemplateChild<gtk::TextView>,

        /// The selected sources, in the order added. Real paths for argv.
        pub sources: RefCell<Vec<SourceEntry>>,
        /// The destination directory (never lossy-converted).
        pub dest: RefCell<Option<PathBuf>>,

        /// Backing model for the preview list (holds `ChangeObject`s).
        pub preview_store: OnceCell<gio::ListStore>,
        /// The live rsync process, if one is running (held so it can cancel).
        pub runner: RefCell<Option<Runner>>,
        /// `rsync:` error lines collected during the current run.
        pub run_errors: RefCell<Vec<String>>,
        /// Deletions itemized by the most recent dry run (for confirmation).
        pub deletions: RefCell<Vec<String>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ForesightWindow {
        const NAME: &'static str = "ForesightWindow";
        type Type = super::ForesightWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ForesightWindow {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.setup_rows();
            obj.setup_preview_list();
            obj.setup_actions();
        }
    }
    impl WidgetImpl for ForesightWindow {}
    impl WindowImpl for ForesightWindow {}
    impl ApplicationWindowImpl for ForesightWindow {}
    impl AdwApplicationWindowImpl for ForesightWindow {}
}

glib::wrapper! {
    pub struct ForesightWindow(ObjectSubclass<imp::ForesightWindow>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gtk::gio::ActionGroup, gtk::gio::ActionMap, gtk::Accessible,
                    gtk::Buildable, gtk::ConstraintTarget, gtk::Native, gtk::Root,
                    gtk::ShortcutManager;
}

impl ForesightWindow {
    pub fn new(app: &adw::Application) -> Self {
        glib::Object::builder().property("application", app).build()
    }

    // -- setup --------------------------------------------------------------

    /// Wire the add buttons, the destination row, drag-and-drop, and initial
    /// state.
    fn setup_rows(&self) {
        let imp = self.imp();
        imp.preview_button.set_sensitive(false);
        imp.start_button.set_sensitive(false);

        // Add-source buttons.
        imp.add_folder_button.connect_clicked(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.choose_add_folders()
        ));
        imp.add_file_button.connect_clicked(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.choose_add_files()
        ));

        // Drop files/folders onto the Sources group to add them (multi-file
        // drops arrive as a gdk::FileList).
        let sources_drop =
            gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        sources_drop.connect_drop(glib::clone!(
            #[weak(rename_to = win)]
            self,
            #[upgrade_or]
            false,
            move |_, value, _, _| {
                if let Ok(list) = value.get::<gdk::FileList>() {
                    let mut added = false;
                    for file in list.files() {
                        added |= win.add_source(&file);
                    }
                    return added;
                }
                false
            }
        ));
        imp.sources_group.add_controller(sources_drop);

        // Destination: row body opens the folder picker; drop accepts a folder.
        imp.dest_row.connect_activated(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.choose_dest()
        ));
        let dest_drop = gtk::DropTarget::new(gio::File::static_type(), gdk::DragAction::COPY);
        dest_drop.connect_drop(glib::clone!(
            #[weak(rename_to = win)]
            self,
            #[upgrade_or]
            false,
            move |_, value, _, _| {
                if let Ok(file) = value.get::<gio::File>() {
                    if file.query_file_type(gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
                        == gio::FileType::Directory
                    {
                        win.set_dest(&file);
                        return true;
                    }
                }
                false
            }
        ));
        imp.dest_row.add_controller(dest_drop);

        self.refresh_sources_state();
    }

    /// Build the sectioned preview `ListView`: rows show the change path;
    /// section headers name the `ChangeKind` group; deletions render
    /// destructively.
    fn setup_preview_list(&self) {
        let imp = self.imp();
        let store = gio::ListStore::new::<ChangeObject>();

        // Sort by kind then path so groups are contiguous; the section sorter
        // (kind only) then draws a header per kind.
        let sorter = gtk::CustomSorter::new(|a, b| {
            let a = a.downcast_ref::<ChangeObject>().unwrap();
            let b = b.downcast_ref::<ChangeObject>().unwrap();
            a.kind_order()
                .cmp(&b.kind_order())
                .then_with(|| a.display().cmp(&b.display()))
                .into()
        });
        let section_sorter = gtk::CustomSorter::new(|a, b| {
            let a = a.downcast_ref::<ChangeObject>().unwrap();
            let b = b.downcast_ref::<ChangeObject>().unwrap();
            a.kind_order().cmp(&b.kind_order()).into()
        });

        let sort_model = gtk::SortListModel::new(Some(store.clone()), Some(sorter));
        sort_model.set_section_sorter(Some(&section_sorter));
        let selection = gtk::NoSelection::new(Some(sort_model));

        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let label = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .build();
            item.downcast_ref::<gtk::ListItem>()
                .unwrap()
                .set_child(Some(&label));
        });
        factory.connect_bind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let change = item.item().and_downcast::<ChangeObject>().unwrap();
            let label = item.child().and_downcast::<gtk::Label>().unwrap();
            label.set_label(&change.display());
            if change.destructive() {
                label.add_css_class("error");
            } else {
                label.remove_css_class("error");
            }
        });

        let header_factory = gtk::SignalListItemFactory::new();
        header_factory.connect_setup(|_, item| {
            let label = gtk::Label::builder()
                .xalign(0.0)
                .css_classes(["heading"])
                .build();
            item.downcast_ref::<gtk::ListHeader>()
                .unwrap()
                .set_child(Some(&label));
        });
        header_factory.connect_bind(|_, item| {
            let header = item.downcast_ref::<gtk::ListHeader>().unwrap();
            if let Some(change) = header.item().and_downcast::<ChangeObject>() {
                let label = header.child().and_downcast::<gtk::Label>().unwrap();
                label.set_label(&change.kind_name());
            }
        });

        imp.preview_list.set_model(Some(&selection));
        imp.preview_list.set_factory(Some(&factory));
        imp.preview_list.set_header_factory(Some(&header_factory));
        imp.preview_store
            .set(store)
            .expect("preview_store set once");
    }

    /// Wire the Preview / Start / Cancel buttons.
    fn setup_actions(&self) {
        let imp = self.imp();

        imp.preview_button.connect_clicked(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.run_preview(false)
        ));
        imp.start_button.connect_clicked(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.on_start_clicked()
        ));
        imp.cancel_button.connect_clicked(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| {
                if let Some(runner) = win.imp().runner.borrow().as_ref() {
                    runner.cancel();
                }
            }
        ));

        // "New Job" (win.new-job): clear the whole form to start fresh.
        let new_job = gio::SimpleAction::new("new-job", None);
        new_job.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| win.clear_job()
        ));
        self.add_action(&new_job);

        // The partial-result banner offers a one-tap reset.
        imp.result_banner.set_button_label(Some("New Job"));
        imp.result_banner.connect_button_clicked(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.clear_job()
        ));
    }

    /// Reset the window to an empty Configure page for a new transfer. Ignored
    /// while a run is live.
    fn clear_job(&self) {
        if self.is_running() {
            return;
        }
        let imp = self.imp();

        // Sources: drop every row and the backing list.
        for entry in imp.sources.borrow_mut().drain(..) {
            imp.sources_group.remove(&entry.row);
        }
        // Destination.
        *imp.dest.borrow_mut() = None;
        imp.dest_row.set_subtitle("Not selected");
        imp.dest_row.set_tooltip_text(None);
        imp.delete_row.set_active(false);

        // Advanced options.
        imp.remove_source_row.set_active(false);
        imp.bwlimit_row.set_value(0.0);
        imp.exclude_row.set_text("");
        imp.extra_args_row.set_text("");

        // Results from any previous run.
        if let Some(store) = imp.preview_store.get() {
            store.remove_all();
        }
        imp.run_errors.borrow_mut().clear();
        imp.deletions.borrow_mut().clear();
        imp.log_view.buffer().set_text("");
        imp.overall_progress.set_fraction(0.0);
        imp.overall_progress.set_text(None);
        imp.current_file_label.set_label("");
        imp.result_banner.set_revealed(false);

        imp.main_stack.set_visible_child_name("configure");
        self.refresh_sources_state();
    }

    // -- source list & destination selection -------------------------------

    /// Pick one or more folders (portal) and add them as sources.
    fn choose_add_folders(&self) {
        let dialog = gtk::FileDialog::builder()
            .title("Add source folders")
            .modal(true)
            .build();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                if let Ok(model) = dialog.select_multiple_folders_future(Some(&win)).await {
                    win.add_sources_from_model(&model);
                }
            }
        ));
    }

    /// Pick one or more files (portal) and add them as sources.
    fn choose_add_files(&self) {
        let dialog = gtk::FileDialog::builder()
            .title("Add source files")
            .modal(true)
            .build();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                if let Ok(model) = dialog.open_multiple_future(Some(&win)).await {
                    win.add_sources_from_model(&model);
                }
            }
        ));
    }

    fn add_sources_from_model(&self, model: &gio::ListModel) {
        for i in 0..model.n_items() {
            if let Some(file) = model.item(i).and_downcast::<gio::File>() {
                self.add_source(&file);
            }
        }
    }

    /// Add one source (file or folder) to the list. Returns whether it was
    /// added (rejects paths already present). The real path is kept for argv;
    /// the row shows the name with the full path as subtitle/tooltip.
    fn add_source(&self, file: &gio::File) -> bool {
        let Some(path) = file.path() else {
            return false;
        };
        let imp = self.imp();
        if imp.sources.borrow().iter().any(|e| e.path == path) {
            return false; // already listed
        }

        let is_dir = file.query_file_type(gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
            == gio::FileType::Directory;
        let full = path.display().to_string();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| full.clone());

        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&name))
            .subtitle(glib::markup_escape_text(&full))
            .tooltip_text(&full)
            .build();
        let icon = gtk::Image::from_icon_name(if is_dir {
            "folder-symbolic"
        } else {
            "text-x-generic-symbolic"
        });
        row.add_prefix(&icon);

        let remove = gtk::Button::builder()
            .icon_name("edit-delete-symbolic")
            .tooltip_text("Remove")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        remove.connect_clicked(glib::clone!(
            #[weak(rename_to = win)]
            self,
            #[weak]
            row,
            move |_| win.remove_source(&row)
        ));
        row.add_suffix(&remove);

        imp.sources_group.add(&row);
        imp.sources
            .borrow_mut()
            .push(SourceEntry { path, is_dir, row });
        self.refresh_sources_state();
        true
    }

    fn remove_source(&self, row: &adw::ActionRow) {
        let imp = self.imp();
        imp.sources_group.remove(row);
        imp.sources.borrow_mut().retain(|e| &e.row != row);
        self.refresh_sources_state();
    }

    fn choose_dest(&self) {
        let dialog = gtk::FileDialog::builder()
            .title("Select destination folder")
            .modal(true)
            .build();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = win)]
            self,
            async move {
                if let Ok(file) = dialog.select_folder_future(Some(&win)).await {
                    win.set_dest(&file);
                }
            }
        ));
    }

    fn set_dest(&self, file: &gio::File) {
        let Some(path) = file.path() else {
            return;
        };
        let (subtitle, tooltip) = describe_path(&path);
        let imp = self.imp();
        imp.dest_row.set_subtitle(&subtitle);
        imp.dest_row.set_tooltip_text(Some(&tooltip));
        *imp.dest.borrow_mut() = Some(path);
        self.refresh_action_sensitivity();
    }

    /// Recompute the placeholder, the `--delete` availability (only the
    /// single-directory "mirror" case), and the action-button sensitivity.
    fn refresh_sources_state(&self) {
        let imp = self.imp();
        let sources = imp.sources.borrow();
        imp.sources_placeholder.set_visible(sources.is_empty());

        let is_mirror = sources.len() == 1 && sources[0].is_dir;
        imp.delete_row.set_sensitive(is_mirror);
        if !is_mirror {
            imp.delete_row.set_active(false);
        }
        drop(sources);
        self.refresh_action_sensitivity();
    }

    fn current_job(&self) -> Option<Job> {
        let imp = self.imp();
        let sources: Vec<Source> = imp
            .sources
            .borrow()
            .iter()
            .map(|e| Source {
                path: e.path.clone(),
                is_dir: e.is_dir,
            })
            .collect();
        if sources.is_empty() {
            return None;
        }
        let dest = imp.dest.borrow().clone()?;

        // Advanced options. Text fields are tokenised on whitespace (never
        // shell-interpreted); an empty bandwidth limit means unlimited.
        let excludes = tokenize(&imp.exclude_row.text());
        let extra_args = tokenize(&imp.extra_args_row.text());
        let bwlimit = match imp.bwlimit_row.value() as u32 {
            0 => None,
            kb => Some(kb),
        };

        Some(Job {
            sources,
            dest,
            delete: imp.delete_row.is_active(),
            remove_source_files: imp.remove_source_row.is_active(),
            bwlimit,
            excludes,
            extra_args,
        })
    }

    // -- run lifecycle (M3) -------------------------------------------------

    fn both_selected(&self) -> bool {
        let imp = self.imp();
        !imp.sources.borrow().is_empty() && imp.dest.borrow().is_some()
    }

    fn is_running(&self) -> bool {
        self.imp().runner.borrow().is_some()
    }

    /// Preview and Start follow selection; both are disabled while a run is
    /// live. Cancel is the inverse. The add/remove controls also lock during a
    /// run so the source list can't change mid-transfer.
    fn refresh_action_sensitivity(&self) {
        let imp = self.imp();
        let running = self.is_running();
        let idle_ready = self.both_selected() && !running;
        imp.preview_button.set_sensitive(idle_ready);
        imp.start_button.set_sensitive(idle_ready);
        imp.cancel_button.set_sensitive(running);
        imp.add_folder_button.set_sensitive(!running);
        imp.add_file_button.set_sensitive(!running);
    }

    fn on_start_clicked(&self) {
        let Some(job) = self.current_job() else {
            return;
        };
        if job.delete {
            // Always run a fresh dry run so the confirmation lists exactly the
            // deletions this sync will perform.
            self.run_preview(true);
        } else {
            self.run_sync();
        }
    }

    /// Run `rsync -a -n -i [--delete]` and fill the preview list. When
    /// `then_confirm_start` is set, the deletion-confirmation dialog opens once
    /// the dry run finishes (the Start-with-delete path).
    fn run_preview(&self, then_confirm_start: bool) {
        let Some(job) = self.current_job() else {
            return;
        };
        let imp = self.imp();

        imp.result_banner.set_revealed(false);
        imp.run_errors.borrow_mut().clear();
        imp.deletions.borrow_mut().clear();
        if let Some(store) = imp.preview_store.get() {
            store.remove_all();
        }
        imp.main_stack.set_visible_child_name("preview");

        let argv = job.build_argv(Mode::Preview);
        let on_event = glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |ev: Event| win.on_preview_event(ev)
        );
        let on_done = glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |c: Completion| win.on_preview_done(c, then_confirm_start)
        );

        match spawn_rsync(argv, on_event, on_done) {
            Ok(runner) => {
                *imp.runner.borrow_mut() = Some(runner);
                self.refresh_action_sensitivity();
            }
            Err(e) => self.report_spawn_error(&e),
        }
    }

    fn on_preview_event(&self, ev: Event) {
        let imp = self.imp();
        match ev {
            Event::Change(change) => {
                if change.deleted {
                    imp.deletions.borrow_mut().push(change.path.clone());
                }
                if let Some(store) = imp.preview_store.get() {
                    store.append(&ChangeObject::new(&change));
                }
            }
            Event::Message(m) if m.is_error => imp.run_errors.borrow_mut().push(m.text),
            Event::Message(_) | Event::Progress(_) => {}
        }
    }

    fn on_preview_done(&self, completion: Completion, then_confirm_start: bool) {
        let imp = self.imp();
        *imp.runner.borrow_mut() = None;
        self.refresh_action_sensitivity();

        // A dry run that itself failed (e.g. bad path) shouldn't proceed to a
        // real sync; surface it and stop.
        if completion.severity == Severity::Error {
            self.show_completion(completion);
            return;
        }
        if matches!(completion.severity, Severity::Partial) {
            self.show_banner(&completion.message);
        }

        if then_confirm_start {
            self.confirm_deletions_then_sync();
        } else {
            let n = imp.preview_store.get().map(|s| s.n_items()).unwrap_or(0);
            self.toast(&format!("Preview: {n} change(s)"));
        }
    }

    fn run_sync(&self) {
        let Some(job) = self.current_job() else {
            return;
        };
        let imp = self.imp();

        imp.result_banner.set_revealed(false);
        imp.run_errors.borrow_mut().clear();
        imp.overall_progress.set_fraction(0.0);
        imp.overall_progress.set_text(Some("Starting…"));
        imp.current_file_label.set_label("");
        imp.main_stack.set_visible_child_name("transfer");

        let argv = job.build_argv(Mode::Sync);
        // Show the exact command for transparency (display only — never re-parsed).
        let buffer = imp.log_view.buffer();
        buffer.set_text(&format!("$ rsync {}\n\n", argv_display(&argv)));
        let on_event = glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |ev: Event| win.on_sync_event(ev)
        );
        let on_done = glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |c: Completion| win.on_sync_done(c)
        );

        match spawn_rsync(argv, on_event, on_done) {
            Ok(runner) => {
                *imp.runner.borrow_mut() = Some(runner);
                self.refresh_action_sensitivity();
            }
            Err(e) => self.report_spawn_error(&e),
        }
    }

    fn on_sync_event(&self, ev: Event) {
        let imp = self.imp();
        match ev {
            Event::Change(change) => {
                imp.current_file_label.set_label(&change.path);
            }
            Event::Progress(p) => {
                imp.overall_progress
                    .set_fraction(f64::from(p.percent) / 100.0);
                let text = if p.scanning() {
                    "Scanning…".to_string()
                } else {
                    format!("{}%  ·  {}  ·  {}", p.percent, p.rate_human, p.elapsed)
                };
                imp.overall_progress.set_text(Some(&text));
            }
            Event::Message(m) => {
                if m.is_error {
                    imp.run_errors.borrow_mut().push(m.text.clone());
                }
                let buffer = imp.log_view.buffer();
                let mut end = buffer.end_iter();
                buffer.insert(&mut end, &m.text);
                buffer.insert(&mut end, "\n");
            }
        }
    }

    fn on_sync_done(&self, completion: Completion) {
        let imp = self.imp();
        *imp.runner.borrow_mut() = None;
        self.refresh_action_sensitivity();
        // Completion is process exit, never percent==100 (rsync can end at 99%).
        if completion.severity == Severity::Success {
            imp.overall_progress.set_fraction(1.0);
            imp.overall_progress.set_text(Some("Done"));
        }
        self.show_completion(completion);
    }

    // -- completion / confirmation UI --------------------------------------

    /// Map a [`Completion`] to the right surface: toast (success), banner
    /// (partial 23/24/25 — never a failure wall), toast (cancelled), or a
    /// details dialog (error).
    fn show_completion(&self, completion: Completion) {
        match completion.severity {
            // A finished transfer offers a one-tap reset for the next job.
            Severity::Success => {
                let toast = adw::Toast::builder()
                    .title(&completion.message)
                    .button_label("New Job")
                    .action_name("win.new-job")
                    .build();
                self.imp().toast_overlay.add_toast(toast);
            }
            Severity::Cancelled => self.toast("Sync cancelled."),
            Severity::Partial => self.show_banner(&completion.message),
            Severity::Error => {
                self.show_error_dialog_with_code(&completion.message, completion.code)
            }
        }
    }

    fn confirm_deletions_then_sync(&self) {
        let deletions = self.imp().deletions.borrow().clone();
        if deletions.is_empty() {
            // --delete on, but the dry run found nothing to remove.
            self.run_sync();
            return;
        }

        let body = {
            const MAX: usize = 20;
            let mut lines: Vec<String> = deletions.iter().take(MAX).cloned().collect();
            if deletions.len() > MAX {
                lines.push(format!("…and {} more", deletions.len() - MAX));
            }
            lines.join("\n")
        };

        let dialog = adw::AlertDialog::builder()
            .heading(format!(
                "Delete {} file(s) in the destination?",
                deletions.len()
            ))
            .body(body)
            .build();
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("sync", "Delete and Sync");
        dialog.set_response_appearance("sync", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");

        dialog.connect_response(
            None,
            glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |_, response| {
                    if response == "sync" {
                        win.run_sync();
                    }
                }
            ),
        );
        dialog.present(Some(self));
    }

    fn show_error_dialog(&self, message: &str) {
        self.show_error_dialog_with_code(message, None);
    }

    fn show_error_dialog_with_code(&self, message: &str, code: Option<i32>) {
        let errors = self.imp().run_errors.borrow();
        let mut body = message.to_string();
        if let Some(code) = code {
            body.push_str(&format!("\n\nrsync exit code {code}."));
        }
        if !errors.is_empty() {
            body.push_str("\n\n");
            body.push_str(&errors.join("\n"));
        }
        let dialog = adw::AlertDialog::builder()
            .heading("Sync failed")
            .body(body)
            .build();
        dialog.add_response("ok", "Close");
        dialog.set_default_response(Some("ok"));
        dialog.present(Some(self));
    }

    fn report_spawn_error(&self, error: &glib::Error) {
        *self.imp().runner.borrow_mut() = None;
        self.refresh_action_sensitivity();
        self.show_error_dialog(&format!("Could not start rsync: {error}"));
    }

    fn show_banner(&self, text: &str) {
        let banner = self.imp().result_banner.get();
        banner.set_title(text);
        banner.set_revealed(true);
    }

    fn toast(&self, text: &str) {
        self.imp().toast_overlay.add_toast(adw::Toast::new(text));
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

/// Split a text field into argv tokens on whitespace — same rule as Septima's
/// "Advanced" switches. No shell interpretation, so brace expansion like
/// `{a,b}` does not apply: type patterns/flags separately (`*.tmp *.log`).
fn tokenize(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_string).collect()
}
