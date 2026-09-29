//! Saved presets — reusable sets of the *Advanced* rsync options (not the
//! source/destination, which need fresh portal grants each session).
//!
//! Persisted as a small `glib::KeyFile` under the app's config dir, which
//! inside the Flatpak is `~/.var/app/<app-id>/config/foresight/profiles.ini` —
//! writable and durable, no portal needed. One group per preset, keyed by
//! index rather than by name; see [`GROUP_PREFIX`] for why that matters.
//!
//! Extra arguments are stored space-joined, matching how the UI tokenises that
//! field. Filter rules are *not*: a rule may contain spaces (`My Documents/`),
//! and space-joining would silently split it into two wrong rules on the next
//! load. Each rule gets its own key instead — see [`FILTER_KEY_PREFIX`].
//!
//! Three rule encodings have shipped, and [`load_from`] reads all of them so no
//! upgrade silently drops a user's rules: the current include/exclude pair, the
//! v0.1.2 exclude-only list, and the pre-editor space-joined field.

use crate::job::{FilterKind, FilterRule};
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
    /// Filter rules in order — order is part of the preset, not incidental.
    pub filters: Vec<FilterRule>,
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

/// Prefix for the per-rule pattern keys: `filter_0`, `filter_1`, … — one key
/// each rather than a single delimited value.
///
/// A delimited list is the obvious encoding, but glib-rs binds
/// `KeyFile::string_list` without a matching `set_string_list`, and writing one
/// by hand means reproducing the `;`-escaping `g_key_file_set_string_list`
/// performs — glib internals we would be guessing at across versions. One key
/// per rule removes the separator from the problem entirely: `set_string`
/// already escapes anything a rule can hold, `;` and spaces included.
///
/// Each version of this encoding got a **new prefix** rather than new values
/// under the old one, so [`load_from`] can tell them apart outright instead of
/// inferring a format from the shape of a value.
const FILTER_KEY_PREFIX: &str = "filter_";
/// Suffix pairing a rule's kind with its pattern: `filter_0` / `filter_0_kind`.
/// Holds [`FilterKind::as_key`], never the display label.
const FILTER_KIND_SUFFIX: &str = "_kind";
/// The v0.1.2 per-rule key. Every rule it can hold is an exclude — include
/// rules did not exist yet.
const LEGACY_EXCLUDE_KEY_PREFIX: &str = "exclude_";
/// The pre-editor key, still read so the oldest presets keep their rules.
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

/// Read `filter_0`, `filter_1`, … (each with its `_kind`) until one is missing.
/// `save_all_to` writes a fresh KeyFile every time, so the run is always
/// contiguous — a gap can only mean the end.
///
/// A pattern whose `_kind` is missing reads as an exclude via
/// [`FilterKind::from_key`], so a half-written group degrades to the safer rule
/// rather than to an include that would let files through.
fn read_filter_rules(key_file: &KeyFile, group: &str) -> Vec<FilterRule> {
    let mut rules = Vec::new();
    while let Ok(pattern) = key_file.string(group, &format!("{FILTER_KEY_PREFIX}{}", rules.len())) {
        let kind_key = format!("{FILTER_KEY_PREFIX}{}{FILTER_KIND_SUFFIX}", rules.len());
        let kind = key_file
            .string(group, &kind_key)
            .map(|k| FilterKind::from_key(&k))
            .unwrap_or_default();
        rules.push(FilterRule {
            kind,
            pattern: pattern.to_string(),
        });
    }
    rules
}

/// Read the v0.1.2 `exclude_0`, `exclude_1`, … run. Every entry is an exclude.
fn read_legacy_exclude_rules(key_file: &KeyFile, group: &str) -> Vec<FilterRule> {
    let mut rules = Vec::new();
    while let Ok(pattern) = key_file.string(
        group,
        &format!("{LEGACY_EXCLUDE_KEY_PREFIX}{}", rules.len()),
    ) {
        rules.push(FilterRule::exclude(pattern.to_string()));
    }
    rules
}

/// Why a presets file that is there did not load. Carries the path for the
/// same reason [`SaveError`] does, and because the first thing anyone will want
/// to do with a file that cannot be read is go and look at it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// The file could not be opened or read: permissions, an I/O error.
    Read { path: PathBuf, reason: String },
    /// The file was read, and is not a key file: corrupt, or cut short.
    Parse { path: PathBuf, reason: String },
}

