//! The remote-endpoint dialog, and the host-key confirmation in front of it.
//!
//! Built in Rust rather than Blueprint, following [`crate::help`]: it is a
//! transient dialog assembled from a handful of rows, not a screen with layout
//! worth diffing.
//!
//! The whole point of this module is that [`present`] hands back an endpoint
//! **only** once the host behind it is trusted. Validation and first-contact
//! confirmation happen here, so the window never has to hold a half-checked
//! endpoint or remember to ask.

use adw::prelude::*;
use gtk::glib;

use crate::endpoint::Endpoint;
use crate::ssh;

/// Show the endpoint form. `on_accept` runs only for a validated endpoint whose
/// host key is already trusted, or has just been confirmed by the user.
pub fn present<F: Fn(Endpoint) + 'static>(
    parent: &impl IsA<gtk::Widget>,
    title: &str,
    existing: Option<Endpoint>,
    on_accept: F,
) {
    let existing = existing.unwrap_or_default();

    let user_row = adw::EntryRow::builder().title("User (optional)").build();
    user_row.set_text(existing.user.as_deref().unwrap_or(""));

    let host_row = adw::EntryRow::builder().title("Host").build();
    host_row.set_text(&existing.host);

    let port_row = adw::SpinRow::builder()
        .title("Port")
        .subtitle("22 is the default")
        .adjustment(&gtk::Adjustment::new(
            existing.port.unwrap_or(22) as f64,
            1.0,
            65535.0,
            1.0,
            10.0,
            0.0,
        ))
        .build();

    let path_row = adw::EntryRow::builder()
        .title("Path on that machine")
        .build();
    path_row.set_text(&existing.path);

    let group = adw::PreferencesGroup::builder()
        .description(
            "Foresight authenticates with the keys already in your desktop's SSH \
             agent. No password is ever asked for, and no key leaves the agent.",
        )
        .build();
    for row in [
        user_row.clone().upcast::<gtk::Widget>(),
        host_row.clone().upcast(),
        port_row.clone().upcast(),
        path_row.clone().upcast(),
    ] {
        group.add(&row);
    }

    // Inline, because a toast behind a modal dialog is a message nobody reads.
    let error = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .css_classes(["error"])
        .build();
    let error_group = adw::PreferencesGroup::new();
    error_group.add(&error);

    let page = adw::PreferencesPage::new();
    page.add(&group);
    page.add(&error_group);

    let header = adw::HeaderBar::builder()
        .show_end_title_buttons(false)
        .show_start_title_buttons(false)
        .build();
    let cancel = gtk::Button::with_label("Cancel");
    let connect = gtk::Button::builder()
        .label("Connect")
        .css_classes(["suggested-action"])
        .build();
    header.pack_start(&cancel);
    header.pack_end(&connect);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&page));

    let dialog = adw::Dialog::builder()
        .title(title)
        .content_width(460)
        .child(&toolbar)
        .build();

    cancel.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));

    let on_accept = std::rc::Rc::new(on_accept);
    connect.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        #[weak]
        user_row,
        #[weak]
        host_row,
        #[weak]
        port_row,
        #[weak]
        path_row,
        #[weak]
        error,
        #[weak]
        connect,
        #[strong]
        on_accept,
        move |_| {
            let user = user_row.text().trim().to_string();
            let port = port_row.value() as u16;
            let endpoint = Endpoint {
                user: (!user.is_empty()).then_some(user),
                host: host_row.text().trim().to_string(),
                port: (port != 22).then_some(port),
                path: path_row.text().trim().to_string(),
            };

            if let Err(e) = endpoint.validate() {
                show_error(&error, &e.to_string());
                return;
            }
            error.set_visible(false);

            // Already trusted: nothing to ask, go straight through.
            let known = ssh::known_hosts_path();
            if ssh::is_trusted(&known, &endpoint.host, endpoint.port) {
                on_accept(endpoint);
                dialog.close();
                return;
            }

            // First contact. ssh-keyscan talks to the network, so it must not
            // run on the main thread — a dead host would freeze the window for
            // the full scan timeout.
            connect.set_sensitive(false);
            connect.set_label("Checking…");
            let host = endpoint.host.clone();
            let scan_port = endpoint.port;
            glib::spawn_future_local(glib::clone!(
                #[weak]
                dialog,
                #[weak]
                error,
                #[weak]
                connect,
                #[strong]
                on_accept,
                async move {
                    let scanned =
                        gtk::gio::spawn_blocking(move || ssh::scan_host(&host, scan_port)).await;
                    connect.set_sensitive(true);
                    connect.set_label("Connect");

                    let keys = match scanned {
                        Ok(Ok(keys)) => keys,
                        Ok(Err(e)) => return show_error(&error, &e.to_string()),
                        Err(_) => return show_error(&error, "the host check did not finish"),
                    };
                    confirm_host_key(&dialog, endpoint, keys, on_accept);
                }
            ));
        }
    ));

    dialog.present(Some(parent));
}

fn show_error(label: &gtk::Label, text: &str) {
    label.set_text(text);
    label.set_visible(true);
}

/// The first-contact question: show what the host offered and let the user
/// compare it against what they were told, before anything is written.
///
/// Deliberately not a "remember this" checkbox on a transfer dialog — trusting
/// a host key is its own decision, and the fingerprint is the only thing that
/// makes it a real one rather than a click-through.
fn confirm_host_key<F: Fn(Endpoint) + 'static>(
    parent: &adw::Dialog,
    endpoint: Endpoint,
    keys: Vec<ssh::HostKey>,
    on_accept: std::rc::Rc<F>,
) {
    let fingerprints = keys
        .iter()
        .map(|k| format!("{}  {}", k.key_type, k.fingerprint))
        .collect::<Vec<_>>()
        .join("\n");

    let alert = adw::AlertDialog::builder()
        .heading(format!("Trust {}?", endpoint.host))
        .body(format!(
            "Foresight has not connected to this machine before. Check that the \
             fingerprint matches what the machine's owner told you — if it does \
             not, someone else may be answering.\n\n{fingerprints}\n\nTrusting it \
             records the key so this is asked once."
        ))
        .build();
    alert.add_response("cancel", "Cancel");
    alert.add_response("trust", "Trust");
    alert.set_response_appearance("trust", adw::ResponseAppearance::Suggested);
    alert.set_default_response(Some("cancel"));
    alert.set_close_response("cancel");

    alert.connect_response(
        None,
        glib::clone!(
            #[weak]
            parent,
            move |_, response| {
                if response != "trust" {
                    return;
                }
                if let Err(e) = ssh::trust(&ssh::known_hosts_path(), &keys) {
                    // Writing failed, so the key is NOT trusted. Saying so is
                    // the only honest option: proceeding would mean the next
                    // run asks again, or worse, appears trusted and is not.
                    let oops = adw::AlertDialog::builder()
                        .heading("Could not record the host key")
                        .body(format!("{e}"))
                        .build();
                    oops.add_response("ok", "OK");
                    oops.present(Some(&parent));
                    return;
                }
                on_accept(endpoint.clone());
                parent.close();
            }
        ),
    );
    alert.present(Some(parent));
}
