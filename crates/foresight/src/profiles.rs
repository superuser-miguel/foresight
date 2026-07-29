//! Saved presets — reusable sets of the *Advanced* rsync options (not the
//! source/destination, which need fresh portal grants each session).
//!
//! Persisted as a small `glib::KeyFile` under the app's config dir, which
//! inside the Flatpak is `~/.var/app/<app-id>/config/foresight/profiles.ini` —
//! writable and durable, no portal needed. One group per preset, keyed by
//! index rather than by name; see [`GROUP_PREFIX`] for why that matters.
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

/// Groups are synthetic (`preset_0`, `preset_1`, …) rather than the preset's
/// own name.
///
/// A GKeyFile group name may not contain `[`, `]`, a tab or a newline, and may
/// not be empty: `g_key_file_set_value` rejects one with a CRITICAL and writes
/// *nothing*. While the name was the group, a preset called `Photos [raw]`
/// therefore vanished on save — silently, because the UI had already added it
/// to the combo and toasted success, so it looked saved until the next launch.
/// Keeping the display name in a value removes the restriction entirely.
const GROUP_PREFIX: &str = "preset_";
/// Key holding a preset's display name inside its group.
const NAME_KEY: &str = "name";

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
        let group = group.to_string();
        // New layout keeps the display name in a value; the old one used the
        // group name itself, which is exactly why it could not represent every
        // name. Falling back to the group keeps those presets loadable.
        let name = key_file
            .string(&group, NAME_KEY)
            .map(|s| s.to_string())
            .unwrap_or_else(|_| group.clone());
        let get = |k: &str| {
            key_file
                .string(&group, k)
                .map(|g| g.to_string())
                .unwrap_or_default()
        };
        let bw = get("bwlimit");
        out.push(Profile {
            delete: key_file.boolean(&group, "delete").unwrap_or(false),
            // Absent in presets saved before this option existed — those were
            // written when a lone folder always synced its contents, so `false`
            // (nest the folder) is the honest new default, not a silent change
            // of what the preset used to do to the *paths* it never stored.
            sync_contents: key_file.boolean(&group, "sync_contents").unwrap_or(false),
            verbose: key_file.boolean(&group, "verbose").unwrap_or(false),
            remove_source_files: key_file.boolean(&group, "move").unwrap_or(false),
            bwlimit: (!bw.is_empty()).then_some(bw),
            excludes: match read_exclude_rules(&key_file, &group) {
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
    for (n, p) in profiles.iter().enumerate() {
        // Index, not name: see GROUP_PREFIX. Writing profiles in order also
        // means the group order on disk is the combo order.
        let group = format!("{GROUP_PREFIX}{n}");
        key_file.set_string(&group, NAME_KEY, &p.name);
        key_file.set_boolean(&group, "delete", p.delete);
        key_file.set_boolean(&group, "sync_contents", p.sync_contents);
        key_file.set_boolean(&group, "verbose", p.verbose);
        key_file.set_boolean(&group, "move", p.remove_source_files);
        key_file.set_string(&group, "bwlimit", p.bwlimit.as_deref().unwrap_or(""));
        for (i, rule) in p.excludes.iter().enumerate() {
            key_file.set_string(&group, &format!("{EXCLUDE_KEY_PREFIX}{i}"), rule);
        }
        key_file.set_string(&group, "extra_args", &p.extra_args.join(" "));
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

    /// Every rule shape a user can actually type must survive storage. `=` is
    /// the KeyFile key/value separator and `\\` its escape character, so these
    /// are the encodings most likely to corrupt a value silently.
    #[test]
    fn rules_containing_keyfile_metacharacters_round_trip() {
        let path = std::env::temp_dir().join(format!("foresight-meta-{}.ini", std::process::id()));
        let rules: Vec<String> = [
            "foo=bar",
            "a=b=c",
            "back\\slash",
            "x\ny",
            "t\tz",
            "  ",
            "üñî",
            "*.tmp",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let p = Profile {
            name: "meta".into(),
            excludes: rules.clone(),
            ..Default::default()
        };

        save_all_to(std::slice::from_ref(&p), &path);
        assert_eq!(load_from(&path)[0].excludes, rules);

        let _ = std::fs::remove_file(&path);
    }

    /// Rule order is load-bearing — rsync applies filter rules in order, so the
    /// first match wins. Indices must not be read back lexicographically
    /// (`exclude_10` before `exclude_2`).
    #[test]
    fn many_rules_keep_their_order() {
        let path = std::env::temp_dir().join(format!("foresight-order-{}.ini", std::process::id()));
        let rules: Vec<String> = (0..150).map(|i| format!("rule{i}")).collect();
        let p = Profile {
            name: "many".into(),
            excludes: rules.clone(),
            ..Default::default()
        };

        save_all_to(std::slice::from_ref(&p), &path);
        assert_eq!(load_from(&path)[0].excludes, rules);

        let _ = std::fs::remove_file(&path);
    }

    /// A preset name is user text, and GKeyFile group names cannot hold `[`,
    /// `]`, tab or newline. While the name *was* the group, saving one of these
    /// silently wrote nothing while the UI reported success — the preset was
    /// gone at next launch. Names now live in a value, so all of these persist.
    #[test]
    fn preset_names_that_a_keyfile_group_could_never_hold() {
        let path = std::env::temp_dir().join(format!("foresight-names-{}.ini", std::process::id()));
        let names = [
            "Photos [raw]",
            "a]b",
            "[bracketed]",
            "tab\there",
            "line\nbreak",
            "has=eq",
            "semi;colon",
            "#hash",
            " lead",
            "trail ",
            "üñî",
        ];
        let originals: Vec<Profile> = names
            .iter()
            .map(|n| Profile {
                name: (*n).into(),
                excludes: vec![format!("{n}-rule")],
                ..Default::default()
            })
            .collect();

        save_all_to(&originals, &path);
        let loaded = load_from(&path);
        assert_eq!(loaded.len(), names.len(), "every preset must survive");
        for (want, got) in originals.iter().zip(loaded.iter()) {
            assert_eq!(got.name, want.name);
            assert_eq!(
                got.excludes, want.excludes,
                "rules must follow their preset"
            );
        }

        let _ = std::fs::remove_file(&path);
    }

    /// One unstorable name used to take only itself down, but silently. Pins
    /// that a mixed set now round-trips whole.
    #[test]
    fn a_bracketed_name_does_not_cost_its_neighbours() {
        let path = std::env::temp_dir().join(format!("foresight-mixed-{}.ini", std::process::id()));
        let mk = |n: &str| Profile {
            name: n.into(),
            ..Default::default()
        };
        let originals = vec![mk("Good one"), mk("Photos [raw]"), mk("Good two")];

        save_all_to(&originals, &path);
        let got: Vec<String> = load_from(&path).iter().map(|p| p.name.clone()).collect();
        assert_eq!(got, vec!["Good one", "Photos [raw]", "Good two"]);

        let _ = std::fs::remove_file(&path);
    }

    /// Presets written before names moved out of the group name: the group IS
    /// the name, and there is no `name` key to read.
    #[test]
    fn presets_from_before_named_groups_still_load() {
        let path =
            std::env::temp_dir().join(format!("foresight-oldgrp-{}.ini", std::process::id()));
        std::fs::write(
            &path,
            "[HDD move]\ndelete=true\nverbose=true\nexcludes=*.tmp .git\n",
        )
        .unwrap();

        let loaded = load_from(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded[0].name, "HDD move",
            "group name is the preset name in the old layout"
        );
        assert_eq!(loaded[0].excludes, vec!["*.tmp", ".git"]);
        assert!(loaded[0].delete);

        // Re-saving migrates it; the name survives the move into a value.
        save_all_to(&loaded, &path);
        let again = load_from(&path);
        assert_eq!(again[0].name, "HDD move");
        assert_eq!(again[0].excludes, vec!["*.tmp", ".git"]);

        let _ = std::fs::remove_file(&path);
    }
}