impl LoadError {
    /// The file that did not load.
    pub fn path(&self) -> &Path {
        match self {
            Self::Read { path, .. } | Self::Parse { path, .. } => path,
        }
    }
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read { path, reason } => {
                write!(f, "could not read “{}”: {reason}", path.display())
            }
            Self::Parse { path, reason } => {
                write!(
                    f,
                    "“{}” is not a valid presets file: {reason}",
                    path.display()
                )
            }
        }
    }
}

/// Load every saved preset.
///
/// No file at all is the first-run state and loads as an empty list. A file
/// that is there and does not load is an `Err`, never an empty list: the two
/// used to be indistinguishable, and the next save — which replaces the whole
/// file with the list in memory — then wrote an almost empty list over every
/// preset the file held.
pub fn load() -> Result<Vec<Profile>, LoadError> {
    load_from(&profiles_path())
}

fn load_from(path: &Path) -> Result<Vec<Profile>, LoadError> {
    let key_file = KeyFile::new();
    if let Err(e) = key_file.load_from_file(path, KeyFileFlags::NONE) {
        // Only "there is no such file" means there are no presets. A GKeyFile
        // parse error rejects the file as a whole, so there is no usable part
        // of it to return alongside the error.
        if e.matches(glib::FileError::Noent) {
            return Ok(Vec::new());
        }
        let (path, reason) = (path.to_path_buf(), e.message().to_string());
        return Err(if e.is::<glib::KeyFileError>() {
            LoadError::Parse { path, reason }
        } else {
            LoadError::Read { path, reason }
        });
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
            // Newest encoding wins; fall back through the two older ones so an
            // upgrade never quietly discards the rules a user saved.
            filters: match read_filter_rules(&key_file, &group) {
                rules if !rules.is_empty() => rules,
                _ => match read_legacy_exclude_rules(&key_file, &group) {
                    rules if !rules.is_empty() => rules,
                    // Pre-editor preset: the only encoding it ever had was
                    // space-joined, so splitting is exact, not a heuristic.
                    _ => split(&get(LEGACY_EXCLUDES_KEY))
                        .into_iter()
                        .map(FilterRule::exclude)
                        .collect(),
                },
            },
            extra_args: split(&get("extra_args")),
            name,
        });
    }
    Ok(out)
}

/// Why the presets did not reach disk. Carries the path because inside the
/// Flatpak it is not where a user would think to look, and "permission denied"
/// with no path is not something anyone can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveError {
    /// The config directory is missing and could not be created.
    CreateDir { dir: PathBuf, reason: String },
    /// The directory is there; writing the file into it failed.
    Write { path: PathBuf, reason: String },
    /// The file on disk cannot be read, and could not be moved out of the way
    /// either. Nothing was written: the save stops here rather than replace it.
    MoveAside {
        path: PathBuf,
        to: PathBuf,
        reason: String,
    },
    /// The unreadable file was moved aside, the write then failed, and the
    /// file could not be put back. The one error after which the file is not
    /// where it was, so it says where it is.
    WriteAfterMove {
        path: PathBuf,
        moved_to: PathBuf,
        reason: String,
    },
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CreateDir { dir, reason } => {
                write!(f, "could not create “{}”: {reason}", dir.display())
            }
            Self::Write { path, reason } => {
                write!(f, "could not write “{}”: {reason}", path.display())
            }
            Self::MoveAside { path, to, reason } => write!(
                f,
                "“{}” cannot be read, and could not be moved aside to “{}” \
                 before replacing it: {reason}",
                path.display(),
                to.display()
            ),
            Self::WriteAfterMove {
                path,
                moved_to,
                reason,
            } => write!(
                f,
                "could not write “{}”: {reason}. The unreadable file that was \
                 there is now at “{}”",
                path.display(),
                moved_to.display()
            ),
        }
    }
}

/// What a save did besides writing the presets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Saved {
    /// Where the file that was on disk went, if it could not be read and was
    /// moved out of the way instead of being replaced.
    pub moved_aside: Option<PathBuf>,
}

/// Marks a presets file that was moved aside unread; a timestamp follows it.
const UNREADABLE_SUFFIX: &str = ".unreadable-";

/// How many names to try for the moved file before giving up. Only a second
/// unreadable file within the same second needs more than one.
const ASIDE_ATTEMPTS: u32 = 100;

