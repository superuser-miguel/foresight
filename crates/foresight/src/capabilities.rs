//! The capability registry — the single, honest source of truth for *which*
//! rsync flags Foresight actually exposes in this release.
//!
//! This table sits beside [`crate::job::build_argv`] on purpose: the Help
//! surface renders straight from it, and a test in this module asserts the
//! registry and `build_argv` agree exactly — every fixed flag the app can emit
//! has an entry here, and every entry maps to a flag the app can emit. Drift
//! becomes a test failure, so the Help can never lie about what the app does.
//!
//! The free-form *Extra arguments* field is deliberately **not** a registry
//! entry: it is the escape hatch for everything Foresight does not expose as a
//! dedicated control (see [`NOT_EXPOSED`]).

/// Where a capability shows up, for grouping in the Help dialog.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// Always applied to every transfer.
    Core,
    /// How Foresight observes rsync (drives the preview, progress bar, log).
    Reporting,
    /// Options the user turns on per job.
    Options,
}

impl Group {
    pub fn title(self) -> &'static str {
        match self {
            Group::Core => "Always applied",
            Group::Reporting => "How Foresight reads rsync",
            Group::Options => "Options you control",
        }
    }

    /// Registry order.
    pub const ORDER: [Group; 3] = [Group::Core, Group::Reporting, Group::Options];
}

/// One exposed capability: the flag(s) it maps to, the UI control that drives
/// it, an honest one-liner, and the `man rsync` option name to cross-reference.
pub struct Capability {
    pub name: &'static str,
    /// Canonical emitted flag id(s) — the argv token up to any `=value`.
    pub flags: &'static [&'static str],
    pub control: &'static str,
    pub description: &'static str,
    pub man_option: &'static str,
    pub group: Group,
}

/// The registry. Keep in sync with [`crate::job::Job::build_argv`] — the test
/// below enforces it.
pub const CAPABILITIES: &[Capability] = &[
    Capability {
        name: "Archive mode",
        flags: &["-a"],
        control: "Always on",
        description: "Recurse and preserve permissions, timestamps, symlinks, and \
                      ownership. (-a expands to -rlptgoD.)",
        man_option: "--archive, -a",
        group: Group::Core,
    },
    Capability {
        name: "Dry-run itemize",
        flags: &["-n", "-i"],
        control: "Dry Run button",
        description: "Preview a job without writing anything: rsync lists exactly \
                      what it would create, update, or delete.",
        man_option: "--dry-run (-n), --itemize-changes (-i)",
        group: Group::Reporting,
    },
    Capability {
        name: "Live progress & per-file output",
        flags: &["--info", "--out-format"],
        control: "Transfer page (every sync)",
        description: "Drives the progress bar and the streaming activity log \
                      during a real transfer.",
        man_option: "--info=progress2, --out-format",
        group: Group::Reporting,
    },
    Capability {
        name: "Mirror deletions",
        flags: &["--delete"],
        control: "Advanced → Mirror deletions",
        description: "Remove destination files that no longer exist in the source. \
                      Off by default; offered only for a single-folder mirror, and \
                      always confirmed against a fresh dry run.",
        man_option: "--delete",
        group: Group::Options,
    },
    Capability {
        name: "Verbose output",
        flags: &["-v"],
        control: "Advanced → Verbose output",
        description: "Add rsync's own messages (file list, transfer stats) to the log.",
        man_option: "--verbose, -v",
        group: Group::Options,
    },
    Capability {
        name: "Move files",
        flags: &["--remove-source-files"],
        control: "Advanced → Move files",
        description: "Delete each source file after it transfers, turning a copy \
                      into a move. Has no effect during a dry run.",
        man_option: "--remove-source-files",
        group: Group::Options,
    },
    Capability {
        name: "Bandwidth limit",
        flags: &["--bwlimit"],
        control: "Advanced → Bandwidth limit + Rate unit",
        description: "Cap the transfer rate, in KB/s, MB/s, or GB/s.",
        man_option: "--bwlimit",
        group: Group::Options,
    },
    Capability {
        name: "Exclude patterns",
        flags: &["--exclude"],
        control: "Advanced → Exclude patterns",
        description: "Skip files matching each space-separated pattern (e.g. *.tmp .git).",
        man_option: "--exclude",
        group: Group::Options,
    },
];

/// Common rsync capabilities Foresight does **not** yet expose as a dedicated
/// control. The Help dialog lists these and points users at the *Extra
/// arguments* field, which passes them through verbatim.
pub const NOT_EXPOSED: &[(&str, &str)] = &[
    ("--checksum (-c)", "Compare by checksum instead of size and modification time."),
    ("--compress (-z)", "Compress file data during the transfer."),
    ("--backup (-b)", "Keep backups of files that get replaced or deleted."),
    ("--partial", "Keep partially transferred files so a re-run can resume them."),
    ("host:path (SSH)", "Sync to or from another machine over SSH (planned as a control)."),
    ("--filter / merge files", "Complex include/exclude rule files."),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::{Job, Mode, Source};
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    /// The stable identity of an argv flag: the token up to any `=value`.
    fn flag_id(tok: &str) -> String {
        tok.split('=').next().unwrap_or(tok).to_string()
    }

    /// Every fixed flag `build_argv` can emit, across both modes, with every
    /// optional flag switched on. `extra_args` is left empty on purpose — it is
    /// the escape hatch, not a registry capability. Fields are set explicitly
    /// (no `..Default`) so adding a new `Job` field forces this test — and thus
    /// the registry — to be revisited.
    fn emitted_flag_ids() -> BTreeSet<String> {
        let job = Job {
            sources: vec![Source {
                path: PathBuf::from("/s"),
                is_dir: true,
            }],
            dest: PathBuf::from("/d"),
            delete: true,
            verbose: true,
            remove_source_files: true,
            bwlimit: Some("85M".into()),
            excludes: vec!["*.tmp".into()],
            extra_args: vec![],
        };
        let mut out = BTreeSet::new();
        for mode in [Mode::Preview, Mode::Sync] {
            for tok in job.build_argv(mode) {
                let s = tok.to_string_lossy();
                if s.starts_with('-') {
                    out.insert(flag_id(&s));
                }
            }
        }
        out
    }

    fn registry_flag_ids() -> BTreeSet<String> {
        CAPABILITIES
            .iter()
            .flat_map(|c| c.flags.iter())
            .map(|f| f.to_string())
            .collect()
    }

    #[test]
    fn registry_matches_build_argv_exactly() {
        assert_eq!(
            registry_flag_ids(),
            emitted_flag_ids(),
            "capability registry drifted from build_argv — update CAPABILITIES"
        );
    }

    #[test]
    fn every_capability_lists_a_flag() {
        for c in CAPABILITIES {
            assert!(!c.flags.is_empty(), "{} has no flags", c.name);
        }
    }
}
