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
use crate::endpoint::Endpoint;
use crate::job::{
    argv_display, spawn_rsync, Completion, FilterKind, FilterRule, Job, Mode, Remote, RunKind,
    Runner, Source, TransferTop,
};
use crate::log_object::LogObject;
use crate::profiles::{self, Profile};
use rsync_events::{Event, Severity};

/// One source shown in the list: its real path, whether it is a directory,
/// and the row widget representing it (kept so it can be removed).
#[derive(Clone, Debug)]
pub struct SourceEntry {
    path: PathBuf,
    is_dir: bool,
    row: adw::ActionRow,
}

/// How many paths one filter rule matched in a dry run, by side. The sides are
/// kept apart because they answer different questions: only a *source* match
/// holds a file back from the transfer, and so from `--remove-source-files`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RuleHits {
    /// Paths the sender hid or showed — what the rule did to the transfer.
    source: u64,
    /// Destination paths it protected from, or exposed to, `--delete`.
    dest: u64,
}

impl RuleHits {
    fn total(self) -> u64 {
        self.source + self.dest
    }
}

/// What has to happen before the window may close. Worked out from the run
/// every time it is asked, by [`ForesightWindow::close_step`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseStep {
    /// Nothing is running: close.
    Close,
    /// A dry run is live. It writes nothing, so there is nothing to ask — but
    /// the process is stopped first, and the window closes when it has gone.
    StopThenClose,
    /// A transfer is live: stopping it is the user's decision.
    Ask(RunKind),
    /// Already stopping a run in order to close; the window goes when it ends.
    Wait,
}

/// Which side of the transfer the remote endpoint sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteSide {
    /// Pull: the remote is where files come from.
    Source,
    /// Push: the remote is where they go.
    Dest,
}

mod imp {
    use super::*;
    use std::cell::{Cell, OnceCell, RefCell};

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
        pub menu_button: TemplateChild<gtk::MenuButton>,
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
        pub add_remote_source_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub clear_remote_source_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub remote_dest_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub dest_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub dest_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub advanced_row: TemplateChild<adw::ExpanderRow>,
        #[template_child]
        pub contents_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub delete_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub verbose_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub remove_source_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub bwlimit_row: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub bwlimit_unit_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub filters_row: TemplateChild<adw::ExpanderRow>,
        #[template_child]
        pub filter_entry: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub filter_kind: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub extra_args_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub preset_combo: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub save_preset_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub delete_preset_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub preview_list: TemplateChild<gtk::ListView>,
        #[template_child]
        pub overall_progress: TemplateChild<gtk::ProgressBar>,
        #[template_child]
        pub current_file_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub log_list: TemplateChild<gtk::ListView>,
        #[template_child]
        pub log_scroll: TemplateChild<gtk::ScrolledWindow>,

        /// The selected sources, in the order added. Real paths for argv.
        pub sources: RefCell<Vec<SourceEntry>>,
        /// Filter rules, in the order the user arranged them. This is the model;
        /// the rows in `filters_row` are rebuilt from it on every change, since
        /// `AdwExpanderRow` can append and remove rows but not reorder them.
        pub filters: RefCell<Vec<FilterRule>>,
        /// The rows currently inside `filters_row`, so a rebuild can take them
        /// back out — an `AdwExpanderRow` will not enumerate them for us.
        pub filter_rows: RefCell<Vec<adw::ActionRow>>,
        /// The destination directory (never lossy-converted).
        pub dest: RefCell<Option<PathBuf>>,
        /// The remote endpoint and which side it is on, once confirmed and
        /// trusted. `None` for a purely local job. Only one side can be remote,
        /// so this is one field — see [`crate::job::Remote`].
        pub remote: RefCell<Option<(RemoteSide, Endpoint)>>,

        /// Backing model for the preview list (holds `ChangeObject`s).
        pub preview_store: OnceCell<gio::ListStore>,
        /// Backing model for the streaming transfer log (holds `LogObject`s).
        pub log_store: OnceCell<gio::ListStore>,
        /// The live rsync process, if one is running (held so it can cancel).
        pub runner: RefCell<Option<Runner>>,
        /// A close was asked for and is waiting on the run to end. Set only
        /// while a run is being stopped for that reason, and never cleared:
        /// the next thing that happens to the window is that it closes.
        pub close_when_done: Cell<bool>,
        /// The "stop and close?" question, while it is on screen.
        pub close_dialog: glib::WeakRef<adw::AlertDialog>,
        /// `rsync:` error lines collected during the current run.
        pub run_errors: RefCell<Vec<String>>,
        /// Deletions itemized by the most recent dry run (for confirmation).
        pub deletions: RefCell<Vec<String>>,
        /// What each filter rule matched in the most recent dry run, beside the
        /// rules it ran with. Evidence about *those* rules only: it is shown
        /// while the list still equals that snapshot, and dropped when the
        /// sources change, since either makes it a claim about some other job.
        pub filter_hits: RefCell<Option<(Vec<FilterRule>, Vec<RuleHits>)>>,
        /// The banner is currently the matched-nothing notice (as opposed to a
        /// run result), so editing the rules may take it down.
        pub filter_banner: Cell<bool>,
        /// What the banner's one button does at the moment. The label is set
        /// from this and the click handler reads it, both through
        /// [`super::ForesightWindow::show_banner`], so the two cannot disagree.
        pub banner_button: RefCell<BannerButton>,