/// `profiles.ini.unreadable-<stamp>` beside `path`, or the first of
/// `…-<stamp>-2`, `-3`, … that is free. A rename replaces its target without
/// asking, and the target here would be an earlier file put aside for keeping.
fn aside_path(path: &Path, stamp: &str) -> Option<PathBuf> {
    let mut base = path.as_os_str().to_os_string();
    base.push(UNREADABLE_SUFFIX);
    base.push(stamp);
    (1..=ASIDE_ATTEMPTS)
        .map(|n| {
            let mut name = base.clone();
            if n > 1 {
                name.push(format!("-{n}"));
            }
            PathBuf::from(name)
        })
        // symlink_metadata, not exists(): a dangling link is still a name in use.
        .find(|candidate| candidate.symlink_metadata().is_err())
}

/// If the file at `path` is there but does not load, move it to a sibling
/// name and say which. `Ok(None)` when there was nothing to protect.
///
/// The choice was between this and refusing to save until the file is fixed.
/// Refusing is simpler, but it leaves presets unusable until the user repairs
/// a file by hand, in a directory they have likely never seen, for a reason
/// that may be one bad byte. Moving it aside keeps every byte of it — a rename
/// does not read or rewrite the file, so it works on one that cannot even be
/// opened — and lets the save go ahead.
///
/// The test is made against the disk at the moment of the save, not
/// remembered from startup, so a create, an update and a delete are all
/// covered by being saves, and so is a file that went bad while the app was
/// running.
///
/// Only a regular file is moved. Anything else on the name (a directory, say)
/// holds no presets to lose, and is left for the write to fail on as before.
fn move_unreadable_aside(path: &Path) -> Result<Option<PathBuf>, SaveError> {
    if load_from(path).is_ok() || !path.metadata().is_ok_and(|m| m.is_file()) {
        return Ok(None);
    }
    let stamp = glib::DateTime::now_local()
        .and_then(|now| now.format("%Y%m%d-%H%M%S"))
        .map(|s| s.to_string())
        .unwrap_or_else(|_| "undated".to_string());
    let failed = |to: PathBuf, reason: String| SaveError::MoveAside {
        path: path.to_path_buf(),
        to,
        reason,
    };
    let Some(to) = aside_path(path, &stamp) else {
        let mut taken = path.as_os_str().to_os_string();
        taken.push(UNREADABLE_SUFFIX);
        taken.push(&stamp);
        return Err(failed(
            PathBuf::from(taken),
            "that name and every numbered variant of it is taken".to_string(),
        ));
    };
    match std::fs::rename(path, &to) {
        Ok(()) => Ok(Some(to)),
        Err(e) => Err(failed(to, e.to_string())),
    }
}

/// Persist the full set of presets, replacing whatever was on disk — unless
/// what was on disk could not be read, in which case it is moved aside first
/// (see [`move_unreadable_aside`]) and [`Saved`] says where to.
///
/// An `Err` means the file is as it was before the call: `save_to_file` writes
/// a temporary beside the target and renames it over, so a failed save never
/// leaves half a file, and a file moved aside for a save that then fails is
/// moved back. Callers rely on that to keep their own list honest. The one
/// exception names itself: [`SaveError::WriteAfterMove`].
pub fn save_all(profiles: &[Profile]) -> Result<Saved, SaveError> {
    save_all_to(profiles, &profiles_path())
}

fn save_all_to(profiles: &[Profile], path: &Path) -> Result<Saved, SaveError> {
    protecting(path, || write_all_to(profiles, path))
}

/// Run `write` with the file at `path` protected: moved aside first if it
/// cannot be read, and moved back if `write` then fails. Apart from
/// [`save_all_to`] so the second half can be tested with a write that fails
/// on demand.
fn protecting(
    path: &Path,
    write: impl FnOnce() -> Result<(), SaveError>,
) -> Result<Saved, SaveError> {
    // Before anything else: if this fails, nothing may be written.
    let moved_aside = move_unreadable_aside(path)?;
    match write() {
        Ok(()) => Ok(Saved { moved_aside }),
        Err(e) => Err(match moved_aside {
            None => e,
            // Put it back, so that a failed save changes nothing at all. The
            // name is free: the write that would have taken it just failed.
            Some(moved_to) => match std::fs::rename(&moved_to, path) {
                Ok(()) => e,
                Err(_) => SaveError::WriteAfterMove {
                    path: path.to_path_buf(),
                    moved_to,
                    reason: match e {
                        SaveError::CreateDir { reason, .. } | SaveError::Write { reason, .. } => {
                            reason
                        }
                        other => other.to_string(),
                    },
                },
            },
        }),
    }
}

