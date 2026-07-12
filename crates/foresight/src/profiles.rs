//! Saved presets — reusable sets of the *Advanced* rsync options (not the
//! source/destination, which need fresh portal grants each session).
//!
//! Persisted as a small `glib::KeyFile` (one group per preset) under the app's
//! config dir, which inside the Flatpak is
//! `~/.var/app/<app-id>/config/foresight/profiles.ini` — writable and durable,
//! no portal needed. Lists are stored space-joined, matching how the UI
//! tokenises the exclude/extra-args fields.

use gtk::glib::{self, KeyFile, KeyFileFlags};
use std::path::{Path, PathBuf};

/// One named preset. Mirrors the Advanced controls, never the paths.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub delete: bool,
    pub verbose: bool,
    pub remove_source_files: bool,
    /// rsync rate token like `"85M"`; empty/`None` means unlimited.
    pub bwlimit: Option<String>,
    pub excludes: Vec<String>,
    pub extra_args: Vec<String>,
}

fn profiles_path() -> PathBuf {
    glib::user_config_dir()
        .join("foresight")
        .join("profiles.ini")
}

fn split(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

/// Load every saved preset (empty list if the file is missing or unreadable).
pub fn load() -> Vec<Profile> {
    load_from(&profiles_path())
}

fn load_from(path: &Path) -> Vec<Profile> {
    let key_file = KeyFile::new();
    if key_file.load_from_file(path, KeyFileFlags::NONE).is_err() {
        return Vec::new();
    }

    let mut out = Vec::new();
    for group in key_file.groups().iter() {
        let name = group.to_string();
        let get = |k: &str| {
            key_file
                .string(&name, k)
                .map(|g| g.to_string())
                .unwrap_or_default()
        };
        let bw = get("bwlimit");
        out.push(Profile {
            delete: key_file.boolean(&name, "delete").unwrap_or(false),
            verbose: key_file.boolean(&name, "verbose").unwrap_or(false),
            remove_source_files: key_file.boolean(&name, "move").unwrap_or(false),
            bwlimit: (!bw.is_empty()).then_some(bw),
            excludes: split(&get("excludes")),
            extra_args: split(&get("extra_args")),
            name,
        });
    }
    out
}

/// Persist the full set of presets, replacing whatever was on disk.
pub fn save_all(profiles: &[Profile]) {
    save_all_to(profiles, &profiles_path());
}

fn save_all_to(profiles: &[Profile], path: &Path) {
    let key_file = KeyFile::new();
    for p in profiles {
        key_file.set_boolean(&p.name, "delete", p.delete);
        key_file.set_boolean(&p.name, "verbose", p.verbose);
        key_file.set_boolean(&p.name, "move", p.remove_source_files);
        key_file.set_string(&p.name, "bwlimit", p.bwlimit.as_deref().unwrap_or(""));
        key_file.set_string(&p.name, "excludes", &p.excludes.join(" "));
        key_file.set_string(&p.name, "extra_args", &p.extra_args.join(" "));
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = key_file.save_to_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_a_keyfile() {
        let path = std::env::temp_dir().join(format!("foresight-prof-{}.ini", std::process::id()));
        let originals = vec![
            Profile {
                name: "HDD move".into(),
                delete: false,
                verbose: true,
                remove_source_files: true,
                bwlimit: Some("85M".into()),
                excludes: vec!["*.tmp".into(), ".git".into()],
                extra_args: vec!["--partial".into()],
            },
            Profile {
                name: "Mirror strict".into(),
                delete: true,
                verbose: false,
                remove_source_files: false,
                bwlimit: None,
                excludes: vec![],
                extra_args: vec![],
            },
        ];

        save_all_to(&originals, &path);
        let mut loaded = load_from(&path);
        // group order from a KeyFile is not guaranteed; compare as sets by name.
        loaded.sort_by(|a, b| a.name.cmp(&b.name));
        let mut expected = originals.clone();
        expected.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(loaded, expected);

        let _ = std::fs::remove_file(&path);
    }
}
