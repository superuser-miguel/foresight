//! Generates `config.rs` (into OUT_DIR) with build-time constants.
//!
//! Values come from `FORESIGHT_*` env vars when Meson/Flatpak drives the build,
//! and fall back to dev defaults so a bare `cargo build`/`cargo test` (CI, host
//! iteration) still compiles. `main.rs` pulls the result in with `include!`.

use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let app_id = env::var("FORESIGHT_APP_ID")
        .unwrap_or_else(|_| "io.github.superuser_miguel.Foresight".to_string());
    // Empty in dev: main.rs falls back to $FORESIGHT_GRESOURCE when this is "".
    let pkgdatadir = env::var("FORESIGHT_PKGDATADIR").unwrap_or_default();
    // Gettext domain, passed by Meson (see po/meson.build). In dev, "foresight"
    // matches the Meson project name so bindtextdomain finds the right .mo.
    let gettext_package =
        env::var("FORESIGHT_GETTEXT_PACKAGE").unwrap_or_else(|_| "foresight".to_string());
    // Directory holding <lang>/LC_MESSAGES/<domain>.mo, passed by Meson.
    // Empty in dev, where there is no installed catalogue to bind.
    let localedir = env::var("FORESIGHT_LOCALEDIR").unwrap_or_default();
    let version =
        env::var("FORESIGHT_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());
    let profile = env::var("FORESIGHT_PROFILE").unwrap_or_else(|_| "development".to_string());

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR set by cargo");
    let dest = Path::new(&out_dir).join("config.rs");
    let contents = format!(
        "pub const APP_ID: &str = {app_id:?};\n\
         pub const PKGDATADIR: &str = {pkgdatadir:?};\n\
         pub const GETTEXT_PACKAGE: &str = {gettext_package:?};\n\
         pub const LOCALEDIR: &str = {localedir:?};\n\
         pub const VERSION: &str = {version:?};\n\
         pub const PROFILE: &str = {profile:?};\n"
    );
    fs::write(&dest, contents).expect("write config.rs");

    for var in [
        "FORESIGHT_APP_ID",
        "FORESIGHT_PKGDATADIR",
        "FORESIGHT_GETTEXT_PACKAGE",
        "FORESIGHT_LOCALEDIR",
        "FORESIGHT_VERSION",
        "FORESIGHT_PROFILE",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }
}