        /// Saved Advanced-option presets, in combo order.
        pub profiles: RefCell<Vec<Profile>>,
        /// Set while `profiles` is *not* what is on disk, because what is on
        /// disk could not be read. Cleared by the first save that goes through.
        pub presets_unread: RefCell<Option<profiles::LoadError>>,
        /// Guards the preset combo's notify handler during programmatic rebuilds.
        pub suppress_combo: Cell<bool>,
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
            // First: setup_rows ends by deriving every action's enabled state,
            // so the actions have to exist by then.
            obj.setup_actions();
            obj.setup_rows();
            obj.setup_preview_list();
            obj.setup_log_list();
            obj.setup_presets();
        }
    }
    impl WidgetImpl for ForesightWindow {}
    impl WindowImpl for ForesightWindow {
        /// Everything that closes the window arrives here — the titlebar
        /// button, the compositor, `window.close()` from any action — which is
        /// why the guard for a live run is here and not on any one of them.
        fn close_request(&self) -> glib::Propagation {
            if self.obj().on_close_request() == glib::Propagation::Stop {
                return glib::Propagation::Stop;
            }
            self.parent_close_request()
        }
    }
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
        imp.add_remote_source_button.connect_clicked(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.choose_remote(RemoteSide::Source)
        ));
        imp.remote_dest_button.connect_clicked(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.choose_remote(RemoteSide::Dest)
        ));
        imp.clear_remote_source_button.connect_clicked(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.clear_remote()
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

        // Filter rules: the entry commits on Enter or its apply button, using
        // whichever kind the dropdown beside it is showing.
        imp.filter_entry.connect_apply(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |entry| {
                if win.add_filter(win.selected_filter_kind(), &entry.text()) {
                    entry.set_text("");
                }
            }
        ));
        self.refresh_filters_state();

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

        // "Copy contents" moves what a leading `/` is anchored to, so a rule
        // can go from matching to dead (or back) on this switch alone.
        imp.contents_row.connect_active_notify(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| {
                win.imp().filter_hits.replace(None);
                win.rebuild_filter_rows();
            }
        ));

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

    /// Build the streaming transfer log as a `ListView`: one typed row per
    /// file/message in arrival order — a leading icon, the path or text, and a
    /// right-aligned tag ("New"/"Updated"/"Deleted"). Deletions and errors
    /// render in red; the command header and informational lines are dimmed.
    /// No sorter: the log is chronological, a live tail of the transfer.
    fn setup_log_list(&self) {
        let imp = self.imp();
        let store = gio::ListStore::new::<LogObject>();
        let selection = gtk::NoSelection::new(Some(store.clone()));

        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            let icon = gtk::Image::new();
            let primary = gtk::Label::builder()
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .build();
            let detail = gtk::Label::builder()
                .xalign(1.0)
                .css_classes(["dim-label", "caption"])
                .build();
            let row = gtk::Box::builder()
                .orientation(gtk::Orientation::Horizontal)
                .spacing(8)
                .build();
            row.append(&icon);
            row.append(&primary);
            row.append(&detail);
            item.downcast_ref::<gtk::ListItem>()
                .unwrap()
                .set_child(Some(&row));
        });
        factory.connect_bind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let obj = item.item().and_downcast::<LogObject>().unwrap();
            let row = item.child().and_downcast::<gtk::Box>().unwrap();
            let icon = row.first_child().and_downcast::<gtk::Image>().unwrap();
            let primary = icon.next_sibling().and_downcast::<gtk::Label>().unwrap();
            let detail = primary.next_sibling().and_downcast::<gtk::Label>().unwrap();

            icon.set_icon_name(Some(&obj.icon()));
            let text = obj.primary();
            primary.set_label(&text);
            primary.set_tooltip_text(Some(&text));

            // Recompute css wholesale each bind (ListView recycles rows).
            let mut classes: Vec<&str> = Vec::new();
            if obj.danger() {
                classes.push("error");
            }
            if obj.dim() {
                classes.push("dim-label");
            }
            if obj.mono() {
                classes.push("monospace");
            }
            primary.set_css_classes(&classes);

            detail.set_label(&obj.detail());
            detail.set_visible(!obj.detail().is_empty());
        });

        imp.log_list.set_model(Some(&selection));
        imp.log_list.set_factory(Some(&factory));
        imp.log_store.set(store).expect("log_store set once");
    }

    /// Append one row to the log and follow to the bottom, like a live tail.
    fn log_push(&self, obj: LogObject) {
        let imp = self.imp();
        if let Some(store) = imp.log_store.get() {
            store.append(&obj);
        }
        // Scroll after layout settles so `upper` reflects the new row.
        let vadj = imp.log_scroll.vadjustment();
        glib::idle_add_local_once(move || vadj.set_value(vadj.upper()));
    }

    /// Add a parameterless `win.<name>` action that calls `run`.
    fn add_simple_action(&self, name: &str, run: impl Fn(&Self) + 'static) {
        let action = gio::SimpleAction::new(name, None);
        action.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, _| run(&win)
        ));
        self.add_action(&action);
    }

    /// Wire the Preview / Start / Cancel buttons, and the window's actions.
    ///
    /// Every keyboard shortcut is one of these actions (the keys themselves are
    /// in [`crate::shortcuts`]). Each calls the very function its button calls —
    /// Start in particular goes through `on_start_clicked`, so the dry-run-first
    /// confirmations cannot be skipped from the keyboard — and each is enabled
    /// only while that button is, which `refresh_action_sensitivity` sees to.
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
            move |_| win.cancel_run()
        ));

        self.add_simple_action("dry-run", |win| win.run_preview(false));
        self.add_simple_action("start-sync", Self::on_start_clicked);
        self.add_simple_action("cancel-run", Self::cancel_run);
        // "New Job" (win.new-job): clear the whole form to start fresh.
        self.add_simple_action("new-job", Self::clear_job);

        self.add_simple_action("add-folder", Self::choose_add_folders);
        self.add_simple_action("add-file", Self::choose_add_files);
        self.add_simple_action("remote-source", |win| win.choose_remote(RemoteSide::Source));
        self.add_simple_action("choose-destination", Self::choose_dest);
        self.add_simple_action("remote-destination", |win| {
            win.choose_remote(RemoteSide::Dest)
        });
        self.add_simple_action("add-filter-rule", Self::focus_filter_entry);
        self.add_simple_action("save-preset", Self::prompt_save_preset);

        // One action for the three pages; the page name is its parameter.
        let show_page = gio::SimpleAction::new("show-page", Some(glib::VariantTy::STRING));
        show_page.connect_activate(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_, page| {
                if let Some(page) = page.and_then(|p| p.str()) {
                    win.imp().main_stack.set_visible_child_name(page);
                }
            }
        ));
        self.add_action(&show_page);

        // "What Foresight Can Do" (win.capabilities): the Help/capability dialog.
        self.add_simple_action("capabilities", crate::help::present);
        // The name is the one GTK gives the action behind a help overlay, and
        // the one the menu has always pointed at.
        self.add_simple_action("show-help-overlay", |win| {
            crate::shortcuts::present(win.upcast_ref());
        });
        self.add_simple_action("close", |win| win.close());

        // A dialog lives inside this window, so the window's accelerators are
        // still matched while one is up — Start would be reachable from behind
        // its own confirmation. Opening or closing one re-derives the actions.
        self.connect_visible_dialog_notify(|win| win.refresh_action_sensitivity());

        // A button with a key says so, the key taken from the same table.
        for (button, action) in [
            (&*imp.preview_button, "win.dry-run"),
            (&*imp.start_button, "win.start-sync"),
            (&*imp.cancel_button, "win.cancel-run"),
            (&*imp.add_folder_button, "win.add-folder"),
            (&*imp.add_file_button, "win.add-file"),
            (&*imp.add_remote_source_button, "win.remote-source"),
            (&*imp.remote_dest_button, "win.remote-destination"),
            (&*imp.save_preset_button, "win.save-preset"),
        ] {
            if let Some(text) = button.tooltip_text() {
                button.set_tooltip_text(Some(&crate::shortcuts::tooltip(&text, action)));
            }
        }
        if let Some(text) = imp.menu_button.tooltip_text() {
            imp.menu_button
                .set_tooltip_text(Some(&format!("{text} (F10)")));
        }

        // The banner has one button, and what it does depends on what the
        // banner is saying: see `BannerButton`.
        imp.result_banner.connect_button_clicked(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.on_banner_button()
        ));
    }

    /// The banner's button was pressed: do what it currently stands for.
    fn on_banner_button(&self) {
        // Cloned out: both branches reach code that sets this cell.
        let button = self.imp().banner_button.borrow().clone();
        match button {
            BannerButton::None => {}
            BannerButton::NewJob => self.clear_job(),
            BannerButton::Problems(report) => self.present_partial_report(&report),
        }
    }

    fn cancel_run(&self) {
        if let Some(runner) = self.imp().runner.borrow().as_ref() {
            runner.cancel();
        }
    }

    /// Open Advanced → Filter rules and put the cursor in the add-rule entry,
    /// which is otherwise two expanders deep.
    fn focus_filter_entry(&self) {
        let imp = self.imp();
        imp.main_stack.set_visible_child_name("configure");
        imp.advanced_row.set_expanded(true);
        imp.filters_row.set_expanded(true);
        // Once the expanders have laid out: a row that is not mapped yet
        // cannot take the focus.
        let entry = imp.filter_entry.get();
        glib::idle_add_local_once(move || {
            entry.grab_focus();
        });
    }

    /// Reset the window to an empty Configure page for a new transfer. Ignored
    /// while a run is live.
    fn clear_job(&self) {
        if self.is_running() {
            return;
        }
        let imp = self.imp();

        // Sources: drop every row and the backing list.
        self.clear_sources();
        // Destination, and any remote endpoint on either side. A new job means
        // a new job — a leftover remote host is exactly the kind of thing that
        // would be missed on the Configure page and noticed on the far machine.
        *imp.dest.borrow_mut() = None;
        *imp.remote.borrow_mut() = None;
        // Repaint from the cleared state: the row title, its icon and the two
        // remote buttons' tooltips all still describe the old endpoint.
        self.refresh_remote_state();
        imp.contents_row.set_active(false);
        imp.delete_row.set_active(false);

        // Advanced options.
        imp.verbose_row.set_active(false);
        imp.remove_source_row.set_active(false);
        imp.bwlimit_row.set_value(0.0);
        imp.bwlimit_unit_row.set_selected(1); // MB/s
        self.set_filters(&[]);
        imp.filter_entry.set_text("");
        imp.filter_kind.set_selected(0); // Exclude
        imp.extra_args_row.set_text("");
        imp.suppress_combo.set(true);
        imp.preset_combo.set_selected(0);
        imp.suppress_combo.set(false);
        imp.delete_preset_button.set_sensitive(false);

        // Results from any previous run.
        if let Some(store) = imp.preview_store.get() {
            store.remove_all();
        }
        imp.run_errors.borrow_mut().clear();
        imp.deletions.borrow_mut().clear();
        if let Some(store) = imp.log_store.get() {
            store.remove_all();
        }
        imp.overall_progress.set_fraction(0.0);
        imp.overall_progress.set_text(None);
        imp.current_file_label.set_label("");
        self.hide_banner();

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

    // -- remote endpoints (M5 part 3) ---------------------------------------

    /// Open the endpoint form for one side. The dialog only calls back with an
    /// endpoint whose host key is already trusted or has just been confirmed,
    /// so nothing here has to re-check.
    fn choose_remote(&self, side: RemoteSide) {
        let existing = self
            .imp()
            .remote
            .borrow()
            .as_ref()
            .filter(|(s, _)| *s == side)
            .map(|(_, e)| e.clone());
        let title = match side {
            RemoteSide::Source => "Remote source",
            RemoteSide::Dest => "Remote destination",
        };
        crate::remote_dialog::present(
            self,
            title,
            existing,
            glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |endpoint| win.set_remote(side, endpoint)
            ),
        );
    }

    fn set_remote(&self, side: RemoteSide, endpoint: Endpoint) {
        let imp = self.imp();
        // Setting a remote source discards local sources rather than trying to
        // combine them: rsync takes local paths or one remote operand, never a
        // mix, so keeping them would only look like they were still in the job.
        if side == RemoteSide::Source {
            self.clear_sources();
        }
        *imp.remote.borrow_mut() = Some((side, endpoint));
        self.refresh_remote_state();
        self.refresh_sources_state();
    }

    fn clear_remote(&self) {
        *self.imp().remote.borrow_mut() = None;
        self.refresh_remote_state();
        self.refresh_sources_state();
    }

    /// The remote endpoint on `side`, if that is where it is.
    fn remote_on(&self, side: RemoteSide) -> Option<Endpoint> {
        self.imp()
            .remote
            .borrow()
            .as_ref()
            .filter(|(s, _)| *s == side)
            .map(|(_, e)| e.clone())
    }

    /// Repaint the destination row and the source placeholder to match the
    /// remote state, and keep the two "remote" buttons mutually exclusive —
    /// rsync refuses a job that is remote at both ends.
    fn refresh_remote_state(&self) {
        let imp = self.imp();
        let remote = imp.remote.borrow().clone();

        match &remote {
            Some((RemoteSide::Dest, e)) => {
                imp.dest_row.set_title("Remote folder");
                imp.dest_row
                    .set_subtitle(&glib::markup_escape_text(&e.to_string()));
                imp.dest_row.set_tooltip_text(Some(&e.to_string()));
                imp.dest_icon.set_icon_name(Some("network-server-symbolic"));
            }
            _ => {
                imp.dest_row.set_title("Folder");
                imp.dest_icon.set_icon_name(Some("folder-open-symbolic"));
                match imp.dest.borrow().as_ref() {
                    Some(path) => {
                        let (subtitle, tooltip) = describe_path(path);
                        imp.dest_row.set_subtitle(&subtitle);
                        imp.dest_row.set_tooltip_text(Some(&tooltip));
                    }
                    None => {
                        imp.dest_row.set_subtitle("Not selected");
                        imp.dest_row.set_tooltip_text(None);
                    }
                }
            }
        }

        // One side at a time (refresh_action_sensitivity enforces it). The
        // tooltip says why, so a disabled button is an explanation rather than
        // a dead end.
        let source_is_remote = matches!(remote, Some((RemoteSide::Source, _)));
        let dest_is_remote = matches!(remote, Some((RemoteSide::Dest, _)));
        // The key is named only where the button works: beside the reason a
        // button is closed off it would read as a way around it.
        let with_key = crate::shortcuts::tooltip;
        imp.add_remote_source_button
            .set_tooltip_text(Some(&if dest_is_remote {
                "The destination is already remote — rsync cannot have both ends on other machines"
                    .to_string()
            } else if source_is_remote {
                with_key("Change the remote source", "win.remote-source")
            } else {
                with_key("Pull from a remote machine over SSH", "win.remote-source")
            }));
        imp.remote_dest_button
            .set_tooltip_text(Some(&if source_is_remote {
                "The source is already remote — rsync cannot have both ends on other machines"
                    .to_string()
            } else if dest_is_remote {
                with_key("Change the remote destination", "win.remote-destination")
            } else {
                with_key(
                    "Send to a remote machine over SSH",
                    "win.remote-destination",
                )
            }));

        self.refresh_action_sensitivity();
    }

    /// Drop every local source row (used when a remote source takes over, and
    /// by New Job).
    fn clear_sources(&self) {
        let imp = self.imp();
        for entry in imp.sources.borrow_mut().drain(..) {
            imp.sources_group.remove(&entry.row);
        }
    }

    // -- filter rules -------------------------------------------------------

    /// Which kind the dropdown beside the entry is currently offering. Index 1
    /// is Include; anything else is Exclude, matching the order in the
    /// Blueprint's `StringList` (Exclude first, as the common case).
    fn selected_filter_kind(&self) -> FilterKind {
        match self.imp().filter_kind.selected() {
            1 => FilterKind::Include,
            _ => FilterKind::Exclude,
        }
    }

    /// Add one filter rule at the end of the list. Returns whether it was added,
    /// so the caller only clears the entry on success — a rejected rule stays
    /// put to be edited rather than silently vanishing.
    ///
    /// Surrounding whitespace is trimmed (it is invariably a typo), but interior
    /// spaces are kept: the pattern becomes one argv element, never two.
    ///
    /// Only an exact `(kind, pattern)` repeat is refused. The *same* pattern
    /// with the other kind is allowed on purpose: it is contradictory, but rsync
    /// resolves it by position (the earlier rule wins), so the list can express
    /// it and the user can see which one is on top.
    fn add_filter(&self, kind: FilterKind, pattern: &str) -> bool {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            return false;
        }
        let imp = self.imp();
        let duplicate = imp
            .filters
            .borrow()
            .iter()
            .any(|r| r.kind == kind && r.pattern == pattern);
        if duplicate {
            let verb = match kind {
                FilterKind::Include => "included",
                FilterKind::Exclude => "excluded",
            };
            self.toast(&format!("“{pattern}” is already {verb}"));
            return false;
        }

        let rule = FilterRule {
            kind,
            pattern: pattern.to_string(),
        };
        let dead = rule.dead_anchor(&self.transfer_top()).is_some();
        imp.filters.borrow_mut().push(rule);
        self.rebuild_filter_rows();
        if dead {
            // Added anyway — the row carries the explanation and the fix — but
            // said out loud, because the list may be scrolled out of view.
            self.toast(&format!("“{pattern}” can never match — see the rule"));
        }
        true
    }

    /// What a leading `/` in a pattern is anchored to, for the job as it stands.
    fn transfer_top(&self) -> TransferTop {
        let imp = self.imp();
        let sources: Vec<Source> = imp
            .sources
            .borrow()
            .iter()
            .map(|s| Source {
                path: s.path.clone(),
                is_dir: s.is_dir,
            })
            .collect();
        let remote = self.remote_on(RemoteSide::Source);
        TransferTop::new(
            &sources,
            remote.as_ref().map(|e| e.path.as_str()),
            imp.contents_row.is_active(),
        )
    }

    /// Replace the rule at `index` with the pattern that was evidently meant.
    fn apply_filter_fix(&self, index: usize, pattern: &str) {
        let imp = self.imp();
        {
            let mut filters = imp.filters.borrow_mut();
            let Some(kind) = filters.get(index).map(|r| r.kind) else {
                return;
            };
            // The corrected rule may already be in the list; then the dead one
            // is simply surplus.
            if filters
                .iter()
                .any(|r| r.kind == kind && r.pattern == pattern)
            {
                filters.remove(index);
            } else {
                filters[index].pattern = pattern.to_string();
            }
        }
        self.rebuild_filter_rows();
    }

    /// The last dry run's hit counts, if they still describe the current rules.
    fn current_filter_hits(&self) -> Option<Vec<RuleHits>> {
        let imp = self.imp();
        let hits = imp.filter_hits.borrow();
        let (rules, counts) = hits.as_ref()?;
        (*rules == *imp.filters.borrow()).then(|| counts.clone())
    }

    fn remove_filter(&self, index: usize) {
        let imp = self.imp();
        {
            let mut filters = imp.filters.borrow_mut();
            if index >= filters.len() {
                return;
            }
            filters.remove(index);
        }
        self.rebuild_filter_rows();
    }

    /// Move the rule at `index` one position earlier (`delta` -1) or later
    /// (+1). This is not cosmetic: rsync takes the first rule that matches, so
    /// moving a rule above another changes which of them decides a path.
    fn move_filter(&self, index: usize, delta: isize) {
        let imp = self.imp();
        {
            let mut filters = imp.filters.borrow_mut();
            let Some(target) = index.checked_add_signed(delta) else {
                return;
            };
            if index >= filters.len() || target >= filters.len() {
                return;
            }
            filters.swap(index, target);
        }
        self.rebuild_filter_rows();
    }

    /// Replace every rule at once (preset applied, or job cleared).
    fn set_filters(&self, rules: &[FilterRule]) {
        *self.imp().filters.borrow_mut() = rules.to_vec();
        self.rebuild_filter_rows();
    }

    /// Rebuild the rule rows from the model.
    ///
    /// Every mutation goes through here rather than patching rows in place:
    /// `AdwExpanderRow` has no reorder API, so a moved rule would need the rows
    /// torn down anyway, and each row's buttons close over its index — which
    /// every insertion, removal and swap invalidates for the rows after it.
    /// Rebuilding keeps the widgets and the model incapable of disagreeing.
    fn rebuild_filter_rows(&self) {
        let imp = self.imp();
        for row in imp.filter_rows.borrow_mut().drain(..) {
            imp.filters_row.remove(&row);
        }

        // Cloned: building rows runs GTK code, and nothing may be holding a
        // borrow if any of it reaches back into these handlers.
        let rules = imp.filters.borrow().clone();
        let last = rules.len().saturating_sub(1);
        let top = self.transfer_top();
        let hits = self.current_filter_hits();
        // The rules changed under a matched-nothing notice: it no longer
        // describes them.
        if hits.is_none() && imp.filter_banner.replace(false) {
            self.hide_banner();
        }
        for (index, rule) in rules.iter().enumerate() {
            // Two ways to know a rule does nothing: by construction (its anchor
            // cannot exist in this transfer), or by evidence (a dry run ran
            // with it and it matched no path). The first needs no run at all.
            let dead_anchor = rule.dead_anchor(&top);
            let verdict = match (&dead_anchor, hits.as_ref().and_then(|h| h.get(index))) {
                (Some(_), _) => Some((
                    true,
                    "can never match: a leading / is the top of the transfer, not of the disk"
                        .to_string(),
                )),
                (None, Some(h)) if h.total() == 0 => {
                    Some((true, "matched nothing in the last dry run".to_string()))
                }
                (None, Some(h)) => Some((
                    false,
                    match h.total() {
                        1 => "matched 1 path in the last dry run".to_string(),
                        n => format!("matched {n} paths in the last dry run"),
                    },
                )),
                (None, None) => None,
            };
            let warn = verdict.as_ref().is_some_and(|(warn, _)| *warn);

            let mut subtitle = format!("{} ({})", rule.kind.label(), rule.kind.flag());
            if let Some((_, note)) = &verdict {
                subtitle.push_str(" · ");
                subtitle.push_str(note);
            }
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&rule.pattern))
                .subtitle(glib::markup_escape_text(&subtitle))
                .build();
            let icon = gtk::Image::from_icon_name(match (warn, rule.kind) {
                (true, _) => "dialog-warning-symbolic",
                (false, FilterKind::Include) => "object-select-symbolic",
                (false, FilterKind::Exclude) => "action-unavailable-symbolic",
            });
            if warn {
                icon.add_css_class("warning");
            }
            row.add_prefix(&icon);

            if let Some(suggestion) = dead_anchor.and_then(|d| d.suggestion) {
                let fix = gtk::Button::builder()
                    .label("Fix")
                    .tooltip_text(format!("Change to {suggestion}"))
                    .valign(gtk::Align::Center)
                    .build();
                fix.connect_clicked(glib::clone!(
                    #[weak(rename_to = win)]
                    self,
                    move |_| win.apply_filter_fix(index, &suggestion)
                ));
                row.add_suffix(&fix);
            }

            let up = gtk::Button::builder()
                .icon_name("go-up-symbolic")
                .tooltip_text("Apply this rule earlier")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .sensitive(index > 0)
                .build();
            up.connect_clicked(glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |_| win.move_filter(index, -1)
            ));
            row.add_suffix(&up);

            let down = gtk::Button::builder()
                .icon_name("go-down-symbolic")
                .tooltip_text("Apply this rule later")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .sensitive(index < last)
                .build();
            down.connect_clicked(glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |_| win.move_filter(index, 1)
            ));
            row.add_suffix(&down);

            let remove = gtk::Button::builder()
                .icon_name("edit-delete-symbolic")
                .tooltip_text("Remove this rule")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            remove.connect_clicked(glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |_| win.remove_filter(index)
            ));
            row.add_suffix(&remove);

            imp.filters_row.add_row(&row);
            imp.filter_rows.borrow_mut().push(row);
        }

        self.refresh_filters_state();
    }

    /// Keep the expander's subtitle honest about how many rules are active and
    /// that their order decides ties, so neither needs the expander open to see.
    fn refresh_filters_state(&self) {
        let imp = self.imp();
        let n = imp.filters.borrow().len();
        let mut subtitle = match n {
            0 => "Skip or keep paths by pattern (--exclude / --include)".to_string(),
            1 => "1 rule".to_string(),
            n => format!("{n} rules · the first one that matches wins"),
        };
        // Visible with the expander shut: a rule that does nothing is the one
        // thing about this list that must not need opening it to find out.
        let idle = self.idle_rules().len();
        if idle > 0 {
            subtitle.push_str(&match idle {
                1 => " · 1 matches nothing".to_string(),
                k => format!(" · {k} match nothing"),
            });
        }
        imp.filters_row.set_subtitle(&subtitle);
    }

    /// Indices of the rules known to do nothing — dead by construction, or
    /// shown to match no path by a dry run that still describes this job.
    fn idle_rules(&self) -> Vec<usize> {
        let top = self.transfer_top();
        let hits = self.current_filter_hits();
        self.imp()
            .filters
            .borrow()
            .iter()
            .enumerate()
            .filter(|(i, rule)| {
                rule.dead_anchor(&top).is_some()
                    || hits
                        .as_ref()
                        .and_then(|h| h.get(*i))
                        .is_some_and(|h| h.total() == 0)
            })
            .map(|(i, _)| i)
            .collect()
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
        let imp = self.imp();
        *imp.dest.borrow_mut() = Some(path);
        // Picking a local folder replaces a remote destination: there is one
        // destination, and the row has to say which it is.
        if self.remote_on(RemoteSide::Dest).is_some() {
            *imp.remote.borrow_mut() = None;
        }
        self.refresh_remote_state();
    }

    /// Recompute the placeholder, the availability of the two single-folder
    /// options (`--delete` and *Sync folder contents*), and the action-button
    /// sensitivity.
    fn refresh_sources_state(&self) {
        let imp = self.imp();
        let remote_source = self.remote_on(RemoteSide::Source);
        let sources = imp.sources.borrow();

        // With a remote source the placeholder becomes the row that states it —
        // there are no local source rows to show, and an empty list with no
        // explanation would read as "nothing selected".
        match &remote_source {
            Some(e) => {
                imp.sources_placeholder.set_visible(true);
                imp.sources_placeholder.set_sensitive(true);
                imp.sources_placeholder.set_title("Remote source");
                imp.sources_placeholder
                    .set_subtitle(&glib::markup_escape_text(&e.to_string()));
                imp.clear_remote_source_button.set_visible(true);
            }
            None => {
                imp.sources_placeholder.set_visible(sources.is_empty());
                imp.sources_placeholder.set_sensitive(false);
                imp.sources_placeholder.set_title("No sources yet");
                imp.sources_placeholder
                    .set_subtitle("Use the buttons above, or drop files and folders here");
                imp.clear_remote_source_button.set_visible(false);
            }
        }

        // The two single-folder options need a lone directory we can see. A
        // remote path was typed rather than picked, and nothing has stat'd the
        // far end, so they stay off for a pull — matching `Job::is_single_dir`.
        let single_dir = remote_source.is_none() && sources.len() == 1 && sources[0].is_dir;
        for row in [&*imp.contents_row, &*imp.delete_row] {
            row.set_sensitive(single_dir);
            if !single_dir {
                row.set_active(false);
            }
        }
        drop(sources);

        // A different set of sources re-anchors every `/pattern`, and makes the
        // last dry run's hit counts evidence about some other job.
        imp.filter_hits.replace(None);
        self.rebuild_filter_rows();

        // The add buttons are refresh_action_sensitivity's to set.
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

        // Each side is satisfied either locally or remotely, never both.
        let remote_source = self.remote_on(RemoteSide::Source);
        let remote_dest = self.remote_on(RemoteSide::Dest);
        if sources.is_empty() && remote_source.is_none() {
            return None;
        }
        let dest = match (&remote_dest, imp.dest.borrow().clone()) {
            (Some(_), local) => local.unwrap_or_default(), // unused; operand wins
            (None, Some(local)) => local,
            (None, None) => return None,
        };

        let (remote, endpoint) = match (remote_source, remote_dest) {
            (Some(e), _) => (Some(Remote::Source(e.operand())), Some(e)),
            (_, Some(e)) => (Some(Remote::Dest(e.operand())), Some(e)),
            _ => (None, None),
        };

        // A remote job carries our ssh command, so the transfer uses the app's
        // own known_hosts and strict checking rather than ssh's defaults. If it
        // cannot be built the job is not runnable: silently falling back to a
        // bare `ssh` would drop exactly the host-key guarantee this is for.
        let remote_shell = match &endpoint {
            Some(e) => match crate::ssh::rsh_command(&crate::ssh::known_hosts_path(), e.port) {
                Ok(cmd) => Some(cmd),
                Err(err) => {
                    self.toast(&err.to_string());
                    return None;
                }
            },
            None => None,
        };

        let adv = self.read_advanced();
        Some(Job {
            sources,
            dest,
            remote,
            delete: adv.delete,
            sync_contents: adv.sync_contents,
            verbose: adv.verbose,
            remove_source_files: adv.remove_source_files,
            bwlimit: adv.bwlimit,
            filters: adv.filters,
            remote_shell,
            extra_args: adv.extra_args,
        })
    }

    /// Snapshot the Advanced controls into a [`Profile`] (paths excluded).
    /// Filter rules come from the list verbatim and in order, one element each;
    /// extra arguments are tokenised on whitespace (never shell-interpreted, so
    /// no quoting or brace expansion). An empty bandwidth limit means unlimited.
    fn read_advanced(&self) -> Profile {
        let imp = self.imp();
        let bwlimit = match imp.bwlimit_row.value() as u64 {
            0 => None,
            n => {
                let suffix = match imp.bwlimit_unit_row.selected() {
                    0 => "K",
                    2 => "G",
                    _ => "M", // index 1 (MB/s) is the default
                };
                Some(format!("{n}{suffix}"))
            }
        };
        Profile {
            name: String::new(),
            delete: imp.delete_row.is_active(),
            sync_contents: imp.contents_row.is_active(),
            verbose: imp.verbose_row.is_active(),
            remove_source_files: imp.remove_source_row.is_active(),
            bwlimit,
            filters: imp.filters.borrow().clone(),
            extra_args: tokenize(&imp.extra_args_row.text()),
        }
    }

    /// Push a preset's options into the Advanced controls. The two single-folder
    /// options are only set when their switches are currently allowed.
    fn apply_advanced(&self, p: &Profile) {
        let imp = self.imp();
        imp.verbose_row.set_active(p.verbose);
        imp.remove_source_row.set_active(p.remove_source_files);
        let (value, unit) = parse_bwlimit(p.bwlimit.as_deref().unwrap_or(""));
        imp.bwlimit_row.set_value(value);
        imp.bwlimit_unit_row.set_selected(unit);
        self.set_filters(&p.filters);
        imp.extra_args_row.set_text(&p.extra_args.join(" "));
        imp.contents_row
            .set_active(p.sync_contents && imp.contents_row.is_sensitive());
        imp.delete_row
            .set_active(p.delete && imp.delete_row.is_sensitive());
    }

    // -- presets (M4) -------------------------------------------------------

    fn setup_presets(&self) {
        let imp = self.imp();
        if let Some(error) = self.load_presets() {
            self.announce_unreadable_presets(&error);
        }

        imp.preset_combo.connect_selected_notify(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.on_preset_selected()
        ));
        imp.save_preset_button.connect_clicked(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.prompt_save_preset()
        ));
        imp.delete_preset_button.connect_clicked(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.delete_selected_preset()
        ));
    }

    /// Read the presets file into the list and the combo. Returns the error if
    /// the file is there and did not load — apart from how that is announced,
    /// so the headless checks can stage one without a toast to account for.
    ///
    /// On an error the list is empty because there is nothing to put in it,
    /// not because the user has no presets; `presets_unread` is what records
    /// the difference, and the row says so for as long as it holds.
    fn load_presets(&self) -> Option<profiles::LoadError> {
        let imp = self.imp();
        let (list, error) = match profiles::load() {
            Ok(list) => (list, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        *imp.profiles.borrow_mut() = list;
        *imp.presets_unread.borrow_mut() = error.clone();
        self.rebuild_preset_combo(0);
        error
    }

    /// Say at startup that the presets are missing because they could not be
    /// read, not because there are none. This runs while the window is being
    /// built, so it cannot be a dialog; a toast cannot hold a path legibly and
    /// is normally gone in seconds. So: a toast that stays until dismissed,
    /// whose button opens the dialog with the path and the reason in it.
    fn announce_unreadable_presets(&self, error: &profiles::LoadError) {
        let toast = adw::Toast::builder()
            .title("Saved presets could not be read")
            .button_label("Details")
            .priority(adw::ToastPriority::High)
            .timeout(0)
            .build();
        let body = unreadable_presets_notice(error);
        toast.connect_button_clicked(glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |_| win.show_preset_error("Presets could not be read", &body)
        ));
        self.imp().toast_overlay.add_toast(toast);
    }

    /// A save is about to replace the presets file with the list in memory.
    /// If that list is empty only because the file could not be read, look
    /// again first: someone who was told at startup that the file is broken
    /// may well have repaired it since, and the repaired presets must be what
    /// this save builds on rather than what it writes over. If the file still
    /// does not load, `save_all` moves it aside.
    fn recheck_unread_presets(&self) {
        if self.imp().presets_unread.borrow().is_some() {
            self.load_presets();
        }
    }

    /// Rebuild the combo model as `["Choose a preset…", <names…>]` and select
    /// `select` (0 = the placeholder), without firing the apply handler.
    fn rebuild_preset_combo(&self, select: u32) {
        let imp = self.imp();
        imp.preset_combo
            .set_subtitle(match *imp.presets_unread.borrow() {
                Some(_) => "The presets file could not be read",
                None => "",
            });
        imp.suppress_combo.set(true);
        let list = gtk::StringList::new(&["Choose a preset…"]);
        for p in imp.profiles.borrow().iter() {
            list.append(&p.name);
        }
        imp.preset_combo.set_model(Some(&list));
        imp.preset_combo.set_selected(select);
        imp.suppress_combo.set(false);
        imp.delete_preset_button.set_sensitive(select > 0);
    }

    fn on_preset_selected(&self) {
        let imp = self.imp();
        if imp.suppress_combo.get() {
            return;
        }
        let idx = imp.preset_combo.selected();
        imp.delete_preset_button.set_sensitive(idx > 0);
        if idx == 0 {
            return;
        }
        let profile = imp.profiles.borrow().get((idx - 1) as usize).cloned();
        if let Some(p) = profile {
            self.apply_advanced(&p);
        }
    }

    fn prompt_save_preset(&self) {
        let entry = gtk::Entry::builder()
            .placeholder_text("Preset name")
            .activates_default(true)
            .build();
        let dialog = adw::AlertDialog::builder()
            .heading("Save preset")
            .body("Save the current Advanced options under a name.")
            .extra_child(&entry)
            .build();
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("save", "Save");
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");

        dialog.connect_response(
            None,
            glib::clone!(
                #[weak(rename_to = win)]
                self,
                #[weak]
                entry,
                move |_, response| {
                    if response == "save" {
                        let name = entry.text().trim().to_string();
                        if !name.is_empty() {
                            win.upsert_preset(name);
                        }
                    }
                }
            ),
        );
        dialog.present(Some(self));
    }

    fn upsert_preset(&self, name: String) {
        match self.try_upsert_preset(&name) {
            Ok(saved) => {
                self.announce_saved("Preset saved", &format!("Saved preset “{name}”"), &saved)
            }
            Err(e) => self.show_preset_error(
                "Preset not saved",
                &format!("“{name}” was not saved: {e}.\n\nYour presets are as they were."),
            ),
        }
    }

    /// The save itself, apart from how its outcome is announced, so the
    /// headless checks can drive a failure without a dialog to dismiss.
    fn try_upsert_preset(&self, name: &str) -> Result<profiles::Saved, profiles::SaveError> {
        let imp = self.imp();
        let mut profile = self.read_advanced();
        profile.name = name.to_string();
        self.recheck_unread_presets();

        // Disk first, memory second. The change is made to a copy, and the
        // copy becomes the list only once it has been written — so on a failed
        // write the list and the combo still describe the file, which a failed
        // save leaves untouched. The other order would show a preset that is
        // gone after a restart, which is the bug this replaced.
        let mut candidate = imp.profiles.borrow().clone();
        match candidate.iter_mut().find(|p| p.name == name) {
            Some(existing) => *existing = profile,
            None => candidate.push(profile),
        }
        let saved = profiles::save_all(&candidate)?;

        let idx = candidate.iter().position(|p| p.name == name).unwrap() as u32 + 1;
        *imp.profiles.borrow_mut() = candidate;
        // Whatever was there before, the file is now the list.
        imp.presets_unread.replace(None);
        self.rebuild_preset_combo(idx);
        Ok(saved)
    }

    /// Announce a save or a delete that went through. Normally a toast; when
    /// the file it replaced had to be moved aside, a dialog, because where the
    /// file went is a path the user needs time to read.
    fn announce_saved(&self, heading: &str, done: &str, saved: &profiles::Saved) {
        match &saved.moved_aside {
            None => self.toast(done),
            Some(to) => {
                self.show_preset_error(heading, &format!("{done}.\n\n{}", moved_aside_notice(to)))
            }
        }
    }

    fn delete_selected_preset(&self) {
        match self.try_delete_selected_preset() {
            Ok(Some((name, saved))) => self.announce_saved(
                "Preset deleted",
                &format!("Deleted preset “{name}”"),
                &saved,
            ),
            Ok(None) => {}
            Err((name, e)) => self.show_preset_error(
                "Preset not deleted",
                &format!("“{name}” is still saved: {e}.\n\nYour presets are as they were."),
            ),
        }
    }

    /// `Ok(None)` when there was nothing selected to delete. An error names
    /// the preset that is still there.
    fn try_delete_selected_preset(
        &self,
    ) -> Result<Option<(String, profiles::Saved)>, (String, profiles::SaveError)> {
        let imp = self.imp();
        let idx = imp.preset_combo.selected();
        if idx == 0 {
            return Ok(None);
        }
        let i = (idx - 1) as usize;
        // Same order as saving, for the mirrored reason: a preset dropped from
        // the list but not from the file comes back on the next launch. On
        // failure nothing is rebuilt, so it also stays selected.
        let mut candidate = imp.profiles.borrow().clone();
        if i >= candidate.len() {
            return Ok(None);
        }
        let removed = candidate.remove(i).name;
        // No recheck here as there is before a save: while the file is unread
        // the list is empty, so there is nothing selected to delete. The file
        // can still have gone bad since it was loaded, and `save_all` looks.
        let saved = match profiles::save_all(&candidate) {
            Ok(saved) => saved,
            Err(e) => return Err((removed, e)),
        };
        *imp.profiles.borrow_mut() = candidate;
        imp.presets_unread.replace(None);
        self.rebuild_preset_combo(0);
        Ok(Some((removed, saved)))
    }

    // -- run lifecycle (M3) -------------------------------------------------

    /// Both ends are chosen. A remote endpoint satisfies its own side, so a
    /// pull needs no local sources and a push needs no local destination.
    fn both_selected(&self) -> bool {
        let imp = self.imp();
        let have_source =
            !imp.sources.borrow().is_empty() || self.remote_on(RemoteSide::Source).is_some();
        let have_dest = imp.dest.borrow().is_some() || self.remote_on(RemoteSide::Dest).is_some();
        have_source && have_dest
    }

    fn is_running(&self) -> bool {
        self.imp().runner.borrow().is_some()
    }

    /// Preview and Start follow selection; both are disabled while a run is
    /// live. Cancel is the inverse. The add/remove controls also lock during a
    /// run so the source list can't change mid-transfer.
    ///
    /// The actions behind the keyboard shortcuts are set here too, from the
    /// same values as the buttons they mirror, so a key and its button cannot
    /// come to disagree.
    fn refresh_action_sensitivity(&self) {
        let imp = self.imp();
        let running = self.is_running();
        let idle_ready = self.both_selected() && !running;
        imp.preview_button.set_sensitive(idle_ready);
        imp.start_button.set_sensitive(idle_ready);
        imp.cancel_button.set_sensitive(running);
        // The four endpoint buttons answer to two things at once — the run lock
        // and which side, if any, is remote — so they are computed here from
        // both, every time. Setting only the lock would leave nothing to lift
        // it when the run ends.
        let source_is_remote = self.remote_on(RemoteSide::Source).is_some();
        let dest_is_remote = self.remote_on(RemoteSide::Dest).is_some();
        // Local sources and a remote source are alternatives, not a mix.
        let local_allowed = !source_is_remote && !running;
        imp.add_folder_button.set_sensitive(local_allowed);
        imp.add_file_button.set_sensitive(local_allowed);
        // One remote side at a time: rsync refuses a job remote at both ends.
        let remote_source_allowed = !dest_is_remote && !running;
        let remote_dest_allowed = !source_is_remote && !running;
        imp.add_remote_source_button
            .set_sensitive(remote_source_allowed);
        imp.remote_dest_button.set_sensitive(remote_dest_allowed);

        // While a dialog is up the keys are the dialog's. It covers the
        // buttons, so they need no such term; an accelerator is matched by the
        // window whatever is drawn over it.
        let free = self.visible_dialog().is_none();
        for (name, enabled) in [
            ("dry-run", idle_ready),
            ("start-sync", idle_ready),
            ("cancel-run", running),
            // clear_job refuses during a run; this makes the menu item say so.
            ("new-job", !running),
            ("add-folder", local_allowed),
            ("add-file", local_allowed),
            ("remote-source", remote_source_allowed),
            ("remote-destination", remote_dest_allowed),
            // The destination row stays clickable during a run. The key is
            // held to the stricter rule the other endpoint controls follow.
            ("choose-destination", !running),
            ("add-filter-rule", true),
            ("save-preset", true),
            ("show-page", true),
            ("capabilities", true),
            ("show-help-overlay", true),
        ] {
            if let Some(action) = self.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                action.set_enabled(enabled && free);
            }
        }
    }

    // -- closing with a run live --------------------------------------------

    /// May the window close now, and if not, what has to happen first? Derived
    /// from the run itself, so there is no "a transfer is live" flag to fall
    /// out of step with whether one is.
    pub(crate) fn close_step(&self) -> CloseStep {
        match self.imp().runner.borrow().as_ref() {
            Some(runner) if runner.is_live() => {
                if self.imp().close_when_done.get() {
                    CloseStep::Wait
                } else if runner.kind() == RunKind::DryRun {
                    CloseStep::StopThenClose
                } else {
                    CloseStep::Ask(runner.kind())
                }
            }
            _ => CloseStep::Close,
        }
    }

    /// The `close-request` handler. `Stop` keeps the window.
    pub(crate) fn on_close_request(&self) -> glib::Propagation {
        match self.close_step() {
            CloseStep::Close => return glib::Propagation::Proceed,
            CloseStep::StopThenClose => self.stop_then_close(),
            CloseStep::Ask(kind) => self.confirm_stop_then_close(kind),
            CloseStep::Wait => {}
        }
        glib::Propagation::Stop
    }

    /// Stop the run and close once it has ended. The closing is done by
    /// [`Self::finish_run`], from the run's completion — that is, when the
    /// process has exited and not when it has been asked to. `Runner::stop`
    /// bounds how long that can take.
    fn stop_then_close(&self) {
        let imp = self.imp();
        if let Some(runner) = imp.runner.borrow().as_ref() {
            imp.close_when_done.set(true);
            imp.overall_progress.set_text(Some("Stopping…"));
            runner.stop();
        }
    }

    fn confirm_stop_then_close(&self, kind: RunKind) {
        // Asked to close twice: the question is already on screen.
        if self.imp().close_dialog.upgrade().is_some() {
            return;
        }
        let dialog = adw::AlertDialog::builder()
            .heading("Stop the transfer and close?")
            .body(close_question_body(kind))
            .build();
        dialog.add_response("keep", "Keep Transferring");
        dialog.add_response("stop", "Stop and Close");
        dialog.set_response_appearance("stop", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("keep"));
        dialog.set_close_response("keep");
        dialog.connect_response(
            None,
            glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |_, response| win.on_close_response(response)
            ),
        );
        self.imp().close_dialog.set(Some(&dialog));
        dialog.present(Some(self));
    }

    /// The answer to the question, apart from the dialog that asks it, so the
    /// headless checks can give one.
    pub(crate) fn on_close_response(&self, response: &str) {
        if response != "stop" {
            return;
        }
        // Decided again rather than assumed: the transfer may have finished
        // while the question was on screen.
        match self.close_step() {
            CloseStep::Close => self.close(),
            CloseStep::Wait => {}
            CloseStep::StopThenClose | CloseStep::Ask(_) => self.stop_then_close(),
        }
    }

    /// A run has ended: let go of it. Returns `true` when the window was
    /// waiting on that to close — it has then been closed, and the caller has
    /// nothing left to show anyone.
    fn finish_run(&self) -> bool {
        let imp = self.imp();
        *imp.runner.borrow_mut() = None;
        self.refresh_action_sensitivity();
        // The question was about a run that is no longer there.
        if let Some(dialog) = imp.close_dialog.upgrade() {
            dialog.force_close();
        }
        if imp.close_when_done.get() {
            self.close();
            return true;
        }
        false
    }

    /// For application shutdown, which closes nothing and asks nothing: the
    /// run is handed over to be stopped, and with it the duty to hold it until
    /// it has ended (`signals::stop_all`, which stops every window's run at
    /// once rather than one after another). Taken out of the cell because the
    /// run's completion handler fires from inside that wait and borrows the
    /// cell itself.
    pub(crate) fn take_run_for_shutdown(&self) -> Option<Runner> {
        self.imp().runner.borrow_mut().take()
    }

    /// This window's share of the application's shutdown, for the checks:
    /// stop the run and do not return until it has ended. Bounded, like `stop`.
    #[cfg(feature = "selftest")]
    pub(crate) fn stop_run_for_shutdown(&self) {
        let runs: Vec<Runner> = self.take_run_for_shutdown().into_iter().collect();
        crate::signals::stop_all(&runs, &|| false);
    }

    /// For the check that signals a real `foresight`: a throttled transfer
    /// held by this window as one started from it would be, wired to the
    /// window's own handlers. `rsync` is whatever PATH says it is, which is
    /// how that check puts a process that ignores SIGTERM in its place.
    #[cfg(feature = "selftest")]
    pub(crate) fn hold_run_for_selftest(&self, kind: RunKind, argv: Vec<std::ffi::OsString>) {
        let on_event = glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |ev: Event| win.on_sync_event(ev)
        );
        let on_done = glib::clone!(
            #[weak(rename_to = win)]
            self,
            move |c: Completion| match kind {
                RunKind::DryRun => win.on_preview_done(c, false),
                RunKind::Transfer { .. } => win.on_sync_done(c),
            }
        );
        let runner = spawn_rsync(argv, on_event, on_done)
            .expect("rsync spawns")
            .with_kind(kind);
        *self.imp().runner.borrow_mut() = Some(runner);
        self.refresh_action_sensitivity();
    }

    fn on_start_clicked(&self) {
        let Some(job) = self.current_job() else {
            return;
        };
        // A move deletes from the source whatever transfers, so an exclude that
        // silently matches nothing is how a folder meant to stay put leaves.
        // The dry run is what can tell, so such a job always gets one first.
        let move_with_excludes = job.remove_source_files
            && job.reports_filter_hits()
            && job.filters.iter().any(|r| r.kind == FilterKind::Exclude);
        if job.delete || move_with_excludes {
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

        self.begin_preview(&job);
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

        let kind = RunKind::of(Mode::Preview, &argv);
        match spawn_rsync(argv, on_event, on_done) {
            Ok(runner) => {
                *imp.runner.borrow_mut() = Some(runner.with_kind(kind));
                self.refresh_action_sensitivity();
            }
            Err(e) => self.report_spawn_error(&e),
        }
    }

    /// Forget everything the previous dry run left behind, so that whatever
    /// this one collects is its own: the errors, the deletions to confirm, the
    /// hit counts and the list itself.
    fn begin_preview(&self, job: &Job) {
        let imp = self.imp();
        self.hide_banner();
        imp.run_errors.borrow_mut().clear();
        imp.deletions.borrow_mut().clear();
        imp.filter_banner.set(false);
        imp.filter_hits.replace(job.reports_filter_hits().then(|| {
            (
                job.filters.clone(),
                vec![RuleHits::default(); job.filters.len()],
            )
        }));
        if let Some(store) = imp.preview_store.get() {
            store.remove_all();
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
            Event::Filter(f) => {
                if let Some((rules, counts)) = imp.filter_hits.borrow_mut().as_mut() {
                    // rsync echoes the pattern verbatim, and the list refuses an
                    // exact (kind, pattern) repeat, so this finds one rule.
                    let hit = rules.iter().position(|r| {
                        (r.kind == FilterKind::Exclude) == f.action.is_exclude()
                            && r.pattern == f.pattern
                    });
                    if let Some(i) = hit {
                        use rsync_events::FilterAction::{Hiding, Showing};
                        if matches!(f.action, Hiding | Showing) {
                            counts[i].source += 1;
                        } else {
                            counts[i].dest += 1;
                        }
                    }
                }
            }
            Event::Message(m) if m.is_error => imp.run_errors.borrow_mut().push(m.text),
            Event::Message(_) | Event::Progress(_) => {}
        }
    }

    /// A dry run ended. Settle what it left behind, then show it.
    fn on_preview_done(&self, completion: Completion, then_confirm_start: bool) {
        // The window was waiting on this run in order to close, and has: there
        // is nobody left to settle anything for or show anything to.
        if self.finish_run() {
            return;
        }
        let outcome = self.settle_preview(&completion, then_confirm_start);
        self.present_preview(outcome, completion);
    }

    /// The half of [`Self::on_preview_done`] that decides: release the run,
    /// keep or discard what it collected, and say what may happen next. It
    /// presents nothing, so it can be driven without a dialog to dismiss.
    ///
    /// What a dry run collects is a finding only if the scan finished. The
    /// deletions list is confirmed as "what this sync will delete", and a rule
    /// with no hits is reported as "matched nothing"; from a scan that stopped
    /// part-way both are merely what had turned up so far. So an incomplete
    /// run leaves neither behind, and everything downstream sees no evidence
    /// rather than evidence of nothing.
    fn settle_preview(&self, completion: &Completion, then_confirm_start: bool) -> PreviewOutcome {
        let imp = self.imp();
        // Idempotent after `finish_run`; kept because the headless checks
        // drive this function on its own.
        *imp.runner.borrow_mut() = None;
        self.refresh_action_sensitivity();

        let outcome = PreviewOutcome::decide(completion.severity, then_confirm_start);
        if !outcome.is_complete() {
            imp.filter_hits.replace(None);
            imp.deletions.borrow_mut().clear();
        }
        // Complete: the hit counts are final, let the rule rows show them.
        // Incomplete: the rows go back to saying only what needs no run.
        self.rebuild_filter_rows();
        outcome
    }

    /// The half that shows: a toast, a banner, a dialog, or the confirmations
    /// that lead to the real sync.
    fn present_preview(&self, outcome: PreviewOutcome, completion: Completion) {
        let imp = self.imp();
        match outcome {
            PreviewOutcome::Confirm => self.confirm_idle_excludes_then_sync(),
            PreviewOutcome::Report => {
                let n = imp.preview_store.get().map(|s| s.n_items()).unwrap_or(0);
                self.toast(&format!("Preview: {n} change(s)"));
                let idle = self.idle_rules();
                if !idle.is_empty() {
                    let filters = imp.filters.borrow();
                    let names: Vec<&str> =
                        idle.iter().map(|&i| filters[i].pattern.as_str()).collect();
                    let title = match names.len() {
                        1 => format!("A filter rule matched nothing: {}", names[0]),
                        n => format!("{n} filter rules matched nothing: {}", names.join(", ")),
                    };
                    self.show_banner(&title, BannerButton::None);
                    imp.filter_banner.set(true);
                }
            }
            PreviewOutcome::Incomplete => {
                // No button: "New Job" belongs to a finished transfer. And not
                // the matched-nothing notice either, so editing the rules must
                // not take this down.
                let title =
                    incomplete_preview_notice(completion.code, imp.run_errors.borrow().len());
                self.show_banner(&title, BannerButton::None);
            }
            PreviewOutcome::Refused => {
                let body = refused_start_body(
                    imp.delete_row.is_active(),
                    imp.remove_source_row.is_active(),
                    completion.code,
                    &imp.run_errors.borrow(),
                );
                let dialog = adw::AlertDialog::builder()
                    .heading("Dry run incomplete — nothing was transferred")
                    .body(body)
                    .build();
                dialog.add_response("ok", "Close");
                dialog.set_default_response(Some("ok"));
                dialog.present(Some(self));
            }
            PreviewOutcome::Cancelled => self.toast("Dry run cancelled. Nothing was transferred."),
            // A dry run that itself failed (e.g. bad path): surface it as any
            // failed run is.
            // Only `Severity::Error` arrives here, which asks nothing about
            // what kind of run it was.
            PreviewOutcome::Failed => self.show_completion(completion, None),
        }
    }

    /// Excludes that held nothing back in the source. `dest` hits do not count:
    /// protecting a destination path keeps no file out of the transfer.
    fn idle_source_excludes(&self) -> Vec<String> {
        let Some(hits) = self.current_filter_hits() else {
            return Vec::new();
        };
        self.imp()
            .filters
            .borrow()
            .iter()
            .zip(hits)
            .filter(|(rule, h)| rule.kind == FilterKind::Exclude && h.source == 0)
            .map(|(rule, _)| rule.pattern.clone())
            .collect()
    }

    /// The gate in front of a move: if an exclude matched nothing, say so and
    /// make going on a decision. Then the deletion confirmation, as before.
    fn confirm_idle_excludes_then_sync(&self) {
        let idle = if self.imp().remove_source_row.is_active() {
            self.idle_source_excludes()
        } else {
            Vec::new()
        };
        if idle.is_empty() {
            self.confirm_deletions_then_sync();
            return;
        }

        let heading = match idle.len() {
            1 => "An exclude rule matched nothing".to_string(),
            n => format!("{n} exclude rules matched nothing"),
        };
        let body = format!(
            "{}\n\nNothing is being held back by {}, so whatever {} meant to keep \
             in the source will be moved with everything else — and a move deletes \
             each file from the source once it has transferred.\n\nThe Preview \
             lists exactly what will move.",
            idle.join("\n"),
            if idle.len() == 1 {
                "this rule"
            } else {
                "these rules"
            },
            if idle.len() == 1 {
                "it was"
            } else {
                "they were"
            },
        );
        let dialog = adw::AlertDialog::builder()
            .heading(heading)
            .body(body)
            .build();
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("move", "Move Anyway");
        dialog.set_response_appearance("move", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.connect_response(
            None,
            glib::clone!(
                #[weak(rename_to = win)]
                self,
                move |_, response| {
                    if response == "move" {
                        win.confirm_deletions_then_sync();
                    }
                }
            ),
        );
        dialog.present(Some(self));
    }

    fn run_sync(&self) {
        let Some(job) = self.current_job() else {
            return;
        };
        let imp = self.imp();

        self.hide_banner();
        imp.run_errors.borrow_mut().clear();
        imp.overall_progress.set_fraction(0.0);
        imp.overall_progress.set_text(Some("Starting…"));
        imp.current_file_label.set_label("");
        imp.main_stack.set_visible_child_name("transfer");

        let argv = job.build_argv(Mode::Sync);
        // Show the exact command for transparency (display only — never re-parsed).
        if let Some(store) = imp.log_store.get() {
            store.remove_all();
        }
        self.log_push(LogObject::command(format!("rsync {}", argv_display(&argv))));
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

        let kind = RunKind::of(Mode::Sync, &argv);
        match spawn_rsync(argv, on_event, on_done) {
            Ok(runner) => {
                *imp.runner.borrow_mut() = Some(runner.with_kind(kind));
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
                self.log_push(LogObject::change(&change));
            }
            // Being stopped in order to close: the bar says so, and keeps
            // saying so over whatever rsync had left to report.
            Event::Progress(_) if imp.close_when_done.get() => {}
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
                self.log_push(LogObject::message(&m));
            }
            // Only the dry run asks for these.
            Event::Filter(_) => {}
        }
    }

    fn on_sync_done(&self, completion: Completion) {
        let imp = self.imp();
        // Read before `finish_run` lets go of the run: what it was doing to
        // the two ends is part of what a partial result has to say.
        let kind = imp.runner.borrow().as_ref().map(Runner::kind);
        if self.finish_run() {
            return;
        }
        // Completion is process exit, never percent==100 (rsync can end at 99%).
        if completion.severity == Severity::Success {
            imp.overall_progress.set_fraction(1.0);
            imp.overall_progress.set_text(Some("Done"));
        }
        self.show_completion(completion, kind);
    }

    // -- completion / confirmation UI --------------------------------------

    /// Map a [`Completion`] to the right surface: toast (success), banner
    /// (partial 23/24/25 — never a failure wall), toast (cancelled), or a
    /// details dialog (error). `kind` is what the run was, where that is
    /// known; only a partial result has anything to say about it.
    fn show_completion(&self, completion: Completion, kind: Option<RunKind>) {
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
            // Still only a banner: nothing is put in front of the user. What
            // changes is that the banner leads to what rsync reported.
            Severity::Partial => {
                let report = PartialReport::new(&completion, kind, &self.imp().run_errors.borrow());
                self.show_banner(&report.banner_text(), report.button());
            }
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

    /// A preset that could not be written. A dialog rather than a toast: the
    /// user has to do something about it (free space, fix permissions), and a
    /// toast is gone before the path in it can be read. Not
    /// `show_error_dialog`, which is headed "Sync failed" and appends the last
    /// run's rsync errors.
    fn show_preset_error(&self, heading: &str, body: &str) {
        let dialog = adw::AlertDialog::builder()
            .heading(heading)
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

    /// What a partial transfer reported, on request: the banner's button
    /// leads here. Never opened by the run itself.
    fn present_partial_report(&self, report: &PartialReport) {
        let dialog = adw::AlertDialog::builder()
            .heading(PARTIAL_HEADING)
            .body(report.body())
            .build();
        dialog.add_response("ok", "Close");
        dialog.set_default_response(Some("ok"));
        dialog.present(Some(self));
    }

    /// Put the banner up. Every use of it comes through here, so that the
    /// button it shows and what the button does are set together. The
    /// matched-nothing notice marks itself as such after calling this.
    fn show_banner(&self, text: &str, button: BannerButton) {
        let imp = self.imp();
        let banner = imp.result_banner.get();
        banner.set_title(text);
        banner.set_button_label(button.label());
        imp.banner_button.replace(button);
        imp.filter_banner.set(false);
        banner.set_revealed(true);
    }

    /// Take the banner down, and with it whatever its button stood for: a
    /// report on a run is dropped when the banner that led to it goes. The
    /// label is left as it is — the banner is on screen while it slides away,
    /// and a button changing under it would be seen — so a press that lands
    /// in that moment does nothing.
    fn hide_banner(&self) {
        let imp = self.imp();
        imp.result_banner.set_revealed(false);
        imp.banner_button.replace(BannerButton::None);
    }

    fn toast(&self, text: &str) {
        self.imp().toast_overlay.add_toast(adw::Toast::new(text));
    }
}

/// What a finished dry run allows next — the decision, apart from how it is
/// shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PreviewOutcome {
    /// A plain Dry Run that finished: report what it found.
    Report,
    /// The dry run in front of Start finished: on to the confirmations.
    Confirm,
    /// A plain Dry Run that rsync could not finish. The list is worth looking
    /// at and has to be labelled as partial.
    Incomplete,
    /// The dry run in front of Start could not finish, so the sync is not
    /// started: Start only asks for a dry run when the job deletes or moves,
    /// and what it would delete or move is exactly what is now unknown.
    Refused,
    /// Stopped by the user. Nothing follows.
    Cancelled,
    /// rsync failed outright. Nothing follows.
    Failed,
}

impl PreviewOutcome {
    /// Exit 23/24/25 is tolerated for a transfer, where it means "most of it
    /// arrived". For a dry run it means "most of it was looked at", which is
    /// not something a confirmation can be built on.
    fn decide(severity: Severity, then_confirm_start: bool) -> Self {
        match (severity, then_confirm_start) {
            (Severity::Success, false) => Self::Report,
            (Severity::Success, true) => Self::Confirm,
            (Severity::Partial, false) => Self::Incomplete,
            (Severity::Partial, true) => Self::Refused,
            (Severity::Cancelled, _) => Self::Cancelled,
            (Severity::Error, _) => Self::Failed,
        }
    }

    /// The scan finished, so what it collected describes the whole job.
    fn is_complete(self) -> bool {
        matches!(self, Self::Report | Self::Confirm)
    }
}

/// What the banner's one button stands for. The banner is shared — a finished
/// transfer, a partial one, and two notices about a dry run — so the button's
/// meaning is this value and nothing else: never the label read back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum BannerButton {
    /// No button: the banner is a notice and offers nothing, or is not up.
    #[default]
    None,
    /// Clear the form for the next job.
    NewJob,
    /// Show what a partial transfer reported.
    Problems(PartialReport),
}

