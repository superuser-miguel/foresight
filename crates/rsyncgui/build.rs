//! Generates `config.rs` (into OUT_DIR) with build-time constants.
//!
//! Values come from `RSYNCGUI_*` env vars when Meson/Flatpak drives the build,
//! and fall back to dev defaults so a bare `cargo build`/`cargo test` (CI, host
//! iteration) still compiles. `main.rs` pulls the result in with `include!`.

use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let app_id =
        env::var("RSYNCGUI_APP_ID").unwrap_or_else(|_| "io.github.CHANGEME.RsyncGUI".to_string());
    // Empty in dev: main.rs falls back to $RSYNCGUI_GRESOURCE when this is "".
    let pkgdatadir = env::var("RSYNCGUI_PKGDATADIR").unwrap_or_default();
    let version =
        env::var("RSYNCGUI_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());
    let profile = env::var("RSYNCGUI_PROFILE").unwrap_or_else(|_| "development".to_string());

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR set by cargo");
    let dest = Path::new(&out_dir).join("config.rs");
    let contents = format!(
        "pub const APP_ID: &str = {app_id:?};\n\
         pub const PKGDATADIR: &str = {pkgdatadir:?};\n\
         pub const VERSION: &str = {version:?};\n\
         pub const PROFILE: &str = {profile:?};\n"
    );
    fs::write(&dest, contents).expect("write config.rs");

    for var in [
        "RSYNCGUI_APP_ID",
        "RSYNCGUI_PKGDATADIR",
        "RSYNCGUI_VERSION",
        "RSYNCGUI_PROFILE",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }
}