/// The write itself. What it puts on disk is the frozen 1.0 format.
fn write_all_to(profiles: &[Profile], path: &Path) -> Result<(), SaveError> {
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
        for (i, rule) in p.filters.iter().enumerate() {
            key_file.set_string(&group, &format!("{FILTER_KEY_PREFIX}{i}"), &rule.pattern);
            key_file.set_string(
                &group,
                &format!("{FILTER_KEY_PREFIX}{i}{FILTER_KIND_SUFFIX}"),
                rule.kind.as_key(),
            );
        }
        key_file.set_string(&group, "extra_args", &p.extra_args.join(" "));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| SaveError::CreateDir {
            dir: dir.to_path_buf(),
            reason: e.to_string(),
        })?;
    }
    key_file.save_to_file(path).map_err(|e| SaveError::Write {
        path: path.to_path_buf(),
        reason: e.message().to_string(),
    })
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
                filters: vec![
                    FilterRule::exclude("*.tmp"),
                    FilterRule::include("My Documents/"),
                    FilterRule::exclude(".git"),
                ],
                extra_args: vec!["--partial".into()],
            },
            Profile {
                name: "Mirror strict".into(),
                delete: true,
                sync_contents: true,
                verbose: false,
                remove_source_files: false,
                bwlimit: None,
                filters: vec![],
                extra_args: vec![],
            },
        ];

        save_all_to(&originals, &path).unwrap();
        let mut loaded = load_from(&path).unwrap();
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
            filters: vec![FilterRule::exclude("weird;name"), FilterRule::include("b")],
            ..Profile::default()
        }];

        save_all_to(&originals, &path).unwrap();
        assert_eq!(
            load_from(&path).unwrap()[0].filters,
            vec![FilterRule::exclude("weird;name"), FilterRule::include("b")]
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Kind travels with its pattern, and the list order survives — both are
    /// meaning, not presentation: rsync stops at the first rule that matches, so
    /// a reordered or re-kinded preset would sync a different set of files.
    #[test]
    fn rule_kinds_and_their_order_round_trip() {
        let path = std::env::temp_dir().join(format!("foresight-kinds-{}.ini", std::process::id()));
        let rules = vec![
            FilterRule::include("*/"),
            FilterRule::include("*.jpg"),
            FilterRule::exclude("build/"),
            FilterRule::exclude("*"),
        ];
        let p = Profile {
            name: "only jpegs".into(),
            filters: rules.clone(),
            ..Default::default()
        };

        save_all_to(std::slice::from_ref(&p), &path).unwrap();
        assert_eq!(load_from(&path).unwrap()[0].filters, rules);

        let _ = std::fs::remove_file(&path);
    }

    /// Presets written by v0.1.2 stored one key per rule under `exclude_N`, and
    /// every rule it could express was an exclude. Those must load as excludes —
    /// reading them as includes would invert the preset and copy exactly the
    /// files the user had been skipping.
    #[test]
    fn presets_from_v0_1_2_load_as_exclude_rules() {
        let path = std::env::temp_dir().join(format!("foresight-v012-{}.ini", std::process::id()));
        std::fs::write(
            &path,
            "[preset_0]\nname=HDD move\ndelete=false\nverbose=true\nbwlimit=85M\n\
             exclude_0=*.tmp\nexclude_1=My Documents/\nexclude_2=.git\nextra_args=--partial\n",
        )
        .unwrap();

        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "HDD move");
        assert_eq!(
            loaded[0].filters,
            vec![
                FilterRule::exclude("*.tmp"),
                FilterRule::exclude("My Documents/"),
                FilterRule::exclude(".git"),
            ],
            "every pre-include-rules rule is an exclude"
        );

        // Re-saving migrates it to the kinded encoding without changing meaning.
        save_all_to(&loaded, &path).unwrap();
        assert_eq!(load_from(&path).unwrap()[0].filters, loaded[0].filters);

        let _ = std::fs::remove_file(&path);
    }

    /// A pattern whose `_kind` key is missing (a truncated or hand-edited file)
    /// must read as the conservative kind. An unreadable rule that defaulted to
    /// *include* would silently widen a filter set meant to restrict.
    #[test]
    fn a_rule_with_no_recorded_kind_reads_as_exclude() {
        let path =
            std::env::temp_dir().join(format!("foresight-nokind-{}.ini", std::process::id()));
        std::fs::write(
            &path,
            "[preset_0]\nname=truncated\nfilter_0=*.tmp\nfilter_1=*.jpg\nfilter_1_kind=include\n",
        )
        .unwrap();

        assert_eq!(
            load_from(&path).unwrap()[0].filters,
            vec![FilterRule::exclude("*.tmp"), FilterRule::include("*.jpg")]
        );

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

        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded[0].filters,
            vec![FilterRule::exclude("*.tmp"), FilterRule::exclude(".git")]
        );
        assert_eq!(loaded[0].extra_args, vec!["--partial"]);
        assert!(loaded[0].verbose);

        // Re-saving migrates it to the list encoding, and it still round-trips.
        save_all_to(&loaded, &path).unwrap();
        assert_eq!(
            load_from(&path).unwrap()[0].filters,
            vec![FilterRule::exclude("*.tmp"), FilterRule::exclude(".git")]
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Every rule shape a user can actually type must survive storage. `=` is
    /// the KeyFile key/value separator and `\\` its escape character, so these
    /// are the encodings most likely to corrupt a value silently.
    #[test]
    fn rules_containing_keyfile_metacharacters_round_trip() {
        let path = std::env::temp_dir().join(format!("foresight-meta-{}.ini", std::process::id()));
        // Alternating kinds so the `_kind` keys are exercised beside patterns
        // that could corrupt the key/value encoding.
        let rules: Vec<FilterRule> = [
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
        .enumerate()
        .map(|(i, s)| {
            if i % 2 == 0 {
                FilterRule::exclude(*s)
            } else {
                FilterRule::include(*s)
            }
        })
        .collect();
        let p = Profile {
            name: "meta".into(),
            filters: rules.clone(),
            ..Default::default()
        };

        save_all_to(std::slice::from_ref(&p), &path).unwrap();
        assert_eq!(load_from(&path).unwrap()[0].filters, rules);

        let _ = std::fs::remove_file(&path);
    }

    /// Rule order is load-bearing — rsync applies filter rules in order, so the
    /// first match wins. Indices must not be read back lexicographically
    /// (`exclude_10` before `exclude_2`).
    #[test]
    fn many_rules_keep_their_order() {
        let path = std::env::temp_dir().join(format!("foresight-order-{}.ini", std::process::id()));
        let rules: Vec<FilterRule> = (0..150)
            .map(|i| FilterRule::exclude(format!("rule{i}")))
            .collect();
        let p = Profile {
            name: "many".into(),
            filters: rules.clone(),
            ..Default::default()
        };

        save_all_to(std::slice::from_ref(&p), &path).unwrap();
        assert_eq!(load_from(&path).unwrap()[0].filters, rules);

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
                filters: vec![FilterRule::exclude(format!("{n}-rule"))],
                ..Default::default()
            })
            .collect();

        save_all_to(&originals, &path).unwrap();
        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.len(), names.len(), "every preset must survive");
        for (want, got) in originals.iter().zip(loaded.iter()) {
            assert_eq!(got.name, want.name);
            assert_eq!(got.filters, want.filters, "rules must follow their preset");
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

        save_all_to(&originals, &path).unwrap();
        let got: Vec<String> = load_from(&path)
            .unwrap()
            .iter()
            .map(|p| p.name.clone())
            .collect();
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

        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(
            loaded[0].name, "HDD move",
            "group name is the preset name in the old layout"
        );
        assert_eq!(
            loaded[0].filters,
            vec![FilterRule::exclude("*.tmp"), FilterRule::exclude(".git")]
        );
        assert!(loaded[0].delete);

        // Re-saving migrates it; the name survives the move into a value.
        save_all_to(&loaded, &path).unwrap();
        let again = load_from(&path).unwrap();
        assert_eq!(again[0].name, "HDD move");
        assert_eq!(
            again[0].filters,
            vec![FilterRule::exclude("*.tmp"), FilterRule::exclude(".git")]
        );

        let _ = std::fs::remove_file(&path);
    }

    /// The bug this guards: both errors used to be discarded, so the window
    /// announced a save that never happened. A regular file where the config
    /// directory should be fails for root as well, which a read-only directory
    /// would not.
    #[test]
    fn a_config_dir_that_cannot_be_created_is_an_error() {
        let blocker = std::env::temp_dir().join(format!("foresight-nodir-{}", std::process::id()));
        std::fs::write(&blocker, "not a directory").unwrap();
        let path = blocker.join("foresight").join("profiles.ini");

        let result = save_all_to(&[Profile::default()], &path);
        assert!(
            matches!(&result, Err(SaveError::CreateDir { dir, .. }) if dir == path.parent().unwrap()),
            "{result:?}"
        );
        // What the user is shown has to say where, not only what.
        let shown = result.unwrap_err().to_string();
        assert!(shown.contains(&blocker.display().to_string()), "{shown}");

        let _ = std::fs::remove_file(&blocker);
    }

    /// The directory exists but the file cannot be replaced — here because a
    /// directory is sitting on its name. What was on disk must be untouched,
    /// and no temporary may be left beside it.
    #[test]
    fn a_file_that_cannot_be_written_is_an_error() {
        let dir = std::env::temp_dir().join(format!("foresight-nowrite-{}", std::process::id()));
        let path = dir.join("profiles.ini");
        std::fs::create_dir_all(&path).unwrap();

        let result = save_all_to(&[Profile::default()], &path);
        assert!(
            matches!(&result, Err(SaveError::Write { path: p, .. }) if *p == path),
            "{result:?}"
        );
        assert!(path.is_dir());
        let left_behind: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .filter(|n| n != "profiles.ini")
            .collect();
        assert!(left_behind.is_empty(), "{left_behind:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A failed save leaves the previous file readable and unchanged, which is
    /// what lets the window keep its list as it was.
    #[test]
    fn a_failed_save_leaves_the_previous_presets_on_disk() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!("foresight-keep-{}", std::process::id()));
        let path = dir.join("profiles.ini");
        let _ = std::fs::remove_dir_all(&dir);
        let before = vec![Profile {
            name: "Kept".into(),
            ..Profile::default()
        }];
        save_all_to(&before, &path).unwrap();

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        // Root writes through a read-only directory, so there is nothing to
        // observe; say so rather than fail on a machine that is not broken.
        let enforced = std::fs::write(dir.join("probe"), "").is_err();
        let result = save_all_to(&[], &path);
        let after = load_from(&path).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        if !enforced {
            eprintln!("skipped: directory permissions are not enforced for this user");
            return;
        }
        assert!(matches!(result, Err(SaveError::Write { .. })), "{result:?}");
        assert_eq!(after, before);
    }

    /// A directory of its own per test, so a test can assert on everything
    /// that is in it — which is how a stray file would be noticed.
    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("foresight-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// A preset a key file could hold, then a line it cannot. Not UTF-8
    /// either, so the tests that compare it do so as bytes.
    const GARBAGE: &[u8] = b"[preset_0]\nname=Years of work\ndelete=true\nnot a key file \xff\n";

    /// First run: nothing to read is not something to report.
    #[test]
    fn a_missing_file_is_an_empty_list_and_not_an_error() {
        let dir = scratch_dir("missing");
        assert_eq!(load_from(&dir.join("profiles.ini")), Ok(Vec::new()));
        // Nor is a missing directory, which is what a first run really has.
        assert_eq!(
            load_from(&dir.join("foresight").join("profiles.ini")),
            Ok(Vec::new())
        );
        // A save onto nothing has nothing to move aside.
        let saved = save_all_to(&[Profile::default()], &dir.join("profiles.ini")).unwrap();
        assert_eq!(saved, Saved { moved_aside: None });
        assert_eq!(names_in(&dir), ["profiles.ini"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file with nothing in it parses, and holds no presets: there is
    /// nothing in it to protect, so it is not an error and is not moved.
    #[test]
    fn an_empty_file_is_an_empty_list() {
        let dir = scratch_dir("empty");
        let path = dir.join("profiles.ini");
        std::fs::write(&path, "").unwrap();

        assert_eq!(load_from(&path), Ok(Vec::new()));
        let saved = save_all_to(&[Profile::default()], &path).unwrap();
        assert_eq!(saved.moved_aside, None);
        assert_eq!(names_in(&dir), ["profiles.ini"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The bug this guards: a file that did not parse loaded as "no presets",
    /// and the next save replaced it. One bad line fails the whole file — the
    /// preset above it is not returned — so the whole file is what is at stake.
    #[test]
    fn a_file_that_does_not_parse_is_an_error_and_is_not_touched() {
        let dir = scratch_dir("garbage");
        let path = dir.join("profiles.ini");
        std::fs::write(&path, GARBAGE).unwrap();

        let result = load_from(&path);
        assert!(
            matches!(&result, Err(LoadError::Parse { path: p, .. }) if *p == path),
            "{result:?}"
        );
        let error = result.unwrap_err();
        assert_eq!(error.path(), path);
        assert!(error.to_string().contains(&path.display().to_string()));
        assert_eq!(std::fs::read(&path).unwrap(), GARBAGE);
        assert_eq!(names_in(&dir), ["profiles.ini"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file cut short in the middle of a line, as an interrupted copy or a
    /// full disk leaves it.
    #[test]
    fn a_truncated_file_is_an_error() {
        let dir = scratch_dir("truncated");
        let path = dir.join("profiles.ini");
        let cut = b"[preset_0]\nname=A\nfilter_0=*.tmp\nfilter_0_ki";
        std::fs::write(&path, cut).unwrap();

        let result = load_from(&path);
        assert!(matches!(result, Err(LoadError::Parse { .. })), "{result:?}");
        assert_eq!(std::fs::read(&path).unwrap(), cut);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_cannot_be_opened_is_an_error() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch_dir("noread");
        let path = dir.join("profiles.ini");
        save_all_to(
            &[Profile {
                name: "Locked away".into(),
                ..Profile::default()
            }],
            &path,
        )
        .unwrap();
        let bytes = std::fs::read(&path).unwrap();

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads through the mode bits, so there is nothing to observe.
        let enforced = std::fs::read(&path).is_err();
        let loaded = load_from(&path);
        // The protection does not need to read the file it protects.
        let saved = save_all_to(&[Profile::default()], &path);
        let moved = saved.as_ref().ok().and_then(|s| s.moved_aside.clone());
        if let Some(moved) = &moved {
            std::fs::set_permissions(moved, std::fs::Permissions::from_mode(0o644)).unwrap();
        } else {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
        let kept = moved.as_ref().map(|m| std::fs::read(m).unwrap());
        let _ = std::fs::remove_dir_all(&dir);

        if !enforced {
            eprintln!("skipped: file permissions are not enforced for this user");
            return;
        }
        assert!(
            matches!(&loaded, Err(LoadError::Read { path: p, .. }) if *p == path),
            "{loaded:?}"
        );
        assert!(saved.is_ok(), "{saved:?}");
        assert_eq!(kept, Some(bytes));
    }

    /// The protection itself: the save goes through, and every byte of the
    /// file it would have destroyed is still on disk, under a name that says
    /// what it is.
    #[test]
    fn an_unreadable_file_is_moved_aside_before_a_save_replaces_it() {
        let dir = scratch_dir("aside");
        let path = dir.join("profiles.ini");
        std::fs::write(&path, GARBAGE).unwrap();
        let fresh = vec![Profile {
            name: "New".into(),
            ..Profile::default()
        }];

        let saved = save_all_to(&fresh, &path).unwrap();
        let moved = saved.moved_aside.expect("the file was not moved aside");
        assert_eq!(moved.parent(), path.parent());
        let name = moved.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("profiles.ini.unreadable-"), "{name}");
        assert_eq!(std::fs::read(&moved).unwrap(), GARBAGE);
        assert_eq!(load_from(&path), Ok(fresh.clone()));
        assert_eq!(names_in(&dir), [String::from("profiles.ini"), name]);

        // What is there now is ours, so the next save moves nothing.
        assert_eq!(save_all_to(&[], &path).unwrap().moved_aside, None);
        assert_eq!(names_in(&dir).len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A save that empties the list — deleting the only preset — is still a
    /// save, and gets the same protection as one that adds.
    #[test]
    fn saving_an_empty_list_protects_the_file_too() {
        let dir = scratch_dir("aside-empty");
        let path = dir.join("profiles.ini");
        std::fs::write(&path, GARBAGE).unwrap();

        let moved = save_all_to(&[], &path).unwrap().moved_aside.unwrap();
        assert_eq!(std::fs::read(&moved).unwrap(), GARBAGE);
        assert_eq!(load_from(&path), Ok(Vec::new()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two files put aside within the same second must both survive: a rename
    /// onto a name in use would replace the first with the second.
    #[test]
    fn a_file_moved_aside_never_replaces_an_earlier_one() {
        let dir = scratch_dir("aside-twice");
        let path = dir.join("profiles.ini");

        let mut kept = Vec::new();
        for contents in ["first ruin\n", "second ruin\n", "third ruin\n"] {
            std::fs::write(&path, contents).unwrap();
            let moved = save_all_to(&[], &path).unwrap().moved_aside.unwrap();
            kept.push((moved, contents));
        }
        for (moved, contents) in &kept {
            assert_eq!(std::fs::read_to_string(moved).unwrap(), *contents);
        }
        assert_eq!(names_in(&dir).len(), 4, "{:?}", names_in(&dir));

        // The naming on its own, where the clock cannot blur it.
        std::fs::write(dir.join("p.unreadable-S"), "").unwrap();
        std::os::unix::fs::symlink("nowhere", dir.join("p.unreadable-S-2")).unwrap();
        assert_eq!(
            aside_path(&dir.join("p"), "S"),
            Some(dir.join("p.unreadable-S-3"))
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// If the file cannot be moved out of the way, the save does not happen.
    #[test]
    fn a_file_that_cannot_be_moved_aside_stops_the_save() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch_dir("aside-blocked");
        let path = dir.join("profiles.ini");
        std::fs::write(&path, GARBAGE).unwrap();

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let enforced = std::fs::write(dir.join("probe"), "").is_err();
        let result = save_all_to(&[Profile::default()], &path);
        let (bytes, names) = (std::fs::read(&path), names_in(&dir));
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        if !enforced {
            eprintln!("skipped: directory permissions are not enforced for this user");
            return;
        }
        assert!(
            matches!(&result, Err(SaveError::MoveAside { path: p, to, .. })
                if *p == path && to.parent() == path.parent()),
            "{result:?}"
        );
        let shown = result.unwrap_err().to_string();
        assert!(shown.contains(&path.display().to_string()), "{shown}");
        assert_eq!(bytes.unwrap(), GARBAGE);
        assert_eq!(names, ["profiles.ini"]);
    }

    /// Moved aside, and then the write fails: the file goes back where it
    /// was, so a failed save has still changed nothing.
    #[test]
    fn a_failed_write_puts_the_file_back() {
        let dir = scratch_dir("aside-back");
        let path = dir.join("profiles.ini");
        std::fs::write(&path, GARBAGE).unwrap();

        let failure = SaveError::Write {
            path: path.clone(),
            reason: "staged".into(),
        };
        let mut seen_during_write = Vec::new();
        let result = protecting(&path, || {
            seen_during_write = names_in(&dir);
            Err(failure.clone())
        });

        // It really had been moved when the write was attempted…
        assert_eq!(seen_during_write.len(), 1, "{seen_during_write:?}");
        assert!(seen_during_write[0].starts_with("profiles.ini.unreadable-"));
        // …and is back, whole, with the write's own error reported.
        assert_eq!(result, Err(failure));
        assert_eq!(std::fs::read(&path).unwrap(), GARBAGE);
        assert_eq!(names_in(&dir), ["profiles.ini"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Something on the file's name that is not a file holds no presets. It is
    /// reported when loading and left alone when saving, where the write fails
    /// on it as it always did.
    #[test]
    fn a_directory_on_the_files_name_is_not_moved_aside() {
        let dir = scratch_dir("aside-dir");
        let path = dir.join("profiles.ini");
        std::fs::create_dir(&path).unwrap();

        assert!(load_from(&path).is_err());
        assert_eq!(move_unreadable_aside(&path), Ok(None));
        assert!(path.is_dir());
        assert_eq!(names_in(&dir), ["profiles.ini"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What loading tolerates today, pinned so that reporting errors did not
    /// quietly make it stricter: groups and keys no reader knows, comments,
    /// and values that are not what their key expects all still load.
    #[test]
    fn a_file_that_parses_loads_as_leniently_as_it_always_did() {
        let dir = scratch_dir("lenient");
        let path = dir.join("profiles.ini");
        std::fs::write(
            &path,
            "# written by hand\n\
             [preset_0]\nname=A\ndelete=yes\nverbose=true\nfuture_key=42\n\
             filter_0=a\nfilter_0_kind=protect\nfilter_2=unreached\n\n\
             [settings]\ntheme=dark\n",
        )
        .unwrap();

        assert_eq!(
            load_from(&path),
            Ok(vec![
                Profile {
                    name: "A".into(),
                    verbose: true,
                    filters: vec![FilterRule::exclude("a")],
                    ..Profile::default()
                },
                Profile {
                    name: "settings".into(),
                    ..Profile::default()
                },
            ])
        );
        // It loaded, so a save replaces it in place.
        let saved = save_all_to(&load_from(&path).unwrap(), &path).unwrap();
        assert_eq!(saved.moved_aside, None);
        assert_eq!(names_in(&dir), ["profiles.ini"]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