impl BannerButton {
    fn label(&self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::NewJob => Some("New Job"),
            Self::Problems(_) => Some("Show Problems"),
        }
    }
}

/// A transfer that ended with exit 23, 24 or 25: what is known about it once
/// the run itself has gone. Made when the run ends and from then on the only
/// source for the banner and the dialog, so neither can describe a later run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartialReport {
    /// `classify_exit`'s sentence for the exit code.
    message: String,
    code: Option<i32>,
    /// What the run was doing to the two ends, where that is known.
    kind: Option<RunKind>,
    /// The error lines rsync printed, without its closing summary.
    problems: Vec<String>,
}

impl PartialReport {
    fn new(completion: &Completion, kind: Option<RunKind>, errors: &[String]) -> Self {
        Self {
            message: completion.message.clone(),
            code: completion.code,
            kind,
            problems: reported_problems(errors),
        }
    }

    /// With nothing to list there is nothing to open, and the banner keeps
    /// the button a finished transfer has.
    fn button(&self) -> BannerButton {
        if self.problems.is_empty() {
            BannerButton::NewJob
        } else {
            BannerButton::Problems(self.clone())
        }
    }

    fn banner_text(&self) -> String {
        partial_banner_text(&self.message, self.code, self.problems.len())
    }

    fn body(&self) -> String {
        partial_report_body(self.kind, self.code, &self.problems)
    }
}

