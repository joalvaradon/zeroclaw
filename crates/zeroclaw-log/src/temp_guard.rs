//! Scope guard for sibling temp files that are written and then renamed over
//! their destination.
//!
//! Any writer that creates a temp file next to its target and has fallible
//! steps before the final `rename` must own the temp file through this guard,
//! so an early return removes it instead of leaking it into the log directory.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Which writer owns a sibling temp file. Also the infix of its file name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TempKind {
    /// The rolling trim: `<stem>.tmp.<pid>.<nanos>`.
    Trim,
    /// The legacy-log migration: `<stem>.migrate.<pid>.<nanos>`.
    Migrate,
}

impl TempKind {
    /// Every writer kind. A reader of temp names matches against this list, so
    /// a new kind is recognized by adding it here and to [`TempKind::infix`].
    const ALL: [Self; 2] = [Self::Trim, Self::Migrate];

    fn infix(self) -> &'static str {
        match self {
            Self::Trim => "tmp",
            Self::Migrate => "migrate",
        }
    }
}

/// The parts of a name built by [`temp_sibling_path`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TempName {
    pub(crate) kind: TempKind,
    pub(crate) pid: u32,
    /// Creation time in nanoseconds since the Unix epoch, as the writer read
    /// its clock.
    pub(crate) nanos: i64,
}

/// Path of a sibling temp file for `active`, in the same directory:
/// `<stem>.<kind>.<pid>.<nanos>`, where `<stem>` is what `Path::with_extension`
/// leaves of the active file name. Every writer that stages a file next to the
/// log builds its name here, so the name has a single definition.
pub(crate) fn temp_sibling_path(active: &Path, kind: TempKind, now_nanos: i64) -> PathBuf {
    active.with_extension(format!(
        "{}.{}.{}",
        kind.infix(),
        std::process::id(),
        now_nanos
    ))
}

/// Recognize a name built by [`temp_sibling_path`]. `active_stem` is
/// `Path::file_stem` of the active file, the same function `with_extension`
/// uses, so producer and reader agree on what the stem is.
///
/// Only the exact shape `<stem>.<kind>.<digits>.<digits>` matches: no prefix
/// matching, no extra segments, ASCII digits only (`str::parse` alone would
/// accept a leading `+`), and values that do not fit `u32`/`i64` do not match.
/// A reader that deletes what it recognizes relies on this being strict.
pub(crate) fn parse_temp_sibling(active_stem: &str, file_name: &str) -> Option<TempName> {
    let rest = file_name.strip_prefix(active_stem)?.strip_prefix('.')?;
    let mut parts = rest.split('.');
    let kind_part = parts.next()?;
    let pid_part = parts.next()?;
    let nanos_part = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let kind = TempKind::ALL
        .into_iter()
        .find(|kind| kind.infix() == kind_part)?;
    Some(TempName {
        kind,
        pid: ascii_digits(pid_part)?.parse().ok()?,
        nanos: ascii_digits(nanos_part)?.parse().ok()?,
    })
}

fn ascii_digits(part: &str) -> Option<&str> {
    (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())).then_some(part)
}

