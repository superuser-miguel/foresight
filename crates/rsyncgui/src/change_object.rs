//! `ChangeObject` — a `glib::Object` wrapper around one `ItemizedChange`, so a
//! `gio::ListStore` / `gtk::ListView` can render the dry-run preview.
//!
//! Each object precomputes its display string, a sort key by `ChangeKind`
//! (grouping the list into sections), the section heading, and whether it
//! should render destructively (deletions).

use gtk::glib;
use gtk::subclass::prelude::*;
use rsync_events::{ChangeKind, ItemizedChange};

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    pub struct ChangeObject {
        pub display: RefCell<String>,
        pub kind_order: Cell<i32>,
        pub kind_name: RefCell<String>,
        pub destructive: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ChangeObject {
        const NAME: &'static str = "RsyncGuiChangeObject";
        type Type = super::ChangeObject;
    }

    impl ObjectImpl for ChangeObject {}
}

glib::wrapper! {
    pub struct ChangeObject(ObjectSubclass<imp::ChangeObject>);
}

impl ChangeObject {
    pub fn new(change: &ItemizedChange) -> Self {
        let obj: Self = glib::Object::new();
        let imp = obj.imp();

        let mut display = change.path.clone();
        if let Some(target) = &change.link_target {
            display.push_str(" → ");
            display.push_str(target);
        }

        let (order, name) = kind_meta(change.kind());
        *imp.display.borrow_mut() = display;
        imp.kind_order.set(order);
        *imp.kind_name.borrow_mut() = name.to_string();
        imp.destructive.set(change.kind() == ChangeKind::Deleted);
        obj
    }

    pub fn display(&self) -> String {
        self.imp().display.borrow().clone()
    }

    /// Stable ordering key; also defines section boundaries in the list.
    pub fn kind_order(&self) -> i32 {
        self.imp().kind_order.get()
    }

    pub fn kind_name(&self) -> String {
        self.imp().kind_name.borrow().clone()
    }

    pub fn destructive(&self) -> bool {
        self.imp().destructive.get()
    }
}

/// `(sort order, section heading)` for each change kind.
fn kind_meta(kind: ChangeKind) -> (i32, &'static str) {
    match kind {
        ChangeKind::Created => (0, "New"),
        ChangeKind::Updated => (1, "Updated"),
        ChangeKind::Attrs => (2, "Attributes only"),
        ChangeKind::Deleted => (3, "Deleted"),
        ChangeKind::Unchanged => (4, "Unchanged"),
    }
}
