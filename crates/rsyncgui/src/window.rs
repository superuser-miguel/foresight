//! The main window: a composite template bound to `src/ui/window.blp`.
//!
//! All layout lives in the Blueprint. Rust reaches widgets only through the
//! `#[template_child]` bindings below; this file adds no widget tree of its own.

use adw::subclass::prelude::*;
use gtk::glib;

mod imp {
    use super::*;

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

    impl ObjectImpl for RsyncGuiWindow {}
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
}
