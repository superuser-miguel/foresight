//! Saved presets — reusable sets of the *Advanced* rsync options (not the
//! source/destination, which need fresh portal grants each session).
//!
//! Persisted as a small `glib::KeyFile` (one group per preset) under the app's
//! config dir, which inside the Flatpak is
//! `~/.var/app/<app-id>/config/foresight/profiles.ini` — writable and durable,
//! no portal needed.
//!
//! Extra arguments are stored space-joined, matching how the UI tokenises that
//! field. Exclude rules are *not*: a rule may contain spaces (`My Documents/`),
//! and space-joining would silently split it into two wrong rules on the next
//! load. Each rule gets its own key instead — see [`EXCLUDE_KEY_PREFIX`].

use gtk::glib::{self, KeyFile, KeyFileFlags};
use std::path::{Path, PathBuf};

/// One named preset. Mirrors the Advanced controls, never the paths.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub delete: bool,
    /// Copy a lone folder's *contents* rather than the folder itself.
    pub sync_contents: bool,
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

/// Prefix for the per-rule keys: `exclude_0`, `exclude_1`, … — one key each
/// rather than a single delimited value.
///
/// A delimited list is the obvious encoding, but glib-rs binds
/// `KeyFile::string_list` without a matching `set_string_list`, and writing one
/// by hand means reproducing the `;`-escaping `g_key_file_set_string_list`
/// performs — glib internals we would be guessing at across versions. One key
/// per rule removes the separator from the problem entirely: `set_string`
/// already escapes anything a rule can hold, `;` and spaces included.
///
/// Deliberately not the old `excludes` key. Presets written before this editor
/// stored rules space-joined, so a distinct key lets [`load_from`] tell the two
/// encodings apart outright instead of inferring it from whether some value
/// happens to contain a space.
const EXCLUDE_KEY_PREFIX: &str = "exclude_";
/// The pre-editor key, still read so old presets keep their rules.
const LEGACY_EXCLUDES_KEY: &str = "excludes";

/// Read `exclude_0`, `exclude_1`, … until one is missing. `save_all_to` writes
/// a fresh KeyFile every time, so the run is always contiguous — a gap can only
/// mean the end.
fn read_exclude_rules(key_file: &KeyFile, group: &str) -> Vec<String> {
    let mut rules = Vec::new();
    while let Ok(rule) = key_file.string(group, &format!("{EXCLUDE_KEY_PREFIX}{}", rules.len())) {
        rules.push(rule.to_string());
    }
    rules
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
            // Absent in presets saved before this option existed — those were
            // written when a lone folder always synced its contents, so `false`
            // (nest the folder) is the honest new default, not a silent change
            // of what the preset used to do to the *paths* it never stored.
            sync_contents: key_file.boolean(&name, "sync_contents").unwrap_or(false),
            verbose: key_file.boolean(&name, "verbose").unwrap_or(false),
            remove_source_files: key_file.boolean(&name, "move").unwrap_or(false),
            bwlimit: (!bw.is_empty()).then_some(bw),
            excludes: match read_exclude_rules(&key_file, &name) {
                // Pre-editor preset: the only encoding it ever had was
                // space-joined, so splitting is exact, not a heuristic.
                rules if rules.is_empty() => split(&get(LEGACY_EXCLUDES_KEY)),
                rules => rules,
            },
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
        key_file.set_boolean(&p.name, "sync_contents", p.sync_contents);
        key_file.set_boolean(&p.name, "verbose", p.verbose);
        key_file.set_boolean(&p.name, "move", p.remove_source_files);
        key_file.set_string(&p.name, "bwlimit", p.bwlimit.as_deref().unwrap_or(""));
        for (i, rule) in p.excludes.iter().enumerate() {
            key_file.set_string(&p.name, &format!("{EXCLUDE_KEY_PREFIX}{i}"), rule);
        }
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
                sync_contents: false,
                verbose: true,
                remove_source_files: true,
                bwlimit: Some("85M".into()),
                // The middle rule is the point: a space inside a pattern must
                // survive the round trip as one rule, not split into two.
                excludes: vec!["*.tmp".into(), "My Documents/".into(), ".git".into()],
                extra_args: vec!["--partial".into()],
            },
            Profile {
                name: "Mirror strict".into(),
                delete: true,
                sync_contents: true,
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

    /// `;` is the KeyFile list separator, so a rule containing one is exactly
    /// what a delimited encoding would mangle. Pins that our per-key encoding
    /// does not care.
    #[test]
    fn a_semicolon_in_a_rule_survives_the_encoding() {
        let path = std::env::temp_dir().join(format!("foresight-semi-{}.ini", std::process::id()));
        let originals = vec![Profile {
            name: "odd".into(),
            excludes: vec!["weird;name".into(), "b".into()],
            ..Profile::default()
        }];

        save_all_to(&originals, &path);
        assert_eq!(load_from(&path)[0].excludes, vec!["weird;name", "b"]);

        let _ = std::fs::remove_file(&path);
    }

    /// Presets written before the rules editor stored excludes space-joined
    /// under `excludes`. Those must still load, or upgrading silently drops
    /// every rule a user had saved.
    #[test]
    fn presets_from_before_the_rules_editor_still_load() {
        let path =
            std::env::temp_dir().join(format!("foresight-legacy-{}.ini", std::process::id()));
        std::fs::write(
            &path,
            "[Old preset]\ndelete=false\nverbose=true\nbwlimit=\nexcludes=*.tmp .git\nextra_args=--partial\n",
        )
        .unwrap();

        let loaded = load_from(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].excludes, vec!["*.tmp", ".git"]);
        assert_eq!(loaded[0].extra_args, vec!["--partial"]);
        assert!(loaded[0].verbose);

        // Re-saving migrates it to the list encoding, and it still round-trips.
        save_all_to(&loaded, &path);
        assert_eq!(load_from(&path)[0].excludes, vec!["*.tmp", ".git"]);

        let _ = std::fs::remove_file(&path);
    }
}
