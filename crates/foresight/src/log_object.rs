//! `LogObject` — one row in the streaming transfer log.
//!
//! Unlike the Preview list (which groups a *finished* dry run by change kind),
//! the transfer log is chronological: files, rsync messages, and errors are
//! appended in the order they arrive, each as a typed row with a leading icon,
//! a primary line (path or text), and an optional right-aligned tag. The
//! presentation is precomputed here so the list factory just reads fields.

use gtk::glib;
use gtk::subclass::prelude::*;
use rsync_events::{ChangeKind, ItemizedChange, Message};

mod imp {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    pub struct LogObject {
        pub icon: RefCell<String>,
        pub primary: RefCell<String>,
        pub detail: RefCell<String>,
        /// Render the primary line in red (deletions, errors).
        pub danger: Cell<bool>,
        /// Render dimmed (the command header, informational messages).
        pub dim: Cell<bool>,
        /// Use a monospace font for the primary line (the command header).
        pub mono: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LogObject {
        const NAME: &'static str = "ForesightLogObject";
        type Type = super::LogObject;
    }

    impl ObjectImpl for LogObject {}
}

glib::wrapper! {
    pub struct LogObject(ObjectSubclass<imp::LogObject>);
}

impl LogObject {
    fn build(icon: &str, primary: String, detail: &str, danger: bool, dim: bool, mono: bool) -> Self {
        let obj: Self = glib::Object::new();
        let imp = obj.imp();
        *imp.icon.borrow_mut() = icon.to_string();
        *imp.primary.borrow_mut() = primary;
        *imp.detail.borrow_mut() = detail.to_string();
        imp.danger.set(danger);
        imp.dim.set(dim);
        imp.mono.set(mono);
        obj
    }

    /// The `rsync …` command shown once at the top of a run.
    pub fn command(command: String) -> Self {
        Self::build("utilities-terminal-symbolic", command, "", false, true, true)
    }

    /// One itemized file from the live transfer (`%i %n%L`).
    pub fn change(change: &ItemizedChange) -> Self {
        let mut primary = change.path.clone();
        if let Some(target) = &change.link_target {
            primary.push_str(" → ");
            primary.push_str(target);
        }
        let (icon, tag) = kind_meta(change.kind());
        let danger = change.kind() == ChangeKind::Deleted;
        Self::build(icon, primary, tag, danger, false, false)
    }

    /// A verbatim rsync message (informational or an error/warning).
    pub fn message(message: &Message) -> Self {
        if message.is_error {
            Self::build("dialog-error-symbolic", message.text.clone(), "", true, false, false)
        } else {
            Self::build(
                "dialog-information-symbolic",
                message.text.clone(),
                "",
                false,
                true,
                false,
            )
        }
    }

    pub fn icon(&self) -> String {
        self.imp().icon.borrow().clone()
    }
    pub fn primary(&self) -> String {
        self.imp().primary.borrow().clone()
    }
    pub fn detail(&self) -> String {
        self.imp().detail.borrow().clone()
    }
    pub fn danger(&self) -> bool {
        self.imp().danger.get()
    }
    pub fn dim(&self) -> bool {
        self.imp().dim.get()
    }
    pub fn mono(&self) -> bool {
        self.imp().mono.get()
    }
}

/// `(icon name, right-aligned tag)` for each change kind.
fn kind_meta(kind: ChangeKind) -> (&'static str, &'static str) {
    match kind {
        ChangeKind::Created => ("list-add-symbolic", "New"),
        ChangeKind::Updated => ("emblem-synchronizing-symbolic", "Updated"),
        ChangeKind::Attrs => ("document-properties-symbolic", "Attributes"),
        ChangeKind::Deleted => ("user-trash-symbolic", "Deleted"),
        ChangeKind::Unchanged => ("object-select-symbolic", "Unchanged"),
    }
}