/// Heading of the dialog a partial transfer's banner leads to. Not "Sync
/// failed": the sync ran to its end.
const PARTIAL_HEADING: &str = "Sync finished with problems";

/// rsync's one line saying that it stopped deleting because of an I/O error.
const DELETION_SKIPPED: &str = "IO error encountered -- skipping file deletion";

/// The collected error lines without rsync's closing summary (`rsync error:
/// … (code 23) at main.c(…)`). That line restates the exit code, once per end
/// of the transfer, and names nothing: counted, it would make one unreadable
/// file "2 problems", or 3 over ssh.
fn reported_problems(errors: &[String]) -> Vec<String> {
    errors
        .iter()
        .filter(|line| !(line.starts_with("rsync error: ") && line.contains("(code ")))
        .cloned()
        .collect()
}

/// Banner text for a partial transfer: `classify_exit`'s sentence, then how
/// many problems rsync reported. When none were collected — exit 24, whose
/// "file has vanished" lines are warnings to rsync and to the parser — the
/// sentence is all there is to say, with where rsync's own words are.
fn partial_banner_text(message: &str, code: Option<i32>, problems: usize) -> String {
    let mut text = message.to_string();
    match problems {
        0 => {
            text.push_str(
                " No specific errors were collected; rsync's output is in the activity log",
            );
            if let Some(code) = code {
                text.push_str(&format!(" (exit code {code})"));
            }
            text.push('.');
        }
        1 => text.push_str(" 1 problem reported."),
        n => text.push_str(&format!(" {n} problems reported.")),
    }
    text
}

/// Body of the dialog a partial transfer's banner leads to: what "partial"
/// means, what it did to deletions and to a move where that is true of this
/// run, the exit code, and what rsync reported. Says nothing about which
/// files were deleted: that is not known here.
fn partial_report_body(kind: Option<RunKind>, code: Option<i32>, problems: &[String]) -> String {
    let mut body = String::from("The sync ran to its end, but not everything went through. ");
    body.push_str(&match problems.len() {
        0 => "rsync reported no error lines.".to_string(),
        1 => "rsync reported 1 problem, shown below.".to_string(),
        n => format!("rsync reported {n} problems, shown below."),
    });
    if let Some(RunKind::Transfer { moves, deletes }) = kind {
        if deletes && problems.iter().any(|p| p.starts_with(DELETION_SKIPPED)) {
            body.push_str(
                "\n\nMirror deletions stopped part-way. Some files may already have \
                 been deleted from the destination; others that were due to be \
                 deleted were left there.",
            );
        }
        if moves {
            body.push_str(
                "\n\nThis was a move. Files that transferred were removed from the \
                 source; files that did not transfer are still in the source.",
            );
        }
    }
    if let Some(code) = code {
        body.push_str(&format!("\n\nrsync exit code {code}."));
    }
    if !problems.is_empty() {
        const MAX: usize = 20;
        body.push_str("\n\n");
        body.push_str(&problems[..problems.len().min(MAX)].join("\n"));
        if problems.len() > MAX {
            body.push_str(&format!("\n…and {} more", problems.len() - MAX));
        }
    }
    body
}

/// Banner text for a plain Dry Run that rsync could not finish. Written for
/// the dry run rather than borrowed from `classify_exit`, whose wording is
/// about files that were "not transferred" — here none were meant to be.
fn incomplete_preview_notice(code: Option<i32>, errors: usize) -> String {
    let mut text = String::from("Dry run incomplete");
    match errors {
        0 => {}
        1 => text.push_str(" — rsync reported 1 error"),
        n => text.push_str(&format!(" — rsync reported {n} errors")),
    }
    if let Some(code) = code {
        text.push_str(&format!(" (exit code {code})"));
    }
    text.push_str(". This list may be missing changes. Nothing was transferred.");
    text
}

/// Body of the dialog shown when Start is refused because its dry run was
/// incomplete: that nothing happened, why the sync was not started, and what
/// rsync reported.
fn refused_start_body(deletes: bool, moves: bool, code: Option<i32>, errors: &[String]) -> String {
    let mut body = String::from(
        "The sync was not started. Nothing was transferred, deleted or moved.\n\n\
         rsync could not finish the dry run, so the Preview may be missing changes.",
    );
    if deletes {
        body.push_str(
            " Mirror deletions removes files from the destination, and the list \
             of them to confirm cannot be trusted to be complete.",
        );
    }
    if moves {
        body.push_str(
            " Move files removes each file from the source once it has \
             transferred, and whether the exclude rules hold anything back \
             cannot be judged.",
        );
    }
    body.push_str(
        "\n\nThe Preview shows what rsync did find. Fix what is reported below, \
         then start again.",
    );
    if let Some(code) = code {
        body.push_str(&format!("\n\nrsync exit code {code}."));
    }
    if !errors.is_empty() {
        const MAX: usize = 20;
        body.push_str("\n\n");
        body.push_str(&errors[..errors.len().min(MAX)].join("\n"));
        if errors.len() > MAX {
            body.push_str(&format!("\n…and {} more", errors.len() - MAX));
        }
    }
    body
}

/// The body of the "stop and close?" question. Says only what is true of the
/// run it is asked about: the sentences on moving and deleting appear when
/// that run is doing those things.
fn close_question_body(kind: RunKind) -> String {
    let mut body = String::from(
        "A transfer is in progress. Stopping it ends rsync where it is: files \
         already transferred stay in the destination, and the rest are not \
         transferred.",
    );
    if let RunKind::Transfer { moves, deletes } = kind {
        if moves {
            body.push_str("\n\nFiles already moved are gone from the source.");
        }
        if deletes {
            body.push_str("\n\nFiles already deleted from the destination stay deleted.");
        }
    }
    body
}

/// What the Details dialog says when the presets file did not load.
fn unreadable_presets_notice(error: &profiles::LoadError) -> String {
    format!(
        "Your saved presets were not loaded: {error}.\n\n\
         The file has not been changed. If you save a preset, the file will \
         first be moved aside, unchanged, and a new one started in its place."
    )
}

