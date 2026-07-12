//! Job model and the single source of truth for rsync argv construction.
//!
//! Every rsync invocation in the app goes through [`Job::build_argv`]. No inline
//! argv vectors elsewhere, no shell strings ever: the subprocess is always
//! spawned with an argv vector, so no user-supplied string is shell-interpreted.
//! Paths are carried as [`PathBuf`]/[`OsString`] end to end and never
//! lossy-converted to UTF-8 before reaching argv.
//!
//! The two command lines this produces are the reporting contract that
//! `rsync-events` is tested against (see that crate's docs):
//!
//! ```text
//! rsync -a --info=progress2 --out-format='%i %n%L' [--delete] SRC/ DST/   # Sync
//! rsync -a -n -i [--delete] SRC/ DST/                                     # Preview
//! ```

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

/// Which of the two contract command lines to build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `-n -i`: a dry run that itemizes changes to populate the preview list.
    Preview,
    /// `--info=progress2 --out-format='%i %n%L'`: the real transfer.
    Sync,
}

/// A configured sync. The preview and the real run are built from the *same*
/// `Job`, so the dry run faithfully predicts what the transfer will do —
/// including deletions when [`delete`](Self::delete) is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub source: PathBuf,
    pub dest: PathBuf,
    /// Mirror deletions (`--delete`). Off by default; a safety rail in the UI.
    pub delete: bool,
}

impl Job {
    pub fn new(source: impl Into<PathBuf>, dest: impl Into<PathBuf>) -> Self {
        Self {
            source: source.into(),
            dest: dest.into(),
            delete: false,
        }
    }

    /// Build the exact argv for `mode`. The program name (`rsync`) is **not**
    /// included — the caller supplies the bundled binary path to the spawner.
    ///
    /// The source always gets a trailing `/` so rsync copies its *contents*
    /// into the destination (mirror semantics) rather than nesting the source
    /// directory inside the destination.
    pub fn build_argv(&self, mode: Mode) -> Vec<OsString> {
        let mut argv: Vec<OsString> = Vec::new();
        argv.push(OsString::from("-a"));

        match mode {
            Mode::Preview => {
                argv.push(OsString::from("-n"));
                argv.push(OsString::from("-i"));
            }
            Mode::Sync => {
                argv.push(OsString::from("--info=progress2"));
                argv.push(OsString::from("--out-format=%i %n%L"));
            }
        }

        if self.delete {
            argv.push(OsString::from("--delete"));
        }

        argv.push(with_trailing_slash(&self.source));
        argv.push(self.dest.as_os_str().to_os_string());
        argv
    }
}

/// Return `path` as an `OsString` guaranteed to end in a single `/`, operating
/// on raw bytes so non-UTF-8 paths survive untouched.
fn with_trailing_slash(path: &Path) -> OsString {
    let mut bytes = path.as_os_str().as_bytes().to_vec();
    if bytes.last() != Some(&b'/') {
        bytes.push(b'/');
    }
    OsString::from_vec(bytes)
}

/// Convenience for logging/display only — never feed this back into argv.
pub fn argv_display(argv: &[OsString]) -> String {
    argv.iter()
        .map(|a| OsStr::to_string_lossy(a).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    fn as_strs(argv: &[OsString]) -> Vec<&str> {
        argv.iter().map(|a| a.to_str().unwrap()).collect()
    }

    #[test]
    fn preview_matches_contract() {
        let job = Job::new("/data/src", "/data/dst");
        let argv = job.build_argv(Mode::Preview);
        assert_eq!(
            as_strs(&argv),
            ["-a", "-n", "-i", "/data/src/", "/data/dst"]
        );
    }

    #[test]
    fn sync_matches_contract() {
        let job = Job::new("/data/src", "/data/dst");
        let argv = job.build_argv(Mode::Sync);
        assert_eq!(
            as_strs(&argv),
            [
                "-a",
                "--info=progress2",
                "--out-format=%i %n%L",
                "/data/src/",
                "/data/dst",
            ]
        );
    }

    #[test]
    fn delete_toggle_inserted_before_paths_in_both_modes() {
        let mut job = Job::new("/s", "/d");
        job.delete = true;
        assert_eq!(
            as_strs(&job.build_argv(Mode::Preview)),
            ["-a", "-n", "-i", "--delete", "/s/", "/d"]
        );
        assert_eq!(
            as_strs(&job.build_argv(Mode::Sync)),
            [
                "-a",
                "--info=progress2",
                "--out-format=%i %n%L",
                "--delete",
                "/s/",
                "/d"
            ]
        );
    }

    #[test]
    fn delete_off_by_default() {
        let job = Job::new("/s", "/d");
        assert!(!job.delete);
        assert!(!job.build_argv(Mode::Sync).iter().any(|a| a == "--delete"));
    }

    #[test]
    fn source_gets_single_trailing_slash() {
        // no slash -> one added
        let a = Job::new("/a/b", "/x").build_argv(Mode::Sync);
        assert!(a.iter().any(|s| s.as_bytes() == b"/a/b/"));
        // already slashed -> not doubled
        let b = Job::new("/a/b/", "/x").build_argv(Mode::Sync);
        assert!(b.iter().any(|s| s.as_bytes() == b"/a/b/"));
        assert!(!b.iter().any(|s| s.as_bytes() == b"/a/b//"));
    }

    #[test]
    fn dest_is_passed_verbatim_without_trailing_slash() {
        let argv = Job::new("/s", "/d/e").build_argv(Mode::Preview);
        assert_eq!(argv.last().unwrap().as_bytes(), b"/d/e");
    }

    #[test]
    fn out_format_is_one_argv_element_not_shell_split() {
        // The space inside --out-format=%i %n%L must live in a SINGLE argv
        // element; a shell string would have split it into two args.
        let argv = Job::new("/s", "/d").build_argv(Mode::Sync);
        assert!(argv.iter().any(|a| a.as_bytes() == b"--out-format=%i %n%L"));
    }

    #[test]
    fn non_utf8_path_is_preserved_byte_for_byte() {
        use std::os::unix::ffi::OsStrExt;
        let src = PathBuf::from(OsStr::from_bytes(b"/bad/\xff\xfename"));
        let argv = Job::new(src, "/d").build_argv(Mode::Preview);
        // trailing slash appended, original bytes intact
        assert!(argv.iter().any(|a| a.as_bytes() == b"/bad/\xff\xfename/"));
    }
}