// Test seam: the last temp path created through `create_exclusive_temp` on this
// thread. Both writers create their temp file there, so a test can read the
// name each one really uses without a seam of its own.
#[cfg(test)]
thread_local! {
    static LAST_CREATED: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn record_created(path: &Path) {
    LAST_CREATED.with(|seen| *seen.borrow_mut() = Some(path.to_path_buf()));
}

#[cfg(test)]
pub(crate) fn last_created_temp() -> Option<PathBuf> {
    LAST_CREATED.with(|seen| seen.borrow().clone())
}

/// Removes the temp file on drop unless [`TempFileCleanup::disarm`] was called.
///
/// Keep it alive until the rename, and let the file handle (or a writer that
/// owns it) go out of scope before the guard does, so the file is closed when
/// it is removed.
pub(crate) struct TempFileCleanup {
    path: PathBuf,
    armed: bool,
}

/// Create `path` exclusively (`create_new`, mode `0o600` on Unix) and return
/// the handle together with an armed guard. Nothing fallible runs between the
/// creation and the guard, so the file is never left without an owner.
///
/// `label` names the caller in the error context (`creating <label> temp ...`).
pub(crate) fn create_exclusive_temp(path: &Path, label: &str) -> Result<(File, TempFileCleanup)> {
    let mut opts = OpenOptions::new();
    opts.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = opts
        .open(path)
        .with_context(|| format!("creating {label} temp {}", path.display()))?;
    #[cfg(test)]
    record_created(path);
    Ok((file, TempFileCleanup::new(path.to_path_buf())))
}

impl TempFileCleanup {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    /// Call right after the rename succeeded: the file now is the destination.
    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TempFileCleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_cleanup_removes_only_uncommitted_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("migration.tmp");
        fs::write(&path, "pending").unwrap();
        {
            let _cleanup = TempFileCleanup::new(path.clone());
        }
        assert!(!path.exists(), "uncommitted temp file must be removed");

        fs::write(&path, "committed").unwrap();
        {
            let mut cleanup = TempFileCleanup::new(path.clone());
            cleanup.disarm();
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), "committed");
    }

    #[test]
    fn temp_creation_collision_preserves_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("migration.tmp");
        fs::write(&path, "owned elsewhere").unwrap();

        assert!(create_exclusive_temp(&path, "migration").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "owned elsewhere");
    }

    #[test]
    fn the_produced_names_are_the_ones_the_writers_always_used() {
        // The trim and the migration built these names inline with
        // `with_extension` before the helper existed. Pin that exact shape so
        // a temp file left by an older build keeps the name a later reader of
        // these names expects.
        let pid = std::process::id();
        for active in [
            "logs/runtime-trace.jsonl",
            "logs/trace",
            "logs/a.b.jsonl",
            "runtime-trace.jsonl",
        ] {
            let active = Path::new(active);
            assert_eq!(
                temp_sibling_path(active, TempKind::Trim, 42),
                active.with_extension(format!("tmp.{pid}.42"))
            );
            assert_eq!(
                temp_sibling_path(active, TempKind::Migrate, 42),
                active.with_extension(format!("migrate.{pid}.42"))
            );
        }
    }

    #[test]
    fn a_produced_temp_name_is_recognized_with_its_parts() {
        for active in [
            "logs/runtime-trace.jsonl",
            "logs/trace",
            "logs/a.b.jsonl",
            "runtime-trace.jsonl",
        ] {
            let active = Path::new(active);
            let stem = active.file_stem().unwrap().to_str().unwrap();
            for kind in [TempKind::Trim, TempKind::Migrate] {
                let produced = temp_sibling_path(active, kind, 1_234_567);
                assert_eq!(produced.parent(), active.parent());
                let name = produced.file_name().unwrap().to_str().unwrap();
                assert_eq!(
                    parse_temp_sibling(stem, name),
                    Some(TempName {
                        kind,
                        pid: std::process::id(),
                        nanos: 1_234_567,
                    }),
                    "{name} must round-trip for the active path {}",
                    active.display()
                );
            }
        }
    }

    #[test]
    fn a_name_of_another_stem_or_shape_is_not_recognized() {
        assert_eq!(parse_temp_sibling("trace", "other.tmp.1.2"), None);
        assert_eq!(parse_temp_sibling("trace", "trace.tmp.1.2.3"), None);
        assert_eq!(parse_temp_sibling("trace", "trace.tmp.+1.2"), None);
        assert_eq!(parse_temp_sibling("trace", "trace.tmp.1"), None);
        assert!(parse_temp_sibling("trace", "trace.tmp.1.2").is_some());
        assert!(parse_temp_sibling("trace", "trace.migrate.7.8").is_some());
    }
}