/// What is said after a save that had to move the unreadable file aside.
fn moved_aside_notice(to: &std::path::Path) -> String {
    format!(
        "The presets file that was there could not be read, so it was not \
         overwritten. It was moved, unchanged, to “{}”, and a new presets file \
         was started in its place.",
        to.display()
    )
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

/// Parse an rsync rate token (`"85M"`, `"500K"`, `"2G"`, or a bare number =
/// KiB/s) back into `(spin value, unit index)` where the unit index is
/// `0=KB/s, 1=MB/s, 2=GB/s`. Empty → `(0, MB/s)`.
fn parse_bwlimit(token: &str) -> (f64, u32) {
    let t = token.trim();
    if t.is_empty() {
        return (0.0, 1);
    }
    let (num, unit) = match t.as_bytes().last() {
        Some(b'K' | b'k') => (&t[..t.len() - 1], 0),
        Some(b'M' | b'm') => (&t[..t.len() - 1], 1),
        Some(b'G' | b'g') => (&t[..t.len() - 1], 2),
        _ => (t, 0), // a bare number is KiB/s in rsync
    };
    (num.trim().parse::<f64>().unwrap_or(0.0), unit)
}

/// Headless widget checks, compiled only under the `selftest` feature and
/// driven from `main` so they run on the GTK main thread.
///
/// These cover what `cargo test` structurally cannot: libtest gives each test
/// its own thread and GTK aborts when touched off-main. The filter-rule editor
/// lives almost entirely in widget state, so without this it would be tested
/// only through its data layer — which is how a preset bug survived to release
/// once already.
#[cfg(feature = "selftest")]
impl ForesightWindow {
    pub(crate) fn run_selftest(&self) -> (u32, u32) {
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
        // Patterns only — for the checks where the kind is not what's at stake.
        let rules = |w: &ForesightWindow| -> Vec<String> {
            w.imp()
                .filters
                .borrow()
                .iter()
                .map(|r| r.pattern.clone())
                .collect()
        };
        // Kind + pattern, rendered compactly so a failure prints legibly.
        let kinded = |w: &ForesightWindow| -> Vec<String> {
            w.imp()
                .filters
                .borrow()
                .iter()
                .map(|r| format!("{}:{}", r.kind.as_key(), r.pattern))
                .collect()
        };
        let ex = FilterKind::Exclude;
        let inc = FilterKind::Include;

        // Adding: trim the edges, refuse nothing-rules, refuse duplicates.
        self.set_filters(&[]);
        let added = self.add_filter(ex, "  *.tmp  ");
        check(
            "add trims surrounding whitespace",
            added && rules(self) == ["*.tmp"],
            format!("{:?}", rules(self)),
        );
        check(
            "exact duplicate refused",
            !self.add_filter(ex, "*.tmp"),
            "accepted".into(),
        );
        check(
            "duplicate-after-trim refused",
            !self.add_filter(ex, "   *.tmp "),
            "accepted".into(),
        );
        check("empty refused", !self.add_filter(ex, ""), "accepted".into());
        check(
            "whitespace-only refused",
            !self.add_filter(ex, "      "),
            "accepted".into(),
        );
        check(
            "rejected rules add no junk",
            rules(self) == ["*.tmp"],
            format!("{:?}", rules(self)),
        );
        // The same pattern with the other kind is a different rule: rsync
        // resolves the contradiction by position, so the list must hold both.
        check(
            "same pattern, other kind accepted",
            self.add_filter(inc, "*.tmp") && kinded(self) == ["exclude:*.tmp", "include:*.tmp"],
            format!("{:?}", kinded(self)),
        );

        // The add-rule row must actually appear when the expander is opened.
        // It is the only way to add a rule, so if it does not render the whole
        // editor is unreachable while looking perfectly fine — which is exactly
        // what a screenshot of an open-but-empty expander would show.
        {
            let imp = self.imp();
            // AdwExpanderRow reveals its rows through an animation, so a child
            // is legitimately unmapped for a few frames after set_expanded.
            // Turn animations off and then pump the loop for real time, or this
            // check measures the animation rather than the widget tree.
            if let Some(settings) = gtk::Settings::default() {
                settings.set_gtk_enable_animations(false);
            }
            // Filter rules lives inside the Advanced expander, which has no id
            // in the Blueprint — walk up and open every expander above it, or
            // the child is unmapped because its parent is closed, not because
            // anything is wrong with it.
            let mut ancestor = imp.filters_row.parent();
            while let Some(w) = ancestor {
                if let Some(exp) = w.downcast_ref::<adw::ExpanderRow>() {
                    exp.set_expanded(true);
                }
                ancestor = w.parent();
            }
            imp.filters_row.set_expanded(true);

            let ctx = glib::MainContext::default();
            let elapsed = std::rc::Rc::new(std::cell::Cell::new(false));
            glib::timeout_add_local_once(std::time::Duration::from_millis(600), {
                let elapsed = elapsed.clone();
                move || elapsed.set(true)
            });
            while !elapsed.get() {
                ctx.iteration(true);
            }
            // Compared against a control that is unconditionally on screen, so
            // "not mapped" cannot be an artefact of the window not being up yet.
            let control = imp.dest_row.is_mapped();
            check(
                "the add-rule entry renders when the expander is open",
                !control || (imp.filter_entry.is_mapped() && imp.filter_kind.is_mapped()),
                format!(
                    "control(dest_row)={control}, filters_row mapped={}, entry mapped={} dropdown mapped={} expanded={}",
                    imp.filters_row.is_mapped(),
                    imp.filter_entry.is_mapped(),
                    imp.filter_kind.is_mapped(),
                    imp.filters_row.is_expanded()
                ),
            );
        }

        // The path a user actually takes: type a pattern, pick a kind, apply.
        // Everything above calls `add_filter` directly, so without this the
        // dropdown could be wired to nothing and every check would still pass.
        self.set_filters(&[]);
        let imp = self.imp();
        imp.filter_kind.set_selected(1); // Include
        imp.filter_entry.set_text("*.jpg");
        imp.filter_entry.emit_by_name::<()>("apply", &[]);
        imp.filter_kind.set_selected(0); // Exclude
        imp.filter_entry.set_text("*.tmp");
        imp.filter_entry.emit_by_name::<()>("apply", &[]);
        check(
            "the entry adds with the kind the dropdown shows",
            kinded(self) == ["include:*.jpg", "exclude:*.tmp"],
            format!("{:?}", kinded(self)),
        );
        check(
            "a committed rule clears the entry",
            imp.filter_entry.text().is_empty(),
            format!("{:?}", imp.filter_entry.text()),
        );
        // A refused rule must stay in the entry to be corrected, not vanish.
        imp.filter_entry.set_text("*.tmp");
        imp.filter_entry.emit_by_name::<()>("apply", &[]);
        check(
            "a refused rule stays in the entry",
            imp.filter_entry.text() == "*.tmp" && kinded(self).len() == 2,
            format!("{:?} / {:?}", imp.filter_entry.text(), kinded(self)),
        );
        imp.filter_entry.set_text("");

        // The point of the list: interior spaces are content, edges are typos.
        self.set_filters(&[]);
        self.add_filter(ex, "  My Documents/  ");
        check(
            "interior spaces kept, edges trimmed",
            rules(self) == ["My Documents/"],
            format!("{:?}", rules(self)),
        );

        // A complete job is needed before argv can be inspected.
        let dir = std::path::PathBuf::from(
            std::env::var("FORESIGHT_SELFTEST_DIR").expect("FORESIGHT_SELFTEST_DIR"),
        );
        let _ = std::fs::create_dir_all(dir.join("src"));
        let _ = std::fs::create_dir_all(dir.join("dst"));
        self.add_source(&gio::File::for_path(dir.join("src")));
        self.set_dest(&gio::File::for_path(dir.join("dst")));
        let argv_rules = |w: &ForesightWindow| -> Vec<String> {
            w.current_job()
                .expect("sources + dest are set")
                .build_argv(crate::job::Mode::Sync)
                .iter()
                .filter(|a| {
                    a.as_encoded_bytes().starts_with(b"--exclude=")
                        || a.as_encoded_bytes().starts_with(b"--include=")
                })
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };

        // rsync applies filter rules in order and the first match wins, so list
        // order is behaviour, not presentation.
        self.set_filters(&[
            FilterRule::exclude("z-last"),
            FilterRule::include("a-first"),
            FilterRule::exclude("m-middle"),
        ]);
        let got = argv_rules(self);
        check(
            "list order and kind reach argv unchanged",
            got == [
                "--exclude=z-last",
                "--include=a-first",
                "--exclude=m-middle",
            ],
            format!("{got:?}"),
        );

        self.set_filters(&[FilterRule::exclude("My Documents/")]);
        let got = argv_rules(self);
        check(
            "a spaced rule is one argv element",
            got == ["--exclude=My Documents/"],
            format!("{got:?}"),
        );

        // Reordering is the whole reason the list is ordered: moving an include
        // above a broader exclude is what carves an exception out of it.
        self.set_filters(&[FilterRule::exclude("*"), FilterRule::include("*.jpg")]);
        self.move_filter(1, -1);
        let got = argv_rules(self);
        check(
            "moving a rule up changes which one wins",
            got == ["--include=*.jpg", "--exclude=*"],
            format!("{got:?}"),
        );
        self.move_filter(0, 1);
        let got = argv_rules(self);
        check(
            "moving a rule down puts it back",
            got == ["--exclude=*", "--include=*.jpg"],
            format!("{got:?}"),
        );

        // The ends must be inert rather than wrap around or panic — the arrows
        // are insensitive there, but nothing else may rely on that.
        self.set_filters(&[FilterRule::exclude("a"), FilterRule::exclude("b")]);
        self.move_filter(0, -1);
        self.move_filter(1, 1);
        check(
            "moving past either end is a no-op",
            rules(self) == ["a", "b"],
            format!("{:?}", rules(self)),
        );
        self.move_filter(9, -1); // stale index, as a rebuilt row could hold
        check(
            "an out-of-range move is ignored",
            rules(self) == ["a", "b"],
            format!("{:?}", rules(self)),
        );

        self.set_filters(&[
            FilterRule::exclude("keep-a"),
            FilterRule::include("drop-me"),
            FilterRule::exclude("keep-b"),
        ]);
        self.remove_filter(1);
        check(
            "removing the middle rule drops only it",
            rules(self) == ["keep-a", "keep-b"] && argv_rules(self).len() == 2,
            format!("{:?}", rules(self)),
        );

        // Every mutation rebuilds the rows, so the widgets and the model must
        // still agree on how many rules there are.
        check(
            "rows track the model after a rebuild",
            self.imp().filter_rows.borrow().len() == self.imp().filters.borrow().len(),
            format!(
                "{} rows vs {} rules",
                self.imp().filter_rows.borrow().len(),
                self.imp().filters.borrow().len()
            ),
        );

        self.set_filters(&[
            FilterRule::exclude("a"),
            FilterRule::exclude("b"),
            FilterRule::exclude("c"),
        ]);
        self.set_filters(&[FilterRule::exclude("x")]);
        check(
            "set_filters replaces rather than appends",
            rules(self) == ["x"] && self.imp().filter_rows.borrow().len() == 1,
            format!("{:?}", rules(self)),
        );

        // The count is the only cue when the expander is collapsed.
        self.set_filters(&[]);
        let empty = self.imp().filters_row.subtitle().to_string();
        self.add_filter(ex, "one");
        let one = self.imp().filters_row.subtitle().to_string();
        self.add_filter(inc, "two");
        let two = self.imp().filters_row.subtitle().to_string();
        check(
            "subtitle counts the rules",
            empty.contains("--exclude")
                && empty.contains("--include")
                && one == "1 rule"
                && two.starts_with("2 rules"),
            format!("{empty:?} / {one:?} / {two:?}"),
        );

        self.set_filters(&[FilterRule::exclude("a"), FilterRule::include("b")]);
        self.clear_job();
        check(
            "New Job clears the rules",
            rules(self).is_empty() && self.imp().filter_rows.borrow().is_empty(),
            format!("{:?}", rules(self)),
        );

        self.set_filters(&[
            FilterRule::exclude("*.tmp"),
            FilterRule::include("My Documents/"),
        ]);
        let snapshot = self.read_advanced();
        self.set_filters(&[]);
        self.apply_advanced(&snapshot);
        check(
            "a preset restores rules, kinds and order verbatim",
            kinded(self) == ["exclude:*.tmp", "include:My Documents/"],
            format!("{:?}", kinded(self)),
        );

        // A preset is only worth anything if it survives the disk. Kind and
        // order are part of what must come back.
        self.upsert_preset("Filter round trip".into());
        let stored = crate::profiles::load()
            .unwrap_or_default()
            .into_iter()
            .find(|p| p.name == "Filter round trip")
            .map(|p| p.filters)
            .unwrap_or_default();
        check(
            "kinds and order survive a save/load cycle",
            stored
                == [
                    FilterRule::exclude("*.tmp"),
                    FilterRule::include("My Documents/"),
                ],
            format!("{stored:?}"),
        );

        // -- remote endpoints (M5 part 3) -----------------------------------
        //
        // rsync refuses a job that is remote at both ends, and refuses to mix
        // local sources with a remote one. Those rules live entirely in widget
        // state, so this is the only place they get checked.
        self.set_filters(&[]);
        let ep = |host: &str, path: &str| crate::endpoint::Endpoint {
            user: Some("miguel".into()),
            host: host.into(),
            port: None,
            path: path.into(),
        };
        let operands = |w: &ForesightWindow| -> Vec<String> {
            w.current_job()
                .map(|j| {
                    let argv = j.build_argv(crate::job::Mode::Sync);
                    argv.iter()
                        .rev()
                        .take(2)
                        .rev()
                        .map(|a| a.to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default()
        };

        // Push: local source, remote destination.
        self.clear_job();
        self.add_source(&gio::File::for_path(dir.join("src")));
        self.set_remote(RemoteSide::Dest, ep("nas.local", "/srv/backup"));
        check(
            "a remote destination satisfies its side",
            self.both_selected(),
            "Preview/Start still disabled".into(),
        );
        let got = operands(self);
        check(
            "the remote operand is the last argument",
            got.last().map(String::as_str) == Some("miguel@nas.local:/srv/backup"),
            format!("{got:?}"),
        );
        check(
            "a remote job carries our strict ssh command",
            self.current_job().is_some_and(|j| {
                let argv = j.build_argv(crate::job::Mode::Sync);
                let has = |s: &str| argv.iter().any(|a| a.to_string_lossy() == s);
                j.remote_shell
                    .as_deref()
                    .is_some_and(|c| c.contains("StrictHostKeyChecking=yes"))
                    && has("-e")
                    // -s would break rrsync-restricted targets, and rsync's
                    // default escaping already protects the remote path.
                    && !has("-s")
            }),
            "missing -e or the strict policy, or sending -s".into(),
        );
        check(
            "both ends cannot be remote",
            !self.imp().add_remote_source_button.is_sensitive(),
            "the remote-source button is still live".into(),
        );

        // Picking a local folder must replace the remote destination, not sit
        // beside it — there is exactly one destination.
        self.set_dest(&gio::File::for_path(dir.join("dst")));
        check(
            "a local folder replaces a remote destination",
            self.remote_on(RemoteSide::Dest).is_none()
                && self.imp().add_remote_source_button.is_sensitive(),
            "the remote destination survived".into(),
        );

        // Pull: remote source, local destination.
        self.clear_job();
        self.set_dest(&gio::File::for_path(dir.join("dst")));
        self.add_source(&gio::File::for_path(dir.join("src")));
        self.set_remote(RemoteSide::Source, ep("nas.local", "/srv/photos"));
        check(
            "a remote source clears the local ones",
            rules(self).is_empty() || self.imp().sources.borrow().is_empty(),
            format!("{} local sources remain", self.imp().sources.borrow().len()),
        );
        check(
            "local source buttons are closed off during a pull",
            !self.imp().add_folder_button.is_sensitive()
                && !self.imp().add_file_button.is_sensitive()
                && !self.imp().remote_dest_button.is_sensitive(),
            "a local/remote mix is still reachable".into(),
        );
        let got = operands(self);
        check(
            "the remote source is the first operand",
            got.first().map(String::as_str) == Some("miguel@nas.local:/srv/photos"),
            format!("{got:?}"),
        );

        // A remote path was typed, not picked, so nothing knows it is a folder:
        // the two single-folder options must not be offered for it.
        check(
            "single-folder options are unavailable for a remote source",
            !self.imp().contents_row.is_sensitive() && !self.imp().delete_row.is_sensitive(),
            "offered against an unstat'd remote path".into(),
        );

        // Removing it hands the local controls back.
        self.clear_remote();
        check(
            "clearing the remote source restores local sources",
            self.imp().add_folder_button.is_sensitive()
                && self.imp().remote_dest_button.is_sensitive()
                && !self.imp().clear_remote_source_button.is_visible(),
            "controls stayed locked".into(),
        );

        // A finished remote run must hand everything back. The run lock used to
        // be set and never lifted, and New Job never repainted the remote
        // state, so after one SSH transfer both remote buttons stayed grey and
        // the destination row still read "Remote folder" — a dead window. Hold
        // a real Runner the way a live transfer does, then let it go.
        self.clear_job();
        self.add_source(&gio::File::for_path(dir.join("src")));
        self.set_remote(RemoteSide::Dest, ep("nas.local", "/srv/backup"));
        match spawn_rsync(vec!["--version".into()], |_| {}, |_| {}) {
            Ok(runner) => {
                *self.imp().runner.borrow_mut() = Some(runner);
                self.refresh_action_sensitivity();
                check(
                    "a live run locks every endpoint button",
                    !self.imp().add_folder_button.is_sensitive()
                        && !self.imp().add_file_button.is_sensitive()
                        && !self.imp().add_remote_source_button.is_sensitive()
                        && !self.imp().remote_dest_button.is_sensitive(),
                    "an endpoint can change mid-transfer".into(),
                );
                *self.imp().runner.borrow_mut() = None;
                self.refresh_action_sensitivity();
                check(
                    "the run lock lifts when the run ends",
                    self.imp().add_folder_button.is_sensitive()
                        && self.imp().remote_dest_button.is_sensitive()
                        // Still a push, so the other side stays closed.
                        && !self.imp().add_remote_source_button.is_sensitive(),
                    "buttons stayed locked after the run".into(),
                );
            }
            Err(e) => check("rsync spawns for the run-lock check", false, e.to_string()),
        }
        self.clear_job();
        check(
            "New Job after a remote run restores both remote buttons",
            self.imp().add_remote_source_button.is_sensitive()
                && self.imp().remote_dest_button.is_sensitive(),
            "a remote button is still grey".into(),
        );
        check(
            "New Job repaints the destination row as local",
            self.imp().dest_row.title() == "Folder"
                && self.imp().dest_row.subtitle().as_deref() == Some("Not selected"),
            format!("row still reads {:?}", self.imp().dest_row.title()),
        );

        // An IPv6 link-local endpoint has to survive with its scope id — the
        // phone-over-hotspot case, and the one a naive `host:path` split eats.
        self.clear_job();
        self.add_source(&gio::File::for_path(dir.join("src")));
        let mut v6 = ep("fe80::1%wlo1", "/data");
        v6.port = Some(2222);
        self.set_remote(RemoteSide::Dest, v6);
        let got = operands(self);
        check(
            "an IPv6 link-local endpoint keeps its brackets and scope id",
            got.last().map(String::as_str) == Some("miguel@[fe80::1%wlo1]:/data"),
            format!("{got:?}"),
        );
        check(
            "a non-default port reaches the ssh command",
            self.current_job()
                .and_then(|j| j.remote_shell)
                .is_some_and(|c| c.ends_with(" -p 2222")),
            "port missing from -e".into(),
        );

        self.clear_job();
        check(
            "New Job clears the remote endpoint",
            self.imp().remote.borrow().is_none(),
            "a remote host survived New Job".into(),
        );

        // -- rules that do nothing -------------------------------------------
        //
        // The soak-found failure: an exclude pasted as a full disk path matches
        // nothing, rsync says nothing, and with Move on the folder it was meant
        // to hold back leaves the source. Two defences, checked separately.
        self.clear_job();
        let _ = std::fs::create_dir_all(dir.join("src/private"));
        self.add_source(&gio::File::for_path(dir.join("src")));
        self.set_dest(&gio::File::for_path(dir.join("dst")));
        let pasted = dir.join("src/private").display().to_string();
        self.add_filter(FilterKind::Exclude, &pasted);
        self.add_filter(FilterKind::Exclude, "*.tmp");
        let subtitle_of = |w: &ForesightWindow, i: usize| {
            w.imp().filter_rows.borrow()[i]
                .subtitle()
                .map(|s| s.to_string())
                .unwrap_or_default()
        };
        check(
            "a full-path exclude is flagged without running anything",
            self.idle_rules() == [0] && subtitle_of(self, 0).contains("can never match"),
            format!(
                "idle={:?} subtitle={:?}",
                self.idle_rules(),
                subtitle_of(self, 0)
            ),
        );
        check(
            "the collapsed expander still says a rule matches nothing",
            self.imp()
                .filters_row
                .subtitle()
                .contains("1 matches nothing"),
            self.imp().filters_row.subtitle().to_string(),
        );
        let fix = self.imp().filters.borrow()[0]
            .dead_anchor(&self.transfer_top())
            .and_then(|d| d.suggestion);
        check(
            "the offered fix is the path relative to the transfer",
            fix.as_deref() == Some("/src/private"),
            format!("{fix:?}"),
        );
        self.apply_filter_fix(0, "/src/private");
        check(
            "applying the fix clears the warning",
            rules(self) == ["/src/private", "*.tmp"] && self.idle_rules().is_empty(),
            format!("{:?} idle={:?}", rules(self), self.idle_rules()),
        );

        // "Copy contents" re-anchors `/`: the rule that was right is now dead,
        // with no edit to the list at all.
        self.imp().contents_row.set_active(true);
        let fix = self.imp().filters.borrow()[0]
            .dead_anchor(&self.transfer_top())
            .and_then(|d| d.suggestion);
        check(
            "Copy contents re-anchors an existing rule",
            self.idle_rules() == [0] && fix.as_deref() == Some("/private"),
            format!("idle={:?} fix={fix:?}", self.idle_rules()),
        );
        self.imp().contents_row.set_active(false);

        // Evidence from a dry run, fed through the real event handler. `*.tmp`
        // is a perfectly valid rule that simply matches nothing here — only the
        // run can know that.
        self.imp().remove_source_row.set_active(true);
        let job = self.current_job().expect("sources + dest are set");
        check(
            "the dry run asks rsync which rules matched",
            job.build_argv(Mode::Preview)
                .iter()
                .any(|a| a == "--debug=FILTER"),
            "--debug=FILTER missing".into(),
        );
        self.imp().filter_hits.replace(Some((
            job.filters.clone(),
            vec![RuleHits::default(); job.filters.len()],
        )));
        self.on_preview_event(Event::Filter(rsync_events::FilterMatch {
            action: rsync_events::FilterAction::Hiding,
            is_dir: true,
            path: "src/private".into(),
            pattern: "/src/private".into(),
        }));
        self.rebuild_filter_rows();
        check(
            "a rule the dry run saw match is credited, the other is flagged",
            subtitle_of(self, 0).contains("matched 1 path")
                && subtitle_of(self, 1).contains("matched nothing")
                && self.idle_rules() == [1],
            format!("{:?} / {:?}", subtitle_of(self, 0), subtitle_of(self, 1)),
        );
        check(
            "a move is gated on excludes that held nothing back",
            self.idle_source_excludes() == ["*.tmp"],
            format!("{:?}", self.idle_source_excludes()),
        );

        // A destination-side hit is not something held back from a move.
        self.on_preview_event(Event::Filter(rsync_events::FilterMatch {
            action: rsync_events::FilterAction::Protecting,
            is_dir: false,
            path: "src/old.tmp".into(),
            pattern: "*.tmp".into(),
        }));
        check(
            "protecting a destination path does not satisfy the move gate",
            self.idle_source_excludes() == ["*.tmp"] && self.idle_rules().is_empty(),
            format!("{:?}", self.idle_source_excludes()),
        );

        // Stale evidence must not outlive the job it described.
        self.add_filter(FilterKind::Exclude, "later");
        check(
            "editing the rules drops the last run's verdicts",
            self.current_filter_hits().is_none() && !subtitle_of(self, 0).contains("matched"),
            subtitle_of(self, 0),
        );
        self.clear_job();

        // -- a dry run that did not finish ------------------------------------
        //
        // Start runs a dry run first only for a job that deletes or moves, and
        // builds its confirmations on what that run collected. A run that was
        // cancelled, or that rsync could not finish, has collected only part of
        // it: nothing may be confirmed from that, and no rule may be reported
        // as matching nothing on the strength of it.
        self.add_source(&gio::File::for_path(dir.join("src")));
        self.set_dest(&gio::File::for_path(dir.join("dst")));
        self.add_filter(FilterKind::Exclude, "/src/private");
        self.add_filter(FilterKind::Exclude, "*.tmp");
        self.imp().remove_source_row.set_active(true);
        self.imp().delete_row.set_active(true);
        let done = |severity: Severity, code: Option<i32>| Completion {
            severity,
            message: String::new(),
            code,
        };
        // A dry run as the window lives it: reset, then rsync's own lines
        // through the real parser and the real event handler. One rule gets a
        // hit and one does not; one file is to be deleted.
        let dry_run = |w: &ForesightWindow, lines: &str| {
            let job = w.current_job().expect("sources + dest are set");
            w.begin_preview(&job);
            let mut parser = rsync_events::StreamParser::new();
            let mut events = parser.feed(lines);
            events.extend(parser.finish());
            for ev in events {
                w.on_preview_event(ev);
            }
        };
        const SCAN: &str =
            "[sender] hiding directory src/private because of pattern /src/private\n\
                            *deleting   src/old.txt\n\
                            >f+++++++++ src/a.txt\n";
        const SCAN_FAILED: &str = "rsync: [sender] opendir \"/x/src/locked\" failed: Permission denied (13)\n\
                                   rsync error: some files/attrs were not transferred (see previous errors) (code 23) at main.c(1394) [sender=3.5.0]\n";
        let verdicts = |w: &ForesightWindow| -> String {
            let rows = w.imp().filter_rows.borrow();
            let mut all: Vec<String> = rows
                .iter()
                .map(|r| r.subtitle().map(|s| s.to_string()).unwrap_or_default())
                .collect();
            all.push(w.imp().filters_row.subtitle().to_string());
            all.join(" | ")
        };
        let state = |w: &ForesightWindow| {
            format!(
                "hits={:?} idle={:?} idle_excludes={:?} deletions={:?} rows={:?}",
                w.current_filter_hits(),
                w.idle_rules(),
                w.idle_source_excludes(),
                w.imp().deletions.borrow(),
                verdicts(w)
            )
        };

        dry_run(self, SCAN);
        let outcome = self.settle_preview(&done(Severity::Success, Some(0)), true);
        check(
            "a complete dry run in front of Start goes on to the confirmations",
            outcome == PreviewOutcome::Confirm
                && self.idle_source_excludes() == ["*.tmp"]
                && *self.imp().deletions.borrow() == ["src/old.txt"],
            format!("{outcome:?} {}", state(self)),
        );
        dry_run(self, SCAN);
        let outcome = self.settle_preview(&done(Severity::Success, Some(0)), false);
        check(
            "a complete plain dry run reports what each rule matched",
            outcome == PreviewOutcome::Report
                && self.idle_rules() == [1]
                && verdicts(self).contains("matched 1 path")
                && verdicts(self).contains("matched nothing")
                && verdicts(self).contains("1 matches nothing"),
            format!("{outcome:?} {}", state(self)),
        );

        for (severity, code, name) in [
            (Severity::Partial, Some(23), "partial"),
            (Severity::Cancelled, None, "cancelled"),
            (Severity::Error, Some(12), "failed"),
        ] {
            for start in [false, true] {
                let path = if start {
                    "in front of Start"
                } else {
                    "on its own"
                };
                dry_run(self, &format!("{SCAN}{SCAN_FAILED}"));
                let outcome = self.settle_preview(&done(severity, code), start);
                check(
                    &format!("a {name} dry run {path} reaches no confirmation and no sync"),
                    !outcome.is_complete() && outcome != PreviewOutcome::Confirm,
                    format!("{outcome:?}"),
                );
                check(
                    &format!("a {name} dry run {path} leaves no verdicts and no deletions"),
                    self.current_filter_hits().is_none()
                        && self.idle_rules().is_empty()
                        && self.idle_source_excludes().is_empty()
                        && self.imp().deletions.borrow().is_empty()
                        && !verdicts(self).contains("matched")
                        && !verdicts(self).contains("nothing")
                        && !self.is_running(),
                    state(self),
                );
            }
        }
        check(
            "rsync's own errors are kept for the refusal to show",
            self.imp().run_errors.borrow().len() == 2
                && self.imp().run_errors.borrow()[0].contains("opendir"),
            format!("{:?}", self.imp().run_errors.borrow()),
        );

        // Refused on the Start path, merely labelled on a plain Dry Run.
        dry_run(self, &format!("{SCAN}{SCAN_FAILED}"));
        let refused = self.settle_preview(&done(Severity::Partial, Some(23)), true);
        dry_run(self, &format!("{SCAN}{SCAN_FAILED}"));
        let labelled = self.settle_preview(&done(Severity::Partial, Some(23)), false);
        check(
            "a partial dry run refuses Start and labels a plain Dry Run",
            refused == PreviewOutcome::Refused && labelled == PreviewOutcome::Incomplete,
            format!("{refused:?} / {labelled:?}"),
        );
        self.present_preview(labelled, done(Severity::Partial, Some(23)));
        let banner = self.imp().result_banner.get();
        check(
            "an incomplete plain dry run says so, and not that files were not transferred",
            banner.is_revealed()
                && banner.title().starts_with("Dry run incomplete")
                && banner.title().contains("Nothing was transferred")
                && !banner.title().contains("matched nothing"),
            banner.title().to_string(),
        );
        // It is a statement about the run, not about the rules: editing them
        // must not take it down the way it does the matched-nothing notice.
        self.add_filter(FilterKind::Exclude, "while-reading");
        check(
            "editing the rules leaves the incomplete notice up",
            banner.is_revealed(),
            "the banner was taken down".into(),
        );
        self.remove_filter(2);

        // What needs no run must survive a run that told us nothing.
        self.add_filter(FilterKind::Exclude, &pasted);
        dry_run(self, SCAN_FAILED);
        self.settle_preview(&done(Severity::Partial, Some(23)), true);
        check(
            "a rule that can never match is still flagged after an incomplete run",
            self.idle_rules() == [2] && subtitle_of(self, 2).contains("can never match"),
            state(self),
        );
        self.remove_filter(2);

        // The next complete run is believed again, and only for what it saw:
        // the deletion from before the incomplete run must not come back.
        dry_run(self, SCAN);
        self.settle_preview(&done(Severity::Success, Some(0)), true);
        dry_run(
            self,
            &format!("*deleting   src/half-seen.txt\n{SCAN_FAILED}"),
        );
        self.settle_preview(&done(Severity::Partial, Some(23)), true);
        let after_incomplete = self.imp().deletions.borrow().clone();
        dry_run(
            self,
            "[sender] hiding directory src/private because of pattern /src/private\n",
        );
        let outcome = self.settle_preview(&done(Severity::Success, Some(0)), true);
        check(
            "deletions from an incomplete run do not reach a later confirmation",
            after_incomplete.is_empty() && self.imp().deletions.borrow().is_empty(),
            format!("{after_incomplete:?} then {}", state(self)),
        );
        check(
            "a complete run after an incomplete one restores the verdicts",
            outcome == PreviewOutcome::Confirm
                && self.idle_source_excludes() == ["*.tmp"]
                && self.idle_rules() == [1]
                && subtitle_of(self, 0).contains("matched 1 path"),
            format!("{outcome:?} {}", state(self)),
        );
        self.clear_job();

        // -- what rsync reports without its prefix ----------------------------
        //
        // With Mirror deletions on and part of the source unreadable, rsync
        // stops deleting and says so in a line that does not start with
        // `rsync:`. It is the one line that says the mirror did not mirror,
        // so it has to be among the errors the refusal shows. The lines are
        // tests/fixtures/dry_run_io_error.txt with the temp path shortened.
        const MIRROR_UNREADABLE: &str = "rsync: [sender] opendir \"/x/io/src/locked\" failed: Permission denied (13)\n\
             *deleting   src/top-stale.txt\n\
             IO error encountered -- skipping file deletion\n\
             cd+++++++++ src/locked/\n\
             >f+++++++++ src/ok/a.txt\n\
             rsync error: some files/attrs were not transferred (see previous errors) (code 23) at main.c(1394) [sender=3.5.0-g483b5efc]\n";
        const SKIPPED: &str = "IO error encountered -- skipping file deletion";
        self.add_source(&gio::File::for_path(dir.join("src")));
        self.set_dest(&gio::File::for_path(dir.join("dst")));
        self.imp().delete_row.set_active(true);

        dry_run(self, MIRROR_UNREADABLE);
        let outcome = self.settle_preview(&done(Severity::Partial, Some(23)), true);
        let collected = self.imp().run_errors.borrow().clone();
        check(
            "a Mirror dry run that met an unreadable folder is refused",
            outcome == PreviewOutcome::Refused && self.imp().deletions.borrow().is_empty(),
            format!("{outcome:?} {}", state(self)),
        );
        check(
            "the skipped-deletion line is collected between rsync's own errors",
            collected.len() == 3
                && collected[0].contains("opendir")
                && collected[1] == SKIPPED
                && collected[2].starts_with("rsync error:"),
            format!("{collected:?}"),
        );
        let body = refused_start_body(
            self.imp().delete_row.is_active(),
            self.imp().remove_source_row.is_active(),
            Some(23),
            &collected,
        );
        check(
            "the refusal says that deletions were skipped",
            body.contains(SKIPPED)
                && body.contains("Mirror deletions")
                && body.contains("opendir")
                && body.contains("rsync exit code 23"),
            body.clone(),
        );
        check(
            "the refusal shows no itemized line as an error",
            !body.contains("*deleting") && !body.contains("src/ok/a.txt"),
            body,
        );

        // The same run as a plain Dry Run: the banner counts it.
        dry_run(self, MIRROR_UNREADABLE);
        let outcome = self.settle_preview(&done(Severity::Partial, Some(23)), false);
        self.present_preview(outcome, done(Severity::Partial, Some(23)));
        let banner = self.imp().result_banner.get();
        check(
            "an incomplete plain Mirror dry run counts the skipped-deletion line",
            outcome == PreviewOutcome::Incomplete
                && banner.title().contains("rsync reported 3 errors"),
            format!("{outcome:?} {}", banner.title()),
        );

        // A healthy Mirror dry run collects nothing, whatever rsync remarks on.
        dry_run(
            self,
            "*deleting   src/gone/other\n\
             cannot delete non-empty directory: src/gone\n\
             skipping non-regular file \"src/link\"\n\
             >f+++++++++ src/a.txt\n",
        );
        let outcome = self.settle_preview(&done(Severity::Success, Some(0)), true);
        check(
            "rsync's routine notices are not collected as errors",
            outcome == PreviewOutcome::Confirm
                && self.imp().run_errors.borrow().is_empty()
                && *self.imp().deletions.borrow() == ["src/gone/other"],
            format!(
                "{outcome:?} {:?} {}",
                self.imp().run_errors.borrow(),
                state(self)
            ),
        );
        self.clear_job();

        // Regression guard: a name a KeyFile group could never hold used to be
        // dropped on save while the UI reported success.
        self.set_filters(&[FilterRule::exclude("*.tmp")]);
        self.upsert_preset("Photos [raw]".into());
        let on_disk_names = || -> Vec<String> {
            crate::profiles::load()
                .unwrap_or_default()
                .iter()
                .map(|p| p.name.clone())
                .collect()
        };
        let on_disk = on_disk_names();
        check(
            "a preset named \"Photos [raw]\" reaches disk",
            on_disk.contains(&"Photos [raw]".to_string()),
            format!("in memory only; on disk: {on_disk:?}"),
        );

        // The other half of the same promise: a save that fails must not be
        // shown as one that worked. A directory sitting on the file's name
        // stops the write for any user, root included, and is undone below so
        // the config dir is left as it was found.
        let names = |w: &ForesightWindow| -> Vec<String> {
            w.imp()
                .profiles
                .borrow()
                .iter()
                .map(|p| p.name.clone())
                .collect()
        };
        let combo_len = |w: &ForesightWindow| w.imp().preset_combo.model().map(|m| m.n_items());
        let ini = glib::user_config_dir()
            .join("foresight")
            .join("profiles.ini");
        let aside = ini.with_extension("ini.selftest");
        let blocked = std::fs::rename(&ini, &aside).is_ok() && std::fs::create_dir(&ini).is_ok();
        let (names_before, combo_before) = (names(self), combo_len(self));

        let saved = self.try_upsert_preset("Never written");
        check(
            "a save that fails is reported as a failure",
            blocked && saved.is_err(),
            format!("blocked: {blocked}, result: {saved:?}"),
        );
        check(
            "a save that fails adds nothing to the preset list",
            names(self) == names_before && combo_len(self) == combo_before,
            format!("{:?}", names(self)),
        );

        self.imp().preset_combo.set_selected(1);
        let deleted = self.try_delete_selected_preset();
        check(
            "a delete that fails keeps the preset, still selected",
            blocked
                && deleted.is_err()
                && names(self) == names_before
                && self.imp().preset_combo.selected() == 1,
            format!("result: {deleted:?}, list: {:?}", names(self)),
        );

        let restored = std::fs::remove_dir(&ini).is_ok() && std::fs::rename(&aside, &ini).is_ok();
        let saved = self.try_upsert_preset("Written after all");
        check(
            "saving works again once the file can be written",
            restored && saved.is_ok() && on_disk_names().contains(&"Written after all".to_string()),
            format!("restored: {restored}, result: {saved:?}"),
        );

        // The read side of the same file. A presets file that is there but
        // cannot be read used to load as "no presets", and the next save then
        // replaced it. The real file is kept in memory meanwhile and put back
        // at the end, along with a listing of the directory to prove that
        // nothing staged here outlives these checks.
        let listing = || -> Vec<String> {
            let mut names: Vec<String> = ini
                .parent()
                .and_then(|dir| std::fs::read_dir(dir).ok())
                .into_iter()
                .flatten()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };
        let (real_bytes, real_names) = (std::fs::read(&ini).unwrap_or_default(), names(self));
        let dir_before = listing();
        let garbage: &[u8] = b"[preset_0]\nname=Years of work\nnot a key file \xff\n";
        let subtitle = |w: &ForesightWindow| {
            w.imp()
                .preset_combo
                .subtitle()
                .unwrap_or_default()
                .to_string()
        };
        let mut moved: Vec<PathBuf> = Vec::new();

        let staged = std::fs::write(&ini, garbage).is_ok();
        let error = self.load_presets();
        check(
            "a presets file that cannot be read is an error, not an empty list",
            staged
                && error.as_ref().is_some_and(|e| e.path() == ini)
                && self.imp().presets_unread.borrow().is_some()
                && !subtitle(self).is_empty(),
            format!("staged: {staged}, error: {error:?}"),
        );
        let notice = error.as_ref().map(unreadable_presets_notice);
        check(
            "the notice gives the path and says the file is unchanged",
            notice.as_ref().is_some_and(|n| {
                n.contains(&ini.display().to_string()) && n.contains("has not been changed")
            }) && std::fs::read(&ini).is_ok_and(|b| b == garbage),
            format!("{notice:?}"),
        );

        let saved = self.try_upsert_preset("After the ruin");
        let aside = saved.as_ref().ok().and_then(|s| s.moved_aside.clone());
        moved.extend(aside.clone());
        check(
            "a save moves the unreadable file aside instead of replacing it",
            aside
                .as_ref()
                .is_some_and(|to| std::fs::read(to).is_ok_and(|b| b == garbage))
                && on_disk_names() == ["After the ruin"],
            format!("result: {saved:?}, on disk: {:?}", on_disk_names()),
        );
        check(
            "after that save the list is the file again, and says so",
            self.imp().presets_unread.borrow().is_none()
                && subtitle(self).is_empty()
                && names(self) == ["After the ruin"],
            format!("{:?}, subtitle: {:?}", names(self), subtitle(self)),
        );

        // A file that goes bad while the app is running, met by a delete.
        let staged = std::fs::write(&ini, garbage).is_ok();
        self.imp().preset_combo.set_selected(1);
        let deleted = self.try_delete_selected_preset();
        let aside = match &deleted {
            Ok(Some((_, saved))) => saved.moved_aside.clone(),
            _ => None,
        };
        moved.extend(aside.clone());
        check(
            "a delete protects an unreadable file the same way",
            staged
                && aside
                    .as_ref()
                    .is_some_and(|to| std::fs::read(to).is_ok_and(|b| b == garbage))
                && moved.len() == 2
                && moved[0] != moved[1]
                && std::fs::read(&moved[0]).is_ok_and(|b| b == garbage)
                && on_disk_names().is_empty(),
            format!("result: {deleted:?}, moved: {moved:?}"),
        );

        // Unreadable at startup, repaired by hand before the first save: the
        // save must build on the repaired file, not write over it.
        let staged = std::fs::write(&ini, garbage).is_ok()
            && self.load_presets().is_some()
            && std::fs::write(&ini, &real_bytes).is_ok();
        let saved = self.try_upsert_preset("After the repair");
        let mut expected = real_names.clone();
        expected.push("After the repair".to_string());
        check(
            "a file repaired since startup is built on, not written over",
            staged
                && saved.as_ref().is_ok_and(|s| s.moved_aside.is_none())
                && on_disk_names() == expected
                && names(self) == expected,
            format!("result: {saved:?}, on disk: {:?}", on_disk_names()),
        );

        let removed = moved.iter().all(|to| std::fs::remove_file(to).is_ok());
        let restored = std::fs::write(&ini, &real_bytes).is_ok() && self.load_presets().is_none();
        check(
            "the config dir is left as it was found",
            removed
                && restored
                && listing() == dir_before
                && names(self) == real_names
                && std::fs::read(&ini).is_ok_and(|b| b == real_bytes),
            format!(
                "removed: {removed}, restored: {restored}, dir: {:?}",
                listing()
            ),
        );

        // -- closing with a run live -----------------------------------------
        //
        // Closing the window used to drop the UI and leave rsync writing, with
        // nothing left to stop it. These hold real rsync processes, throttled
        // so that they are mid-transfer whenever anything is done to them, and
        // judge "stopped" by the process table rather than by the window's own
        // account. They run in windows of their own: a close that is allowed
        // really closes, and this one has to stay for the application to.
        use crate::job::procs;
        use std::time::{Duration, Instant};
        self.clear_job();
        check(
            "a close request with nothing running is allowed",
            self.close_step() == CloseStep::Close
                && self.on_close_request() == glib::Propagation::Proceed,
            format!("{:?}", self.close_step()),
        );

        let pump = |until: &dyn Fn() -> bool, within: Duration| {
            let ctx = glib::MainContext::default();
            let deadline = Instant::now() + within;
            while !until() && Instant::now() < deadline {
                if !ctx.iteration(false) {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        };
        let app = self.application().expect("the window has an application");
        let is_open = |w: &ForesightWindow| app.windows().iter().any(|o| o == w);
        // A window holding a live, throttled rsync of the given kind, wired to
        // the window's own completion handlers. Returns the process tree.
        let open_with_run = |kind: RunKind, tag: &str| -> Option<(ForesightWindow, Vec<u32>)> {
            let base = dir.join(format!("close-{tag}"));
            let _ = std::fs::create_dir_all(base.join("src"));
            let _ = std::fs::create_dir_all(base.join("dst"));
            let _ = std::fs::write(base.join("src/big.bin"), vec![0u8; 4 * 1024 * 1024]);
            let mut src = base.join("src").into_os_string();
            src.push("/");
            let argv = vec![
                "-a".into(),
                "--bwlimit=100".into(),
                "--info=progress2".into(),
                src,
                base.join("dst").into_os_string(),
            ];
            let win: ForesightWindow = glib::Object::builder()
                .property("application", &app)
                .build();
            win.present();
            let on_event = glib::clone!(
                #[weak]
                win,
                move |ev: Event| win.on_sync_event(ev)
            );
            let on_done = glib::clone!(
                #[weak]
                win,
                move |c: Completion| match kind {
                    RunKind::DryRun => win.on_preview_done(c, false),
                    RunKind::Transfer { .. } => win.on_sync_done(c),
                }
            );
            let runner = spawn_rsync(argv, on_event, on_done).ok()?.with_kind(kind);
            let pid = runner.pid()?;
            *win.imp().runner.borrow_mut() = Some(runner);
            win.refresh_action_sensitivity();
            // Until rsync has forked its other half it is not transferring.
            pump(&|| procs::tree(pid).len() > 1, Duration::from_secs(5));
            Some((win, procs::tree(pid)))
        };
        let plain = RunKind::Transfer {
            moves: false,
            deletes: false,
        };

        match open_with_run(plain, "transfer") {
            Some((win, tree)) => {
                win.close();
                check(
                    "a close request with a transfer live is refused",
                    win.on_close_request() == glib::Propagation::Stop
                        && is_open(&win)
                        && win.is_visible()
                        && procs::alive(tree[0]),
                    format!(
                        "open={} rsync alive={}",
                        is_open(&win),
                        procs::alive(tree[0])
                    ),
                );
                check(
                    "closing with a transfer live asks, once",
                    win.imp().close_dialog.upgrade().is_some()
                        && win.close_step() == CloseStep::Ask(plain),
                    format!("{:?}", win.close_step()),
                );
                win.on_close_response("keep");
                pump(&|| !is_open(&win), Duration::from_millis(300));
                check(
                    "keeping the transfer leaves the window and rsync as they were",
                    is_open(&win)
                        && win.is_running()
                        && procs::survivors(&tree, Duration::ZERO) == tree,
                    format!("open={} running={}", is_open(&win), win.is_running()),
                );
                win.on_close_response("stop");
                check(
                    "a window stopping its transfer waits for rsync to exit",
                    is_open(&win) && win.close_step() == CloseStep::Wait,
                    format!("open={} step={:?}", is_open(&win), win.close_step()),
                );
                let asked = Instant::now();
                pump(&|| !is_open(&win), Duration::from_secs(15));
                let left = procs::survivors(&tree, Duration::from_secs(3));
                check(
                    "Stop and Close leaves no rsync and then closes the window",
                    !is_open(&win) && !win.is_running() && left.is_empty(),
                    format!(
                        "open={} running={} still alive={left:?} after {:?}",
                        is_open(&win),
                        win.is_running(),
                        asked.elapsed()
                    ),
                );
                win.stop_run_for_shutdown();
                win.destroy();
            }
            None => check(
                "rsync spawns for the close checks",
                false,
                "transfer".into(),
            ),
        }

        match open_with_run(RunKind::DryRun, "dry-run") {
            Some((win, tree)) => {
                win.close();
                check(
                    "closing during a dry run asks nothing",
                    win.imp().close_dialog.upgrade().is_none(),
                    "a dialog was presented".into(),
                );
                pump(&|| !is_open(&win), Duration::from_secs(15));
                let left = procs::survivors(&tree, Duration::from_secs(3));
                check(
                    "closing during a dry run stops rsync and then closes the window",
                    !is_open(&win) && !win.is_running() && left.is_empty(),
                    format!("open={} still alive={left:?}", is_open(&win)),
                );
                win.stop_run_for_shutdown();
                win.destroy();
            }
            None => check("rsync spawns for the close checks", false, "dry run".into()),
        }

        // Quitting the application passes no close-request; this is what its
        // shutdown runs on every window.
        match open_with_run(plain, "quit") {
            Some((win, tree)) => {
                win.stop_run_for_shutdown();
                let alive_on_return = procs::alive(tree[0]);
                let left = procs::survivors(&tree, Duration::from_secs(3));
                check(
                    "application shutdown does not return with rsync running",
                    !alive_on_return && !win.is_running() && left.is_empty(),
                    format!("alive on return={alive_on_return} still alive={left:?}"),
                );
                win.destroy();
            }
            None => check("rsync spawns for the close checks", false, "quit".into()),
        }
        for tag in ["transfer", "dry-run", "quit"] {
            let _ = std::fs::remove_dir_all(dir.join(format!("close-{tag}")));
        }

        let moving = close_question_body(RunKind::Transfer {
            moves: true,
            deletes: false,
        });
        check(
            "the question mentions the source only when the run is a move",
            moving.contains("gone from the source")
                && !close_question_body(plain).contains("source")
                && !moving.contains("deleted"),
            moving,
        );

        // -- keyboard shortcuts ----------------------------------------------
        //
        // A key is an action plus an accelerator, and an action is only as safe
        // as its `enabled`. What is checked: the table, the application and the
        // window agree on what exists; each action is enabled exactly while
        // its button is; and Start by key stops where Start by click stops.
        // What cannot be checked here is a key actually being pressed.
        use crate::shortcuts::{self, SHORTCUTS};
        self.clear_job();
        let app = self.application().expect("the window has an application");
        let pump = |ms: u64| {
            let ctx = glib::MainContext::default();
            let elapsed = std::rc::Rc::new(std::cell::Cell::new(false));
            glib::timeout_add_local_once(std::time::Duration::from_millis(ms), {
                let elapsed = elapsed.clone();
                move || elapsed.set(true)
            });
            while !elapsed.get() {
                ctx.iteration(true);
            }
        };
        let enabled =
            |w: &ForesightWindow, name: &str| w.lookup_action(name).is_some_and(|a| a.is_enabled());
        let activate = |w: &ForesightWindow, name: &str| {
            gio::prelude::ActionGroupExt::activate_action(w, name, None);
        };

        let missing: Vec<&str> = SHORTCUTS
            .iter()
            .filter_map(|s| s.action)
            .filter(|detailed| {
                let name = detailed.split("::").next().unwrap_or(detailed);
                match name.split_once('.') {
                    Some(("win", n)) => self.lookup_action(n).is_none(),
                    Some(("app", n)) => app.lookup_action(n).is_none(),
                    _ => true,
                }
            })
            .collect();
        check(
            "every action in the shortcut table exists",
            missing.is_empty(),
            format!("no such action: {missing:?}"),
        );
        let unparsed: Vec<&str> = SHORTCUTS
            .iter()
            .flat_map(|s| s.accels.iter().copied())
            .filter(|a| gtk::accelerator_parse(*a).is_none())
            .collect();
        check(
            "every accelerator in the table is one GTK can parse",
            unparsed.is_empty(),
            format!("{unparsed:?}"),
        );
        // Compared as parsed keys: GTK hands accelerators back in its own
        // spelling, which need not be the table's.
        let keys = |accels: Vec<String>| -> Vec<_> {
            accels
                .iter()
                .filter_map(|a| gtk::accelerator_parse(a.as_str()))
                .collect()
        };
        let unregistered: Vec<&str> = SHORTCUTS
            .iter()
            .filter(|s| {
                s.action.is_some_and(|action| {
                    let got = app.accels_for_action(action);
                    keys(got.iter().map(|a| a.to_string()).collect())
                        != keys(s.accels.iter().map(|a| a.to_string()).collect())
                })
            })
            .map(|s| s.title)
            .collect();
        check(
            "the application holds exactly the table's keys for each action",
            unregistered.is_empty(),
            format!("{unregistered:?}"),
        );
        let unlisted: Vec<String> = app
            .list_action_descriptions()
            .iter()
            .map(|a| a.to_string())
            .filter(|a| !SHORTCUTS.iter().any(|s| s.action == Some(a.as_str())))
            .collect();
        check(
            "no accelerator is registered that the table does not list",
            unlisted.is_empty(),
            format!("{unlisted:?}"),
        );
        check(
            "F10 is GTK's: the menu button is the primary one",
            self.imp().menu_button.is_primary(),
            "the table lists F10 but nothing answers it".into(),
        );
        check(
            "a button's tooltip names its key",
            shortcuts::label("win.dry-run").is_some_and(|key| {
                self.imp()
                    .preview_button
                    .tooltip_text()
                    .is_some_and(|t| t.ends_with(&format!("({key})")))
            }),
            format!("{:?}", self.imp().preview_button.tooltip_text()),
        );

        // Each action against the button it mirrors, through the states the
        // window passes through. Returns the pairs that disagree.
        let disagree = |w: &ForesightWindow| -> Vec<&'static str> {
            let imp = w.imp();
            [
                ("dry-run", imp.preview_button.is_sensitive()),
                ("start-sync", imp.start_button.is_sensitive()),
                ("cancel-run", imp.cancel_button.is_sensitive()),
                ("add-folder", imp.add_folder_button.is_sensitive()),
                ("add-file", imp.add_file_button.is_sensitive()),
                ("remote-source", imp.add_remote_source_button.is_sensitive()),
                ("remote-destination", imp.remote_dest_button.is_sensitive()),
            ]
            .into_iter()
            .filter(|(name, sensitive)| enabled(w, name) != *sensitive)
            .map(|(name, _)| name)
            .collect()
        };
        check(
            "nothing selected: Dry Run, Start and Cancel are off by key too",
            disagree(self).is_empty()
                && !enabled(self, "dry-run")
                && !enabled(self, "start-sync")
                && !enabled(self, "cancel-run")
                && enabled(self, "add-folder"),
            format!("disagree: {:?}", disagree(self)),
        );
        activate(self, "start-sync");
        check(
            "a disabled Start does nothing when activated",
            !self.is_running()
                && self.imp().main_stack.visible_child_name().as_deref() == Some("configure"),
            "something started".into(),
        );

        self.add_source(&gio::File::for_path(dir.join("src")));
        self.set_dest(&gio::File::for_path(dir.join("dst")));
        check(
            "both ends selected: Dry Run and Start come on by key",
            disagree(self).is_empty() && enabled(self, "dry-run") && enabled(self, "start-sync"),
            format!("disagree: {:?}", disagree(self)),
        );
        self.set_remote(RemoteSide::Dest, ep("nas.local", "/srv/backup"));
        check(
            "a remote destination closes the remote-source key with its button",
            disagree(self).is_empty() && !enabled(self, "remote-source"),
            format!("disagree: {:?}", disagree(self)),
        );
        self.set_dest(&gio::File::for_path(dir.join("dst")));

        match spawn_rsync(vec!["--version".into()], |_| {}, |_| {}) {
            Ok(runner) => {
                *self.imp().runner.borrow_mut() = Some(runner);
                self.refresh_action_sensitivity();
                check(
                    "a live run leaves Cancel as the only job key",
                    disagree(self).is_empty()
                        && enabled(self, "cancel-run")
                        && !enabled(self, "dry-run")
                        && !enabled(self, "start-sync")
                        && !enabled(self, "new-job")
                        && !enabled(self, "add-folder")
                        && !enabled(self, "choose-destination"),
                    format!("disagree: {:?}", disagree(self)),
                );
                *self.imp().runner.borrow_mut() = None;
                self.refresh_action_sensitivity();
                check(
                    "the keys come back when the run ends",
                    disagree(self).is_empty()
                        && !enabled(self, "cancel-run")
                        && enabled(self, "start-sync")
                        && enabled(self, "new-job")
                        && enabled(self, "choose-destination"),
                    format!("disagree: {:?}", disagree(self)),
                );
            }
            Err(e) => check("rsync spawns for the shortcut checks", false, e.to_string()),
        }

        self.imp().main_stack.set_visible_child_name("configure");
        gio::prelude::ActionGroupExt::activate_action(
            self,
            "show-page",
            Some(&"transfer".to_variant()),
        );
        check(
            "the page action switches pages",
            self.imp().main_stack.visible_child_name().as_deref() == Some("transfer"),
            format!("{:?}", self.imp().main_stack.visible_child_name()),
        );

        self.imp().advanced_row.set_expanded(false);
        self.imp().filters_row.set_expanded(false);
        activate(self, "add-filter-rule");
        pump(600);
        check(
            "the filter-rule key opens the way to the entry and focuses it",
            self.imp().main_stack.visible_child_name().as_deref() == Some("configure")
                && self.imp().advanced_row.is_expanded()
                && self.imp().filters_row.is_expanded()
                && gtk::prelude::RootExt::focus(self)
                    .is_some_and(|w| w.is_ancestor(&*self.imp().filter_entry)),
            format!(
                "advanced={} filters={} focus={:?}",
                self.imp().advanced_row.is_expanded(),
                self.imp().filters_row.is_expanded(),
                gtk::prelude::RootExt::focus(self).map(|w| w.type_().name())
            ),
        );

        // The menu item this all started with.
        let fallback = shortcuts::build_window();
        check(
            "the GtkShortcutsWindow fallback builds from the table",
            fallback.is_ok(),
            format!("{:?}", fallback.as_ref().err()),
        );
        if let Ok(window) = fallback {
            window.destroy();
        }
        let toplevels = || gtk::Window::list_toplevels().len();
        let before = toplevels();
        activate(self, "show-help-overlay");
        pump(200);
        let dialog = self.visible_dialog();
        check(
            "Keyboard Shortcuts opens something",
            dialog.is_some() || toplevels() > before,
            "win.show-help-overlay still does nothing".into(),
        );
        if let Some(dialog) = dialog {
            check(
                "an open dialog takes the keys away from the window",
                !enabled(self, "start-sync")
                    && !enabled(self, "dry-run")
                    && !enabled(self, "show-help-overlay"),
                "an action is still live behind the dialog".into(),
            );
            dialog.force_close();
            pump(600);
            check(
                "closing the dialog hands the keys back",
                self.visible_dialog().is_none()
                    && enabled(self, "start-sync")
                    && disagree(self).is_empty(),
                format!("disagree: {:?}", disagree(self)),
            );
        } else {
            for w in gtk::Window::list_toplevels() {
                if w.type_().name() == "GtkShortcutsWindow" {
                    w.downcast::<gtk::Window>().unwrap().destroy();
                }
            }
        }

        // Start by key, with Mirror deletions on and something to delete. It
        // has to end at the confirmation with the file still there — the same
        // place the button ends — and the confirmation has to hold the keys.
        let stale = dir.join("dst/src/stale.txt");
        let _ = std::fs::create_dir_all(dir.join("dst/src"));
        let _ = std::fs::write(&stale, "only in the destination");
        self.imp().delete_row.set_active(true);
        activate(self, "start-sync");
        check(
            "Start by key with Mirror deletions on begins with a dry run",
            self.is_running()
                && self.imp().main_stack.visible_child_name().as_deref() == Some("preview"),
            format!(
                "running={} page={:?}",
                self.is_running(),
                self.imp().main_stack.visible_child_name()
            ),
        );
        for _ in 0..100 {
            if !self.is_running() {
                break;
            }
            pump(100);
        }
        pump(200);
        check(
            "…and stops at the confirmation, nothing deleted",
            !self.is_running()
                && self.visible_dialog().is_some()
                && stale.exists()
                && self.imp().main_stack.visible_child_name().as_deref() == Some("preview"),
            format!(
                "running={} dialog={} file={} deletions={:?}",
                self.is_running(),
                self.visible_dialog().is_some(),
                stale.exists(),
                self.imp().deletions.borrow()
            ),
        );
        activate(self, "start-sync");
        check(
            "Start cannot be activated again from behind the confirmation",
            !enabled(self, "start-sync") && !self.is_running(),
            "a second run started under the dialog".into(),
        );
        if let Some(dialog) = self.visible_dialog() {
            // Closing is the "cancel" response.
            dialog.close();
            pump(600);
        }
        check(
            "dismissing the confirmation leaves the destination untouched",
            !self.is_running() && stale.exists() && self.visible_dialog().is_none(),
            format!("running={} file={}", self.is_running(), stale.exists()),
        );
        let _ = std::fs::remove_file(&stale);
        self.clear_job();

        // -- a partial transfer says what went wrong --------------------------
        //
        // Exit 23/24/25 stays a banner and nothing more is put in front of
        // anyone; what is checked is that the banner now leads to what rsync
        // reported, that it says what the run did to deletions and to a move
        // only when that is true of the run, and that the banner's other
        // users are unchanged. The lines are rsync 3.5.0's own, from the two
        // runs repeated for real at the end of this block, paths shortened.
        {
            use std::os::unix::fs::PermissionsExt;
            const OPENDIR: &str =
                "rsync: [sender] opendir \"/x/src/locked\" failed: Permission denied (13)";
            const SEND_FILES: &str = "rsync: [sender] send_files failed to open \"/x/src/secret.txt\": Permission denied (13)";
            const EXIT_23: &str = "rsync error: some files/attrs were not transferred (see previous errors) (code 23) at main.c(1394) [sender=3.5.0-g483b5efc]";
            const DELETION: &str = "Mirror deletions stopped part-way";
            const MOVED: &str = "Files that transferred were removed from the source";
            let mirror_lines = format!(
                "{OPENDIR}\n*deleting   src/top-stale.txt\n{DELETION_SKIPPED}\n{SEND_FILES}\n\
                 \r              2  50%    1.95kB/s    0:00:00 (xfr#1, to-chk=1/5)\r\
                 cd+++++++++ src/locked/\n>f+++++++++ src/ok/a.txt\n{EXIT_23}\n"
            );
            let move_lines = format!("{SEND_FILES}\n>f+++++++++ src/ok/a.txt\n{EXIT_23}\n");
            const VANISHED: &str = ">f+++++++++ src/a.txt\n\
                 file has vanished: \"/x/src/z_late.txt\"\n\
                 rsync warning: some files vanished before they could be transferred (code 24) at main.c(1394) [sender=3.5.0-g483b5efc]\n";

            let imp = self.imp();
            let banner = imp.result_banner.get();
            let mirror = RunKind::Transfer {
                moves: false,
                deletes: true,
            };
            let moving = RunKind::Transfer {
                moves: true,
                deletes: false,
            };
            // A transfer as the window lives it: reset as `run_sync` resets,
            // rsync's lines through the real parser and the real handler, a
            // run of the given kind in the window's hands, and then its end
            // through the real completion handler. The run held is a real one
            // (`rsync --version`), so its kind is read from a `Runner`.
            let transfer = |w: &ForesightWindow, lines: &str, kind: RunKind, code: i32| -> bool {
                w.hide_banner();
                w.imp().run_errors.borrow_mut().clear();
                let mut parser = rsync_events::StreamParser::new();
                let mut events = parser.feed(lines);
                events.extend(parser.finish());
                for ev in events {
                    w.on_sync_event(ev);
                }
                let Ok(runner) = spawn_rsync(vec!["--version".into()], |_| {}, |_| {}) else {
                    return false;
                };
                *w.imp().runner.borrow_mut() = Some(runner.with_kind(kind));
                let (severity, message) = rsync_events::classify_exit(code);
                w.on_sync_done(Completion {
                    severity,
                    message,
                    code: Some(code),
                });
                true
            };
            let report_of = |w: &ForesightWindow| match w.imp().banner_button.borrow().clone() {
                BannerButton::Problems(report) => Some(report),
                _ => None,
            };
            let press = |w: &ForesightWindow| {
                w.imp()
                    .result_banner
                    .emit_by_name::<()>("button-clicked", &[]);
                pump(200);
            };
            let shown = |w: &ForesightWindow| -> Option<(String, String)> {
                w.visible_dialog()
                    .and_then(|d| d.downcast::<adw::AlertDialog>().ok())
                    .map(|d| {
                        (
                            d.heading().map(String::from).unwrap_or_default(),
                            d.body().to_string(),
                        )
                    })
            };
            let dismiss = |w: &ForesightWindow| {
                if let Some(dialog) = w.visible_dialog() {
                    dialog.force_close();
                    pump(600);
                }
            };
            let banner_state = |w: &ForesightWindow| {
                format!(
                    "revealed={} title={:?} label={:?} button={:?}",
                    w.imp().result_banner.is_revealed(),
                    w.imp().result_banner.title(),
                    w.imp().result_banner.button_label(),
                    w.imp().banner_button.borrow()
                )
            };

            self.add_source(&gio::File::for_path(dir.join("src")));
            self.set_dest(&gio::File::for_path(dir.join("dst")));

            let ran = transfer(self, &mirror_lines, mirror, 23);
            let report = report_of(self);
            check(
                "a partial transfer is a banner that counts what rsync reported",
                ran && !self.is_running()
                    && banner.is_revealed()
                    && banner.title()
                        == "Completed, but some files could not be transferred. \
                            3 problems reported.",
                banner_state(self),
            );
            check(
                "a partial transfer puts no dialog in front of anyone",
                self.visible_dialog().is_none(),
                format!("{:?}", shown(self)),
            );
            check(
                "after a partial transfer the banner's button leads to the problems",
                banner.button_label().as_deref() == Some("Show Problems") && report.is_some(),
                banner_state(self),
            );
            let collected = imp.run_errors.borrow().clone();
            check(
                "what would be shown is what rsync reported, without its closing summary",
                collected == [OPENDIR, DELETION_SKIPPED, SEND_FILES, EXIT_23]
                    && report.as_ref().is_some_and(|r| {
                        r.problems == [OPENDIR, DELETION_SKIPPED, SEND_FILES]
                            && r.code == Some(23)
                            && r.kind == Some(mirror)
                    }),
                format!("{collected:?} / {report:?}"),
            );
            let body = report.as_ref().map(PartialReport::body).unwrap_or_default();
            check(
                "a Mirror run whose deletions were skipped says deletion stopped part-way",
                body.contains(DELETION)
                    && !body.contains(MOVED)
                    && body.contains("rsync exit code 23.")
                    && body.contains(OPENDIR)
                    && body.contains(DELETION_SKIPPED),
                body.clone(),
            );
            check(
                "the report lists no itemized line and does not claim which files went",
                !body.contains("*deleting")
                    && !body.contains("top-stale")
                    && !body.contains("src/ok/a.txt"),
                body.clone(),
            );
            press(self);
            let dialog = shown(self);
            check(
                "pressing the button opens the report, headed as a sync that finished",
                dialog
                    .as_ref()
                    .is_some_and(|(heading, text)| heading == PARTIAL_HEADING && *text == body)
                    && PARTIAL_HEADING != "Sync failed",
                format!("{dialog:?}"),
            );
            dismiss(self);
            check(
                "closing the report leaves the banner and the job as they were",
                banner.is_revealed()
                    && report_of(self) == report
                    && imp.sources.borrow().len() == 1
                    && imp.dest.borrow().is_some(),
                banner_state(self),
            );
            // The report is of the run that ended, not of whatever is in
            // `run_errors` when the button is pressed.
            imp.run_errors.borrow_mut().push("rsync: later".into());
            press(self);
            let dialog = shown(self);
            check(
                "the report is the finished run's, not what was collected since",
                dialog
                    .as_ref()
                    .is_some_and(|(_, text)| *text == body && !text.contains("later")),
                format!("{dialog:?}"),
            );
            dismiss(self);

            let ran = transfer(self, &move_lines, moving, 23);
            let body = report_of(self).map(|r| r.body()).unwrap_or_default();
            check(
                "a partial move says what left the source, and nothing about deletions",
                ran && banner.title().ends_with("1 problem reported.")
                    && body.contains(MOVED)
                    && body.contains("are still in the source")
                    && !body.contains(DELETION)
                    && body.contains(SEND_FILES),
                format!("{} / {body}", banner_state(self)),
            );
            let ran = transfer(self, &move_lines, mirror, 23);
            let body = report_of(self).map(|r| r.body()).unwrap_or_default();
            check(
                "a Mirror run that was not told deletion stopped says nothing of deletions",
                ran && !body.contains(DELETION) && !body.contains(MOVED) && !body.is_empty(),
                body,
            );
            let plain = RunKind::Transfer {
                moves: false,
                deletes: false,
            };
            let ran = transfer(self, &mirror_lines, plain, 23);
            let body = report_of(self).map(|r| r.body()).unwrap_or_default();
            check(
                "the skipped-deletion line alone, in a run that deletes nothing, adds no sentence",
                ran && !body.contains(DELETION) && body.contains(DELETION_SKIPPED),
                body,
            );

            // Exit 24: what vanished is a warning to rsync and is not
            // collected, so there is nothing to open.
            let ran = transfer(self, VANISHED, moving, 24);
            check(
                "a partial transfer with nothing collected offers no list to open",
                ran && imp.run_errors.borrow().is_empty()
                    && banner.is_revealed()
                    && *imp.banner_button.borrow() == BannerButton::NewJob
                    && banner.button_label().as_deref() == Some("New Job"),
                format!("{:?} {}", imp.run_errors.borrow(), banner_state(self)),
            );
            check(
                "…and says what the exit code means instead",
                banner.title()
                    == "Completed, but some source files vanished mid-sync. No specific \
                        errors were collected; rsync's output is in the activity log \
                        (exit code 24).",
                banner_state(self),
            );
            press(self);
            check(
                "…and its button is New Job, as it was",
                self.visible_dialog().is_none()
                    && !banner.is_revealed()
                    && imp.sources.borrow().is_empty()
                    && imp.dest.borrow().is_none(),
                format!("{} dialog={:?}", banner_state(self), shown(self)),
            );
            dismiss(self);

            // What follows a partial transfer finds the banner as it always
            // was: a successful run, and New Job.
            self.add_source(&gio::File::for_path(dir.join("src")));
            self.set_dest(&gio::File::for_path(dir.join("dst")));
            let ran = transfer(self, &mirror_lines, mirror, 23);
            let had_report = report_of(self).is_some();
            let ran = ran && transfer(self, ">f+++++++++ src/a.txt\n", mirror, 0);
            check(
                "a successful run after a partial one takes the banner and its report down",
                ran && had_report
                    && !banner.is_revealed()
                    && *imp.banner_button.borrow() == BannerButton::None
                    && imp.run_errors.borrow().is_empty(),
                banner_state(self),
            );
            let ran = transfer(self, &mirror_lines, mirror, 23);
            let had_report = report_of(self).is_some();
            self.clear_job();
            check(
                "New Job after a partial transfer takes the banner and its report down",
                ran && had_report
                    && !banner.is_revealed()
                    && *imp.banner_button.borrow() == BannerButton::None
                    && imp.run_errors.borrow().is_empty(),
                banner_state(self),
            );

            press(self);
            check(
                "…and a press on a banner that has gone opens nothing and clears nothing",
                self.visible_dialog().is_none() && !banner.is_revealed(),
                format!("{} dialog={:?}", banner_state(self), shown(self)),
            );
            self.show_banner("Completed.", BannerButton::NewJob);
            check(
                "the banner of a finished transfer is New Job again",
                banner.is_revealed()
                    && banner.button_label().as_deref() == Some("New Job")
                    && *imp.banner_button.borrow() == BannerButton::NewJob,
                banner_state(self),
            );
            self.hide_banner();

            // The two notices that borrow the banner, each put up over a
            // partial transfer's: neither has a button, and a press that
            // reached them anyway would do nothing.
            self.add_source(&gio::File::for_path(dir.join("src")));
            self.set_dest(&gio::File::for_path(dir.join("dst")));
            self.set_filters(&[FilterRule::exclude("*.tmp")]);
            let ran = transfer(self, &mirror_lines, mirror, 23);
            dry_run(self, ">f+++++++++ src/a.txt\n");
            let outcome = self.settle_preview(&done(Severity::Success, Some(0)), false);
            self.present_preview(outcome, done(Severity::Success, Some(0)));
            check(
                "the matched-nothing notice still has no button",
                ran && outcome == PreviewOutcome::Report
                    && banner.is_revealed()
                    && banner.title().contains("matched nothing")
                    && banner.button_label().unwrap_or_default().is_empty()
                    && *imp.banner_button.borrow() == BannerButton::None
                    && imp.filter_banner.get(),
                banner_state(self),
            );
            press(self);
            check(
                "…and pressing where it would be does nothing",
                self.visible_dialog().is_none()
                    && banner.is_revealed()
                    && imp.sources.borrow().len() == 1,
                banner_state(self),
            );
            self.add_filter(FilterKind::Exclude, "while-reading");
            check(
                "…and editing the rules still takes it down",
                !banner.is_revealed() && !imp.filter_banner.get(),
                banner_state(self),
            );

            let ran = transfer(self, &mirror_lines, mirror, 23);
            dry_run(self, SCAN_FAILED);
            let outcome = self.settle_preview(&done(Severity::Partial, Some(23)), false);
            self.present_preview(outcome, done(Severity::Partial, Some(23)));
            check(
                "the incomplete-dry-run notice still has no button",
                ran && outcome == PreviewOutcome::Incomplete
                    && banner.is_revealed()
                    && banner.title().starts_with("Dry run incomplete")
                    && banner.button_label().unwrap_or_default().is_empty()
                    && *imp.banner_button.borrow() == BannerButton::None
                    && !imp.filter_banner.get(),
                banner_state(self),
            );
            press(self);
            check(
                "…and no report of a transfer can be opened from it",
                self.visible_dialog().is_none()
                    && banner.is_revealed()
                    && imp.sources.borrow().len() == 1,
                banner_state(self),
            );
            dismiss(self);
            self.clear_job();

            // The two runs for real, through `run_sync`: what kind of run it
            // was has to survive the run being let go of, and what the report
            // says happened has to be what happened.
            let base = dir.join("partial");
            let mode = |path: &std::path::Path, mode: u32| {
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
            };
            let finish = |w: &ForesightWindow| {
                for _ in 0..150 {
                    if !w.is_running() {
                        break;
                    }
                    pump(100);
                }
                pump(100);
            };

            let (src, dst) = (base.join("mirror/src"), base.join("mirror/dst"));
            let _ = std::fs::create_dir_all(src.join("ok"));
            let _ = std::fs::create_dir_all(src.join("locked"));
            let _ = std::fs::create_dir_all(dst.join("src/ok"));
            let _ = std::fs::write(src.join("ok/a.txt"), "a");
            let _ = std::fs::write(dst.join("src/top-stale.txt"), "stale");
            let _ = std::fs::write(dst.join("src/ok/stale.txt"), "stale");
            mode(&src.join("locked"), 0o000);
            self.add_source(&gio::File::for_path(&src));
            self.set_dest(&gio::File::for_path(&dst));
            imp.delete_row.set_active(true);
            self.run_sync();
            let started = self.is_running();
            finish(self);
            let report = report_of(self);
            let body = report.as_ref().map(PartialReport::body).unwrap_or_default();
            check(
                "a real Mirror run past an unreadable folder ends as a partial with a report",
                started
                    && !self.is_running()
                    && self.visible_dialog().is_none()
                    && banner.is_revealed()
                    && banner.button_label().as_deref() == Some("Show Problems")
                    && report.as_ref().is_some_and(|r| {
                        r.kind == Some(mirror)
                            && r.code == Some(23)
                            && r.problems.iter().any(|p| p == DELETION_SKIPPED)
                            && r.problems.iter().any(|p| p.contains("opendir"))
                            && !r.problems.iter().any(|p| p.starts_with("rsync error:"))
                    })
                    && body.contains(DELETION),
                format!("started={started} {} / {body}", banner_state(self)),
            );
            check(
                "…and deletion did stop part-way: one stale file gone, one left",
                !dst.join("src/top-stale.txt").exists()
                    && dst.join("src/ok/stale.txt").exists()
                    && dst.join("src/ok/a.txt").exists(),
                format!(
                    "top-stale={} ok/stale={} ok/a={}",
                    dst.join("src/top-stale.txt").exists(),
                    dst.join("src/ok/stale.txt").exists(),
                    dst.join("src/ok/a.txt").exists()
                ),
            );
            mode(&src.join("locked"), 0o755);
            mode(&dst.join("src/locked"), 0o755);
            self.clear_job();

            let (src, dst) = (base.join("move/src"), base.join("move/dst"));
            let _ = std::fs::create_dir_all(&src);
            let _ = std::fs::create_dir_all(&dst);
            let _ = std::fs::write(src.join("a.txt"), "a");
            let _ = std::fs::write(src.join("secret.txt"), "s");
            mode(&src.join("secret.txt"), 0o000);
            self.add_source(&gio::File::for_path(&src));
            self.set_dest(&gio::File::for_path(&dst));
            imp.remove_source_row.set_active(true);
            self.run_sync();
            let started = self.is_running();
            finish(self);
            let report = report_of(self);
            let body = report.as_ref().map(PartialReport::body).unwrap_or_default();
            check(
                "a real Move past an unreadable file ends as a partial that says it was a move",
                started
                    && !self.is_running()
                    && banner.is_revealed()
                    && banner.title().ends_with("1 problem reported.")
                    && report.as_ref().is_some_and(|r| {
                        r.kind == Some(moving)
                            && r.problems.len() == 1
                            && r.problems[0].contains("secret.txt")
                    })
                    && body.contains(MOVED)
                    && !body.contains(DELETION),
                format!("started={started} {} / {body}", banner_state(self)),
            );
            check(
                "…and the file that transferred left the source, the other did not",
                !src.join("a.txt").exists()
                    && dst.join("src/a.txt").exists()
                    && src.join("secret.txt").exists()
                    && !dst.join("src/secret.txt").exists(),
                format!(
                    "source a={} secret={}; destination a={} secret={}",
                    src.join("a.txt").exists(),
                    src.join("secret.txt").exists(),
                    dst.join("src/a.txt").exists(),
                    dst.join("src/secret.txt").exists()
                ),
            );
            mode(&src.join("secret.txt"), 0o644);
            self.clear_job();
            let _ = std::fs::remove_dir_all(&base);
            check(
                "the partial-transfer checks leave nothing behind",
                !base.exists() && !self.is_running() && self.visible_dialog().is_none(),
                format!("{} still there", base.display()),
            );
        }

        (pass, fail)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        incomplete_preview_notice, parse_bwlimit, partial_banner_text, partial_report_body,
        refused_start_body, reported_problems, tokenize, BannerButton, PartialReport,
        PreviewOutcome, DELETION_SKIPPED,
    };
    use crate::job::{Completion, RunKind};
    use rsync_events::{classify_exit, Severity};

    const OPENDIR: &str =
        "rsync: [sender] opendir \"/x/src/locked\" failed: Permission denied (13)";
    const SEND_FILES: &str =
        "rsync: [sender] send_files failed to open \"/x/src/secret.txt\": Permission denied (13)";
    const SUMMARY: &str = "rsync error: some files/attrs were not transferred (see previous \
                           errors) (code 23) at main.c(1394) [sender=3.5.0-g483b5efc]";

    fn lines(of: &[&str]) -> Vec<String> {
        of.iter().map(|l| l.to_string()).collect()
    }

    fn transfer(moves: bool, deletes: bool) -> Option<RunKind> {
        Some(RunKind::Transfer { moves, deletes })
    }

    fn exited(code: i32) -> Completion {
        let (severity, message) = classify_exit(code);
        Completion {
            severity,
            message,
            code: Some(code),
        }
    }

    /// rsync's closing line restates the exit code and names nothing; one
    /// unreadable file is one problem, not two.
    #[test]
    fn the_exit_summary_is_not_counted_as_a_problem() {
        assert_eq!(
            reported_problems(&lines(&[OPENDIR, DELETION_SKIPPED, SUMMARY])),
            lines(&[OPENDIR, DELETION_SKIPPED])
        );
        // Over ssh each end prints one.
        assert!(reported_problems(&lines(&[SUMMARY, SUMMARY])).is_empty());
        // Only the summary: a line that merely begins the same way is kept.
        assert_eq!(
            reported_problems(&lines(&["rsync error: something else"])).len(),
            1
        );
    }

    #[test]
    fn the_partial_banner_counts_what_was_reported() {
        let message = classify_exit(23).1;
        assert_eq!(
            partial_banner_text(&message, Some(23), 1),
            "Completed, but some files could not be transferred. 1 problem reported."
        );
        assert_eq!(
            partial_banner_text(&message, Some(23), 3),
            "Completed, but some files could not be transferred. 3 problems reported."
        );
    }

    #[test]
    fn a_partial_with_nothing_collected_says_what_the_exit_code_means() {
        let message = classify_exit(24).1;
        assert_eq!(
            partial_banner_text(&message, Some(24), 0),
            "Completed, but some source files vanished mid-sync. No specific errors were \
             collected; rsync's output is in the activity log (exit code 24)."
        );
        assert!(partial_banner_text(&message, None, 0).ends_with("the activity log."));
    }

    /// The button follows from what there is to show, and from nothing else.
    #[test]
    fn a_partial_offers_its_problems_only_when_it_has_some() {
        let report = PartialReport::new(
            &exited(23),
            transfer(false, false),
            &lines(&[SEND_FILES, SUMMARY]),
        );
        assert_eq!(report.problems, lines(&[SEND_FILES]));
        assert_eq!(report.button(), BannerButton::Problems(report.clone()));
        assert_eq!(report.button().label(), Some("Show Problems"));
        assert!(report.banner_text().ends_with("1 problem reported."));

        // Exit 24: the vanished-file lines are not errors, so nothing is
        // collected — and a lone summary line is nothing to list either.
        for errors in [lines(&[]), lines(&[SUMMARY])] {
            let report = PartialReport::new(&exited(24), transfer(true, true), &errors);
            assert_eq!(report.button(), BannerButton::NewJob);
            assert!(report.banner_text().contains("(exit code 24)"));
        }
        assert_eq!(BannerButton::default(), BannerButton::None);
        assert_eq!(BannerButton::NewJob.label(), Some("New Job"));
        assert_eq!(BannerButton::None.label(), None);
    }

    #[test]
    fn the_partial_report_says_the_sync_finished_and_what_went_wrong() {
        let body = partial_report_body(transfer(false, false), Some(23), &lines(&[SEND_FILES]));
        assert_eq!(
            body,
            format!(
                "The sync ran to its end, but not everything went through. rsync \
                 reported 1 problem, shown below.\n\nrsync exit code 23.\n\n{SEND_FILES}"
            )
        );
        assert!(!body.contains("failed to transfer") && !body.contains("Sync failed"));
        let body = partial_report_body(None, None, &lines(&[OPENDIR, SEND_FILES]));
        assert!(body.contains("reported 2 problems, shown below."), "{body}");
        assert!(!body.contains("exit code"), "{body}");
    }

    /// Deletion is mentioned when the run deleted *and* rsync said it stopped
    /// deleting; either alone says nothing about this run's deletions.
    #[test]
    fn the_deletion_sentence_needs_a_deleting_run_and_the_skipped_line() {
        const SENTENCE: &str = "Mirror deletions stopped part-way";
        let skipped = lines(&[OPENDIR, DELETION_SKIPPED]);
        let body = partial_report_body(transfer(false, true), Some(23), &skipped);
        assert!(body.contains(SENTENCE), "{body}");
        assert!(body.contains("may already have been deleted from the destination"));
        assert!(!body.contains("source"), "{body}");

        for (kind, errors) in [
            // Deleting, but deletion was not interrupted.
            (transfer(false, true), lines(&[SEND_FILES])),
            // The line without a run that deletes.
            (transfer(false, false), skipped.clone()),
            (transfer(true, false), skipped.clone()),
            (Some(RunKind::DryRun), skipped.clone()),
            (None, skipped.clone()),
        ] {
            let body = partial_report_body(kind, Some(23), &errors);
            assert!(!body.contains(SENTENCE), "{kind:?}: {body}");
        }
    }

    #[test]
    fn the_move_sentence_appears_for_a_move_only() {
        const SENTENCE: &str = "Files that transferred were removed from the source";
        let errors = lines(&[SEND_FILES]);
        let body = partial_report_body(transfer(true, false), Some(23), &errors);
        assert!(body.contains(SENTENCE), "{body}");
        assert!(body.contains("are still in the source") && !body.contains("deleted"));
        for kind in [
            transfer(false, false),
            transfer(false, true),
            Some(RunKind::DryRun),
            None,
        ] {
            assert!(!partial_report_body(kind, Some(23), &errors).contains(SENTENCE));
        }
        // Both at once, deletions first, each in its own paragraph.
        let body = partial_report_body(transfer(true, true), Some(23), &lines(&[DELETION_SKIPPED]));
        let (deleted, moved) = (body.find("Mirror deletions"), body.find(SENTENCE));
        assert!(deleted.is_some() && deleted < moved, "{body}");
    }

    /// A big tree can collect thousands of lines.
    #[test]
    fn the_partial_report_caps_its_list() {
        let many: Vec<String> = (0..2500).map(|i| format!("rsync: error {i}")).collect();
        let body = partial_report_body(transfer(false, false), Some(23), &many);
        assert!(body.contains("reported 2500 problems"), "{body}");
        assert!(body.ends_with("rsync: error 19\n…and 2480 more"), "{body}");
        assert!(!body.contains("rsync: error 20\n"));
        let exactly: Vec<String> = many[..20].to_vec();
        let body = partial_report_body(transfer(false, false), Some(23), &exactly);
        assert!(
            body.ends_with("rsync: error 19") && !body.contains("more"),
            "{body}"
        );
    }

    /// Every ending of a dry run, on both paths. Only a run that finished may
    /// lead anywhere; exit 23 is tolerated for a transfer, not for the scan a
    /// confirmation is built on.
    #[test]
    fn only_a_complete_dry_run_leads_to_a_confirmation() {
        use PreviewOutcome::*;
        for (severity, plain, start) in [
            (Severity::Success, Report, Confirm),
            (Severity::Partial, Incomplete, Refused),
            (Severity::Cancelled, Cancelled, Cancelled),
            (Severity::Error, Failed, Failed),
        ] {
            assert_eq!(PreviewOutcome::decide(severity, false), plain);
            assert_eq!(PreviewOutcome::decide(severity, true), start);
        }
        for outcome in [Incomplete, Refused, Cancelled, Failed] {
            assert!(!outcome.is_complete(), "{outcome:?}");
        }
        assert!(Report.is_complete() && Confirm.is_complete());
    }

    #[test]
    fn the_incomplete_notice_speaks_of_a_dry_run() {
        let text = incomplete_preview_notice(Some(23), 2);
        assert_eq!(
            text,
            "Dry run incomplete — rsync reported 2 errors (exit code 23). \
             This list may be missing changes. Nothing was transferred."
        );
        assert_eq!(
            incomplete_preview_notice(None, 0),
            "Dry run incomplete. This list may be missing changes. Nothing was transferred."
        );
        assert!(incomplete_preview_notice(Some(24), 1).contains("1 error "));
    }

    #[test]
    fn the_refusal_says_nothing_happened_and_why() {
        let errors = vec!["rsync: [sender] opendir \"/s/locked\" failed".to_string()];
        let body = refused_start_body(true, false, Some(23), &errors);
        assert!(body.starts_with("The sync was not started. Nothing was transferred"));
        assert!(body.contains("Mirror deletions") && !body.contains("Move files"));
        assert!(body.contains("rsync exit code 23."));
        assert!(body.ends_with(&errors[0]), "{body}");

        let body = refused_start_body(false, true, Some(23), &[]);
        assert!(body.contains("Move files") && !body.contains("Mirror deletions"));
        assert!(body.ends_with("rsync exit code 23."), "{body}");

        // A tree full of unreadable folders must not become a wall.
        let many: Vec<String> = (0..25).map(|i| format!("rsync: error {i}")).collect();
        let body = refused_start_body(true, true, Some(23), &many);
        assert!(body.contains("rsync: error 19\n…and 5 more"), "{body}");
        assert!(!body.contains("rsync: error 20"));
    }

    #[test]
    fn bwlimit_token_parses_value_and_unit() {
        assert_eq!(parse_bwlimit("85M"), (85.0, 1));
        assert_eq!(parse_bwlimit("500K"), (500.0, 0));
        assert_eq!(parse_bwlimit("2G"), (2.0, 2));
        assert_eq!(parse_bwlimit("85m"), (85.0, 1)); // case-insensitive suffix
        assert_eq!(parse_bwlimit("500"), (500.0, 0)); // bare number = KiB/s
        assert_eq!(parse_bwlimit(""), (0.0, 1)); // unlimited, default unit MB/s
    }

    #[test]
    fn tokenize_splits_on_whitespace() {
        assert_eq!(tokenize("  *.tmp   .git "), vec!["*.tmp", ".git"]);
        assert!(tokenize("").is_empty());
    }
}
