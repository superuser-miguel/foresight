//! Translation helpers — thin wrappers around gettext that keep the code
//! readable and give `xgettext` a stable extraction surface (see `po/`).
//!
//! The app's strings come in two shapes:
//!
//! - Plain strings, translated with [`i18n`]. The Blueprint UI already marks
//!   its strings with `_("…")`, which blueprint-compiler turns into
//!   `translatable="yes"` and GTK resolves at runtime through the same gettext
//!   domain — so `textdomain` in `main` is what makes those work too.
//! - Strings with placeholders, translated with [`i18n_f`] (positional `{}`)
//!   or [`i18n_k`] (named `{name}`). Rust's `format!` cannot take a translated
//!   string as its format (the format must be a compile-time literal), so the
//!   placeholder substitution happens here, after gettext has translated.
//!
//! `xgettext` is configured in `po/meson.build` with `--keyword=i18n`,
//! `--keyword=i18n_f`, `--keyword=i18n_k` and `--keyword=ni18n:1,2`, matching
//! the GNOME convention used by Amberol et al.

use gettextrs::{gettext, ngettext};

/// Translate a plain string.
#[allow(dead_code)]
pub fn i18n(format: &str) -> String {
    gettext(format)
}

/// Mark a string as translatable without translating it — the `N_` ("no-op")
/// convention from C gettext.
///
/// Used to register msgids that live in `static`/`const` tables (the
/// capability registry), which cannot call [`i18n`] at initialisation time.
/// The call site still needs to translate: `i18n(CAPABILITIES[i].name)`.
/// `xgettext` extracts these via `--keyword=gettext_noop`.
#[allow(dead_code)]
pub const fn gettext_noop(s: &'static str) -> &'static str {
    s
}

/// Translate a string with positional `{}` placeholders.
///
/// `args` fill the placeholders in order, like `format!` would. The original
/// string is the msgid, so translators see the placeholders as `{}`.
#[allow(dead_code)]
pub fn i18n_f(format: &str, args: &[&str]) -> String {
    let mut s = gettext(format);
    for arg in args {
        if let Some(pos) = s.find("{}") {
            s.replace_range(pos..pos + 2, arg);
        }
    }
    s
}

/// Translate a string with named `{name}` placeholders.
///
/// `kwargs` is a list of `(name, value)` pairs; every `{name}` occurrence is
/// replaced by its value. Names use the `gettext` `{name}` convention (not
/// Rust's `{name}` positional/named syntax) so translators can reorder them.
#[allow(dead_code)]
pub fn i18n_k(format: &str, kwargs: &[(&str, &str)]) -> String {
    let mut s = gettext(format);
    for (k, v) in kwargs {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s
}

/// Translate a singular/plural pair, picking by `n`.
#[allow(dead_code)]
pub fn ni18n(single: &str, multiple: &str, n: u32) -> String {
    ngettext(single, multiple, n)
}
