//! The capability registry — the single, honest source of truth for *which*
//! rsync flags Foresight actually exposes in this release.
//!
//! This table sits beside [`crate::job::Job::build_argv`] on purpose: the Help
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
///
/// The display strings (`name`, `control`, `description`, and the pairs in
/// [`PATH_BEHAVIOR`] / [`NOT_EXPOSED`]) are wrapped in [`crate::i18n::gettext_noop`]
/// so `xgettext` registers them as msgids; `help` translates them at render
/// time with [`crate::i18n::i18n`].
pub const CAPABILITIES: &[Capability] = &[
    Capability {
        name: crate::i18n::gettext_noop("Archive mode"),
        flags: &["-a"],
        control: crate::i18n::gettext_noop("Always on"),
        description: crate::i18n::gettext_noop(
            "Recurse and preserve permissions, timestamps, symlinks, and \
             ownership. (-a expands to -rlptgoD.)",
        ),
        man_option: "--archive, -a",
        group: Group::Core,
    },
    Capability {
        name: crate::i18n::gettext_noop("Dry-run itemize"),
        flags: &["-n", "-i"],
        control: crate::i18n::gettext_noop("Dry Run button"),
        description: crate::i18n::gettext_noop(
            "Preview a job without writing anything: rsync lists exactly \
             what it would create, update, or delete.",
        ),
        man_option: "--dry-run (-n), --itemize-changes (-i)",
        group: Group::Reporting,
    },
    Capability {
        name: crate::i18n::gettext_noop("Live progress & per-file output"),
        flags: &["--info", "--out-format"],
        control: crate::i18n::gettext_noop("Transfer page (every sync)"),
        description: crate::i18n::gettext_noop(
            "Drives the progress bar and the streaming activity log \
             during a real transfer.",
        ),
        man_option: "--info=progress2, --out-format",
        group: Group::Reporting,
    },
    Capability {
        name: crate::i18n::gettext_noop("Mirror deletions"),
        flags: &["--delete"],
        control: crate::i18n::gettext_noop("Advanced → Mirror deletions"),
        description: crate::i18n::gettext_noop(
            "Remove destination files that no longer exist in the source. \
             Off by default; offered only for a single-folder sync, and \
             always confirmed against a fresh dry run.",
        ),
        man_option: "--delete",
        group: Group::Options,
    },
    Capability {
        name: crate::i18n::gettext_noop("Verbose output"),
        flags: &["-v"],
        control: crate::i18n::gettext_noop("Advanced → Verbose output"),
        description: crate::i18n::gettext_noop(
            "Add rsync's own messages (file list, transfer stats) to the log.",
        ),
        man_option: "--verbose, -v",
        group: Group::Options,
    },
    Capability {
        name: crate::i18n::gettext_noop("Move files"),
        flags: &["--remove-source-files"],
        control: crate::i18n::gettext_noop("Advanced → Move files"),
        description: crate::i18n::gettext_noop(
            "Delete each source file after it transfers, turning a copy \
             into a move. Has no effect during a dry run.",
        ),
        man_option: "--remove-source-files",
        group: Group::Options,
    },
    Capability {
        name: crate::i18n::gettext_noop("Bandwidth limit"),
        flags: &["--bwlimit"],
        control: crate::i18n::gettext_noop("Advanced → Bandwidth limit + Rate unit"),
        description: crate::i18n::gettext_noop("Cap the transfer rate, in KB/s, MB/s, or GB/s."),
        man_option: "--bwlimit",
        group: Group::Options,
    },
    Capability {
        name: crate::i18n::gettext_noop("Filter rules"),
        flags: &["--exclude", "--include"],
        control: crate::i18n::gettext_noop("Advanced → Filter rules"),
        description: crate::i18n::gettext_noop(
            "Skip or keep paths by pattern (e.g. *.tmp, .git). The list is \
             ordered and rsync obeys the first rule that matches, so an \
             Include above a broader Exclude carves an exception out of it \
             — use the arrows on a rule to change which one wins. Each rule \
             is passed whole, so it may contain spaces.",
        ),
        man_option: "--exclude, --include",
        group: Group::Options,
    },
    Capability {
        name: crate::i18n::gettext_noop("Remote shell for SSH transfers"),
        flags: &["-e"],
        control: crate::i18n::gettext_noop("Set automatically for a remote endpoint"),
        description: crate::i18n::gettext_noop(
            "Runs ssh with Foresight's own known_hosts, strict host-key \
             checking, and no interactive prompts. Your keys come from \
             the desktop's SSH agent — no private key ever enters the \
             sandbox, and ~/.ssh is never read.",
        ),
        man_option: "--rsh, -e",
        group: Group::Options,
    },
];

/// How Foresight places sources in the destination. Not a flag — it is the
/// shape of the *path operands* [`crate::job::Job::build_argv`] emits — but it
/// changes where files land, so the Help states it outright rather than leaving
/// users to rediscover rsync's trailing-slash rule the hard way.
pub const PATH_BEHAVIOR: (&str, &str) = (
    crate::i18n::gettext_noop("Where your files land"),
    crate::i18n::gettext_noop(
        "Everything you add lands inside the destination: a folder named Photos becomes \
         destination/Photos/, a file becomes destination/file. Turn on Advanced → Sync \
         folder contents to get rsync's other form instead, where a single folder's \
         children are copied straight into the destination and the folder itself is not \
         recreated.",
    ),
);

/// Common rsync capabilities Foresight does **not** yet expose as a dedicated
/// control. The Help dialog lists these and points users at the *Extra
/// arguments* field, which passes them through verbatim.
pub const NOT_EXPOSED: &[(&str, &str)] = &[
    (
        crate::i18n::gettext_noop("--checksum (-c)"),
        crate::i18n::gettext_noop("Compare by checksum instead of size and modification time."),
    ),
    (
        crate::i18n::gettext_noop("--compress (-z)"),
        crate::i18n::gettext_noop("Compress file data during the transfer."),
    ),
    (
        crate::i18n::gettext_noop("--backup (-b)"),
        crate::i18n::gettext_noop("Keep backups of files that get replaced or deleted."),
    ),
    (
        crate::i18n::gettext_noop("--partial"),
        crate::i18n::gettext_noop("Keep partially transferred files so a re-run can resume them."),
    ),
    (
        crate::i18n::gettext_noop("--filter / merge files"),
        crate::i18n::gettext_noop(
            "rsync's fuller filter syntax: rules read from a file, and per-directory \
             merge rules. Plain include and exclude rules are a control — see Filter \
             rules above.",
        ),
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::{FilterRule, Job, Mode, Remote, Source};
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
            // A remote job, so the flags only remote transfers emit are covered.
            remote: Some(Remote::Dest("u@h:/p".into())),
            delete: true,
            sync_contents: true,
            verbose: true,
            remove_source_files: true,
            bwlimit: Some("85M".into()),
            // Both kinds, so the registry must account for both flags.
            filters: vec![FilterRule::exclude("*.tmp"), FilterRule::include("*.jpg")],
            remote_shell: Some("ssh".into()),
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
