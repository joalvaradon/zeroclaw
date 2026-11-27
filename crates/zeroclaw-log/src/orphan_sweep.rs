//! Background sweep of orphaned sibling temp files next to the active log.
//!
//! The rolling trim and the legacy-log migration each stage a sibling temp file
//! and rename it over the log. A scope guard removes that file on an error, but
//! `Drop` does not run when the process is killed, loses power, or panics in a
//! build that aborts on panic, so those files are left behind. This sweep
//! removes them once the writer starts.
//!
//! It deletes files, so it is deliberately narrow: only the exact name shape the
//! two writers produce, only regular files (never links or directories), and
//! only once both the age recorded in the name and the time since the file was
//! last modified are at least [`ORPHAN_MIN_AGE`]. It lists only the directory
//! of the active log.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::temp_guard::parse_temp_sibling;

/// A temp file younger than this may belong to a live trim or migration, so it
/// is kept. Two readings must both reach it: the creation time in the file name
/// (known without a system call, checked first for every entry), and the time
/// of the file's last modification (read only for entries that already look
/// old). A trim or migration that is still writing keeps refreshing the second,
/// so a long one is not taken for an abandoned one. Touching a file can only
/// delay its removal, never hasten it.
///
/// Neither reading helps when the sweeping clock itself is ahead of the clock
/// that stamped the file: both are compared with the sweeper's "now".
///
/// Not a config key: nothing here needs an operator to tune it.
pub(crate) const ORPHAN_MIN_AGE: Duration = Duration::from_secs(30 * 60);

/// Removals between pauses, and how long each pause lasts. Keeps a directory
/// with a very large backlog from monopolizing the disk while the daemon starts.
pub(crate) struct Throttle {
    /// Pause after every `every` removals; `0` never pauses.
    pub(crate) every: usize,
    pub(crate) pause: Duration,
}

/// Pause 1 ms after every 256 removals. Measured once, on one machine (a virtual
/// disk, 100000 empty files), before the modification-time check existed: a
/// sweep took about 1.5 s, and the tail latency of a concurrent synced write
/// stayed well below what it reaches without the pauses. Other disks may want
/// other values.
const SWEEP_THROTTLE: Throttle = Throttle {
    every: 256,
    pause: Duration::from_millis(1),
};

/// What one pass over the directory found and did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SweepReport {
    /// Orphans removed.
    pub(crate) deleted: usize,
    /// Orphan-shaped files younger than the threshold, by the time in their
    /// name or by their last modification.
    pub(crate) kept_recent: usize,
    /// Orphan-shaped files whose age cannot be trusted: stamped later than now
    /// (the clock went backwards, or the name is not ours), stamped `0` (the
    /// writer's own clock was out of range), or modified at a time that is in
    /// the future or not representable. Kept, because deleting on a doubtful
    /// age is the destructive choice.
    pub(crate) kept_future: usize,
    /// Orphan-shaped entries that are not regular files: links and directories.
    pub(crate) kept_not_regular: usize,
    /// Orphans that were gone by the time the sweep removed them.
    pub(crate) already_gone: usize,
    /// Entries that could not be listed, inspected or removed.
    pub(crate) errors: usize,
    /// The first of those errors, for the log.
    pub(crate) first_error: Option<String>,
    /// Age of the oldest and of the newest orphan-shaped regular file seen.
    pub(crate) oldest_age: Option<Duration>,
    pub(crate) newest_age: Option<Duration>,
}

impl SweepReport {
    fn note_error(&mut self, path: &Path, err: &io::Error) {
        self.errors += 1;
        self.first_error
            .get_or_insert_with(|| format!("{}: {err}", path.display()));
    }
}

/// One pass over the directory of `active`, removing orphaned temp files at
/// least `threshold` old as of `now_nanos` (nanoseconds since the Unix epoch).
///
/// A directory that does not exist has nothing to sweep and is not an error. It
/// fails only when the directory exists but cannot be listed; a problem with one
/// entry is counted in the report and the pass continues.
pub(crate) fn sweep_orphans(
    active: &Path,
    now_nanos: i64,
    threshold: Duration,
    throttle: &Throttle,
) -> Result<SweepReport> {
    sweep_with_pause(active, now_nanos, threshold, throttle, thread::sleep)
}

/// [`sweep_orphans`] with the pause injected, so a test can count pauses
/// instead of sleeping.
fn sweep_with_pause(
    active: &Path,
    now_nanos: i64,
    threshold: Duration,
    throttle: &Throttle,
    mut pause: impl FnMut(Duration),
) -> Result<SweepReport> {
    let dir = match active.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let stem = active
        .file_stem()
        .and_then(|stem| stem.to_str())
        .with_context(|| format!("log path {} has no UTF-8 file stem", active.display()))?;
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        // The writer creates the log directory on its first write, so on a
        // fresh install it does not exist yet when the sweep starts. No
        // directory means no leftovers, and it must not be reported: a warning
        // here is itself written into the log being started.
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(SweepReport::default()),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("listing {} for orphaned temp files", dir.display()));
        }
    };

    let mut report = SweepReport::default();
    let mut removals: usize = 0;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                report.note_error(dir, &err);
                continue;
            }
        };
        // The cheap name check comes first: a directory with millions of
        // unrelated entries costs no system call per entry.
        let file_name = entry.file_name();
        let Some(stamp_nanos) = file_name
            .to_str()
            .and_then(|name| parse_temp_sibling(stem, name))
        else {
            continue;
        };
        let path = entry.path();
        // `DirEntry::file_type` does not follow links, so a link or directory
        // with an orphan's name is left alone.
        match entry.file_type() {
            Ok(kind) if kind.is_file() => {}
            Ok(_) => {
                report.kept_not_regular += 1;
                continue;
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                report.already_gone += 1;
                continue;
            }
            Err(err) => {
                report.note_error(&path, &err);
                continue;
            }
        }

        // `0` is what a writer records when its own clock was out of range, so
        // the real creation time is unknown; against a normal clock it would
        // look decades old and a live temp file would be removed.
        if stamp_nanos == 0 {
            report.kept_future += 1;
            continue;
        }
        let Some(age) = elapsed_since(now_nanos, stamp_nanos) else {
            report.kept_future += 1;
            continue;
        };
        report.oldest_age = Some(report.oldest_age.map_or(age, |oldest| oldest.max(age)));
        report.newest_age = Some(report.newest_age.map_or(age, |newest| newest.min(age)));
        if age < threshold {
            report.kept_recent += 1;
            continue;
        }

        // The name says old. The file's own last modification is the second
        // witness, read only here so a directory of unrelated entries, and
        // every young orphan, cost no extra call. A trim or migration still
        // writing keeps it fresh. Unreadable means unknown, and unknown is kept.
        let modified = match entry.metadata().and_then(|meta| meta.modified()) {
            Ok(time) => time,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                report.already_gone += 1;
                continue;
            }
            Err(err) => {
                report.note_error(&path, &err);
                continue;
            }
        };
        let Some(idle) = nanos_since_epoch(modified)
            .and_then(|modified_nanos| elapsed_since(now_nanos, modified_nanos))
        else {
            report.kept_future += 1;
            continue;
        };
        if idle < threshold {
            report.kept_recent += 1;
            continue;
        }

        #[cfg(test)]
        before_remove_hook(&path);
        match remove_if_present(&path) {
            Ok(true) => report.deleted += 1,
            Ok(false) => report.already_gone += 1,
            Err(err) => report.note_error(&path, &err),
        }
        removals += 1;
        if throttle.every > 0
            && removals.is_multiple_of(throttle.every)
            && !throttle.pause.is_zero()
        {
            pause(throttle.pause);
        }
    }
    Ok(report)
}

/// Time from `then_nanos` to `now_nanos`, or `None` when `then_nanos` is later
/// than now (the two clocks disagree, so the age is not known).
fn elapsed_since(now_nanos: i64, then_nanos: i64) -> Option<Duration> {
    let nanos = u64::try_from(now_nanos.saturating_sub(then_nanos)).ok()?;
    Some(Duration::from_nanos(nanos))
}

/// `time` as nanoseconds since the Unix epoch, or `None` when it is before the
/// epoch or does not fit.
fn nanos_since_epoch(time: SystemTime) -> Option<i64> {
    let since = time.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(since.as_nanos()).ok()
}

/// Remove `path`; `Ok(false)` when it was already gone. Removing never follows
/// a link, so a file swapped for a link after it was checked costs only the link.
fn remove_if_present(path: &Path) -> io::Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err),
    }
}

// Test seam: runs for each candidate right before its removal, letting a test
// change the entry inside the window between "judged an orphan" and "removed".
#[cfg(test)]
type BeforeRemoveHook = Box<dyn FnMut(&Path)>;

#[cfg(test)]
thread_local! {
    static BEFORE_REMOVE: std::cell::RefCell<Option<BeforeRemoveHook>> =
        const { std::cell::RefCell::new(None) };
}

/// Run the installed seam, if any. The callback is moved out for the call so it
/// can itself touch the directory without re-entering a live borrow.
#[cfg(test)]
fn before_remove_hook(path: &Path) {
    let mut hook = BEFORE_REMOVE.with(|slot| slot.borrow_mut().take());
    if let Some(callback) = hook.as_mut() {
        callback(path);
    }
    BEFORE_REMOVE.with(|slot| *slot.borrow_mut() = hook);
}

/// At most one sweep runs at a time. A config reload while one is running does
/// not start another.
static SWEEP_RUNNING: AtomicBool = AtomicBool::new(false);

/// Proof that this caller owns the running-sweep flag; releases it on drop.
pub(crate) struct SweepSlot(());

impl Drop for SweepSlot {
    fn drop(&mut self) {
        SWEEP_RUNNING.store(false, Ordering::Release);
    }
}

pub(crate) fn try_begin_sweep() -> Option<SweepSlot> {
    SWEEP_RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .ok()
        .map(|_| SweepSlot(()))
}

/// Start a background sweep of the directory of `active`, unless one is running.
///
/// Never blocks the caller: the writer starts while the daemon holds ownership
/// of its configuration, and a directory with millions of leftovers would stall
/// that start. The thread is detached; if the process exits the sweep stops
/// between two independent removals. Under test the sweep runs inline, so tests
/// do not depend on thread timing.
pub(crate) fn spawn_orphan_sweep(active: &Path) {
    let Some(slot) = try_begin_sweep() else {
        tracing::debug!(
            target: "zeroclaw_log",
            path = %active.display(),
            "log: an orphan temp-file sweep is already running; not starting another"
        );
        return;
    };
    #[cfg(test)]
    run_sweep(active, slot);
    #[cfg(not(test))]
    start_or_warn(active, slot, start_sweep_thread);
}

/// Hands the sweep to `start`. If the thread cannot be started the writer goes
/// on without the sweep: it is housekeeping, not durability. `start` takes the
/// slot, so a start that fails has already let go of it and the next request
/// can run. The starter is a parameter so a test can make it fail.
fn start_or_warn(
    active: &Path,
    slot: SweepSlot,
    start: impl FnOnce(PathBuf, SweepSlot) -> io::Result<JoinHandle<()>>,
) {
    if let Err(err) = start(active.to_path_buf(), slot) {
        tracing::warn!(
            target: "zeroclaw_log",
            error = ?err,
            path = %active.display(),
            "log: could not start the orphan temp-file sweep thread"
        );
    }
}

fn start_sweep_thread(active: PathBuf, slot: SweepSlot) -> io::Result<JoinHandle<()>> {
    start_with(active, slot, |job| {
        thread::Builder::new()
            .name("zeroclaw-log-sweep".into())
            .spawn(job)
    })
}

/// Wraps the sweep, which owns `slot`, as a job for `spawn`. When `spawn` fails
/// it drops the job unrun, and with it the slot. The spawner is a parameter so a
/// test can make it fail.
fn start_with(
    active: PathBuf,
    slot: SweepSlot,
    spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> io::Result<JoinHandle<()>>,
) -> io::Result<JoinHandle<()>> {
    spawn(Box::new(move || run_sweep(&active, slot)))
}

fn run_sweep(active: &Path, slot: SweepSlot) {
    // A clock outside the representable range gives `0`, which stamps every
    // name as "in the future", so nothing is deleted.
    let now_nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
    match sweep_orphans(active, now_nanos, ORPHAN_MIN_AGE, &SWEEP_THROTTLE) {
        Ok(report) => log_report(active, &report),
        Err(err) => tracing::warn!(
            target: "zeroclaw_log",
            error = ?err,
            path = %active.display(),
            "log: orphan temp-file sweep failed"
        ),
    }
    drop(slot);
}

/// One summary per pass, and nothing at all when there was nothing to report.
/// The ages are the clue to a producer that is still failing: a very young
/// orphan means a trim or migration was cut short recently.
fn log_report(active: &Path, report: &SweepReport) {
    let seen = report.deleted
        + report.kept_recent
        + report.kept_future
        + report.kept_not_regular
        + report.already_gone
        + report.errors;
    if seen == 0 {
        return;
    }
    tracing::info!(
        target: "zeroclaw_log",
        path = %active.display(),
        deleted = report.deleted,
        kept_recent = report.kept_recent,
        kept_future = report.kept_future,
        kept_not_regular = report.kept_not_regular,
        already_gone = report.already_gone,
        errors = report.errors,
        oldest_age_secs = report.oldest_age.map(|age| age.as_secs()),
        newest_age_secs = report.newest_age.map(|age| age.as_secs()),
        "log: orphan temp-file sweep finished"
    );
    if let Some(first) = &report.first_error {
        tracing::warn!(
            target: "zeroclaw_log",
            errors = report.errors,
            first_error = %first,
            "log: orphan temp-file sweep could not process some entries"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::temp_guard::parse_temp_sibling;
    use crate::test_support::{CapturedWarnings, set_modified};

    /// A fixed "now" (2027-01-15 UTC), so ages are exact. It does not need to
    /// differ from the real clock: nothing here reads the real one.
    const NOW: i64 = 1_800_000_000_000_000_000;
    const STEM: &str = "runtime-trace";
    const NO_PAUSE: Throttle = Throttle {
        every: 256,
        pause: Duration::ZERO,
    };

    fn nanos_ago(age: Duration) -> i64 {
        NOW - i64::try_from(age.as_nanos()).unwrap()
    }

    fn active_in(dir: &Path) -> PathBuf {
        dir.join("runtime-trace.jsonl")
    }

    /// Creates `name` as a file last modified when the name says it was
    /// created, as a writer that wrote it and was then killed would leave it.
    /// A name that is not an orphan's keeps the modification time of now.
    fn seed(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, "orphan\n").unwrap();
        if let Some(stamp) = parse_temp_sibling(STEM, name).and_then(|n| u64::try_from(n).ok()) {
            set_modified(&path, UNIX_EPOCH + Duration::from_nanos(stamp));
        }
        path
    }

    fn modified_at(nanos: i64) -> SystemTime {
        UNIX_EPOCH + Duration::from_nanos(u64::try_from(nanos).unwrap())
    }

    /// Seeds an orphan whose name says it is `name_age` old and whose file was
    /// last modified `idle` ago. Returns its name.
    fn seed_idle(dir: &Path, name_age: Duration, idle: Duration) -> String {
        let name = format!("runtime-trace.tmp.1.{}", nanos_ago(name_age));
        let path = seed(dir, &name);
        set_modified(&path, modified_at(nanos_ago(idle)));
        name
    }

    fn names(dir: &Path) -> BTreeSet<String> {
        fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    fn only(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|name| (*name).to_string()).collect()
    }

    #[test]
    fn the_age_threshold_is_thirty_minutes() {
        // Pins the figure the operator docs promise (`logging.md` and
        // `observability.md` say 30 minutes). It checks the constant against
        // that promise, not the sweep's behavior; the tests below use the
        // constant itself.
        assert_eq!(ORPHAN_MIN_AGE, Duration::from_secs(30 * 60));
    }

    #[test]
    fn old_trim_and_migration_orphans_of_any_pid_are_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let active = active_in(tmp.path());
        fs::write(&active, "{\"n\":1}\n").unwrap();
        let age = ORPHAN_MIN_AGE + Duration::from_secs(3600);
        let old = nanos_ago(age);
        let own_pid = std::process::id();
        let orphans = [
            format!("runtime-trace.tmp.{own_pid}.{old}"),
            format!("runtime-trace.tmp.99999.{old}"),
            format!("runtime-trace.migrate.{own_pid}.{old}"),
            format!("runtime-trace.migrate.99999.{old}"),
        ];
        for name in &orphans {
            assert!(
                parse_temp_sibling(STEM, name).is_some(),
                "test setup: {name} must be recognized as an orphan name"
            );
            seed(tmp.path(), name);
        }

        let report = sweep_orphans(&active, NOW, ORPHAN_MIN_AGE, &NO_PAUSE).unwrap();

        assert_eq!(report.deleted, 4);
        assert_eq!(names(tmp.path()), only(&["runtime-trace.jsonl"]));
        assert_eq!(fs::read(&active).unwrap(), b"{\"n\":1}\n");
        assert_eq!(report.oldest_age, Some(age));
        assert_eq!(report.newest_age, Some(age));
    }

    #[test]
    fn a_recent_orphan_survives_and_the_threshold_is_inclusive() {
        let tmp = tempfile::tempdir().unwrap();
        let active = active_in(tmp.path());
        let just_under = format!(
            "runtime-trace.tmp.1.{}",
            nanos_ago(ORPHAN_MIN_AGE - Duration::from_secs(1))
        );
        let exactly = format!("runtime-trace.tmp.2.{}", nanos_ago(ORPHAN_MIN_AGE));
        for name in [&just_under, &exactly] {
            assert!(parse_temp_sibling(STEM, name).is_some());
            seed(tmp.path(), name);
        }

        let report = sweep_orphans(&active, NOW, ORPHAN_MIN_AGE, &NO_PAUSE).unwrap();

        assert_eq!(report.deleted, 1, "an orphan exactly at the threshold goes");
        assert_eq!(report.kept_recent, 1);
        assert_eq!(names(tmp.path()), BTreeSet::from([just_under]));
        assert_eq!(report.oldest_age, Some(ORPHAN_MIN_AGE));
        assert_eq!(
            report.newest_age,
            Some(ORPHAN_MIN_AGE - Duration::from_secs(1))
        );
    }

    #[test]
    fn a_name_stamped_in_the_future_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let active = active_in(tmp.path());
        let name = format!("runtime-trace.tmp.1.{}", NOW + 3_600_000_000_000);
        assert!(parse_temp_sibling(STEM, &name).is_some());
        seed(tmp.path(), &name);

        let report = sweep_orphans(&active, NOW, ORPHAN_MIN_AGE, &NO_PAUSE).unwrap();

        assert_eq!(report.deleted, 0);
        assert_eq!(report.kept_future, 1);
        assert_eq!(names(tmp.path()), BTreeSet::from([name]));
    }

    #[test]
    fn a_name_stamped_zero_is_kept_because_its_age_is_unknown() {
        // A writer records `0` when its own clock was out of range. Read against
        // a normal clock that would look decades old.
        let tmp = tempfile::tempdir().unwrap();
        let active = active_in(tmp.path());
        let name = "runtime-trace.tmp.1.0".to_string();
        assert!(parse_temp_sibling(STEM, &name).is_some());
        seed(tmp.path(), &name);

        let report = sweep_orphans(&active, NOW, ORPHAN_MIN_AGE, &NO_PAUSE).unwrap();

        assert_eq!(report.deleted, 0);
        assert_eq!(report.kept_future, 1);
        assert_eq!(names(tmp.path()), BTreeSet::from([name]));
    }

    #[test]
    fn an_old_name_on_a_file_that_was_modified_recently_is_kept() {
        // A trim or migration that has run for a long time is still writing, so
        // its name is old but the file is not idle.
        let tmp = tempfile::tempdir().unwrap();
        let active = active_in(tmp.path());
        let name = seed_idle(tmp.path(), ORPHAN_MIN_AGE * 4, Duration::from_secs(60));

        let report = sweep_orphans(&active, NOW, ORPHAN_MIN_AGE, &NO_PAUSE).unwrap();

        assert_eq!(report.deleted, 0);
        assert_eq!(report.kept_recent, 1);
        assert_eq!(names(tmp.path()), BTreeSet::from([name]));
    }

    #[test]
    fn a_recent_name_on_a_file_that_was_modified_long_ago_is_kept() {
        // A file copied back from a backup keeps its old modification time. Its
        // name says it was created a minute ago, so a live writer may own it:
        // the name is a check of its own, not a duplicate of the other.
        let tmp = tempfile::tempdir().unwrap();
        let active = active_in(tmp.path());
        let name = seed_idle(tmp.path(), Duration::from_secs(60), ORPHAN_MIN_AGE * 4);

        let report = sweep_orphans(&active, NOW, ORPHAN_MIN_AGE, &NO_PAUSE).unwrap();

        assert_eq!(report.deleted, 0);
        assert_eq!(report.kept_recent, 1);
        assert_eq!(names(tmp.path()), BTreeSet::from([name]));
    }

    #[test]
    fn the_idle_threshold_is_inclusive() {
        let tmp = tempfile::tempdir().unwrap();
        let active = active_in(tmp.path());
        let idle_enough = seed_idle(tmp.path(), ORPHAN_MIN_AGE * 4, ORPHAN_MIN_AGE);
        let just_under = seed_idle(
            tmp.path(),
            ORPHAN_MIN_AGE * 5,
            ORPHAN_MIN_AGE - Duration::from_secs(1),
        );

        let report = sweep_orphans(&active, NOW, ORPHAN_MIN_AGE, &NO_PAUSE).unwrap();

        assert_eq!(report.deleted, 1, "idle for exactly the threshold goes");
        assert_eq!(report.kept_recent, 1);
        assert_eq!(names(tmp.path()), BTreeSet::from([just_under]));
        assert!(!tmp.path().join(idle_enough).exists());
    }

    #[test]
    fn a_modification_time_in_the_future_is_kept() {
        // The file system clock and the sweeping clock disagree, so how long the
        // file has been idle is not known.
        let tmp = tempfile::tempdir().unwrap();
        let active = active_in(tmp.path());
        let name = seed_idle(tmp.path(), ORPHAN_MIN_AGE * 4, Duration::ZERO);
        let path = tmp.path().join(&name);
        set_modified(&path, modified_at(NOW + 3_600_000_000_000));

        let report = sweep_orphans(&active, NOW, ORPHAN_MIN_AGE, &NO_PAUSE).unwrap();

        assert_eq!(report.deleted, 0);
        assert_eq!(report.kept_future, 1);
        assert_eq!(names(tmp.path()), BTreeSet::from([name]));
    }

    #[test]
    fn a_sweep_thread_that_cannot_start_is_a_warning_and_leaves_the_slot_free() {
        // `thread::Builder::spawn` can fail (no resources). The writer must go
        // on and say so, and a later request must be able to sweep.
        let _guard = crate::writer::WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        let active = active_in(tmp.path());
        let slot = try_begin_sweep().expect("no sweep is running yet");
        let warnings = CapturedWarnings::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(warnings.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::WARN)
            .finish();
        let mut asked_for = None;

        tracing::subscriber::with_default(subscriber, || {
            // The real wrapper with a spawner that fails: the slot must come
            // back because the unrun job is dropped, not because the test lets
            // go of it.
            start_or_warn(&active, slot, |path, slot| {
                asked_for = Some(path.clone());
                start_with(path, slot, |_job| {
                    Err(io::Error::other("injected: out of threads"))
                })
            });
        });

        assert_eq!(asked_for, Some(active));
        let warned = warnings.text();
        assert!(
            warned.contains("could not start the orphan temp-file sweep thread")
                && warned.contains("out of threads"),
            "the failure must be reported with its cause: {warned:?}"
        );
        assert!(
            try_begin_sweep().is_some(),
            "a sweep that never started must not hold the slot"
        );
    }

    #[test]
    fn only_the_exact_orphan_shape_is_ever_touched() {
        let tmp = tempfile::tempdir().unwrap();
        let active = active_in(tmp.path());
        let old = nanos_ago(ORPHAN_MIN_AGE * 4);
        let mut untouched = vec![
            "runtime-trace.jsonl".to_string(),
            "runtime-trace.jsonl.seq".to_string(),
            "runtime-trace.0000000007-20260624-031500.jsonl".to_string(),
            format!("other.tmp.1.{old}"),
            format!("xruntime-trace.tmp.1.{old}"),
            format!("runtime-trace.bak.1.{old}"),
            format!("runtime-trace.TMP.1.{old}"),
            format!("runtime-trace.tmp.+1.{old}"),
            format!("runtime-trace.tmp.-1.{old}"),
            format!("runtime-trace.tmp..{old}"),
            format!("runtime-trace.tmp.1.{old}.3"),
            format!("runtime-trace.tmp.1.{old}0000000000000000000000"),
            "runtime-trace.tmp.\u{661}.\u{662}".to_string(),
            "runtime-trace.tmp.abc.def".to_string(),
            "runtime-trace.tmp.1".to_string(),
            "runtime-trace.tmp".to_string(),
        ];
        // Windows drops a trailing dot from a name, so this one cannot exist there.
        if cfg!(unix) {
            untouched.push("runtime-trace.tmp.1.".to_string());
        }
        fs::write(&active, "log\n").unwrap();
        for name in &untouched {
            if name != "runtime-trace.jsonl" {
                seed(tmp.path(), name);
            }
            assert!(
                parse_temp_sibling(STEM, name).is_none(),
                "test setup: {name} must not be recognized as an orphan name"
            );
        }

        let report = sweep_orphans(&active, NOW, ORPHAN_MIN_AGE, &NO_PAUSE).unwrap();

        assert_eq!(report.deleted, 0);
        assert_eq!(names(tmp.path()), untouched.iter().cloned().collect());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_with_an_orphan_name_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("logs");
        fs::create_dir(&dir).unwrap();
        let outside = tmp.path().join("outside.txt");
        fs::write(&outside, "precious").unwrap();
        let name = format!("runtime-trace.tmp.1.{}", nanos_ago(ORPHAN_MIN_AGE * 4));
        assert!(parse_temp_sibling(STEM, &name).is_some());
        let link = dir.join(&name);
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        let report = sweep_orphans(&active_in(&dir), NOW, ORPHAN_MIN_AGE, &NO_PAUSE).unwrap();

        assert_eq!(report.deleted, 0);
        assert_eq!(report.kept_not_regular, 1);
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link itself must still be there"
        );
        assert_eq!(fs::read_to_string(&outside).unwrap(), "precious");
    }

    #[test]
    fn a_directory_with_an_orphan_name_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let name = format!("runtime-trace.tmp.1.{}", nanos_ago(ORPHAN_MIN_AGE * 4));
        assert!(parse_temp_sibling(STEM, &name).is_some());
        let dir_named_like_an_orphan = tmp.path().join(&name);
        fs::create_dir(&dir_named_like_an_orphan).unwrap();
        fs::write(dir_named_like_an_orphan.join("inner"), "keep").unwrap();

        let report = sweep_orphans(&active_in(tmp.path()), NOW, ORPHAN_MIN_AGE, &NO_PAUSE).unwrap();

        assert_eq!(report.deleted, 0);
        assert_eq!(report.kept_not_regular, 1);
        assert_eq!(
            fs::read_to_string(dir_named_like_an_orphan.join("inner")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn removing_a_file_that_is_already_gone_is_not_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = seed(tmp.path(), "runtime-trace.tmp.1.2");

        assert!(remove_if_present(&path).unwrap());
        assert!(!path.exists());
        assert!(
            !remove_if_present(&path).unwrap(),
            "a file another sweep or its own guard already removed is not a failure"
        );
    }

    #[test]
    fn thousands_of_old_orphans_are_all_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let active = active_in(tmp.path());
        fs::write(&active, "log\n").unwrap();
        let old = nanos_ago(ORPHAN_MIN_AGE * 4);
        for pid in 0..5000 {
            seed(tmp.path(), &format!("runtime-trace.tmp.{pid}.{old}"));
        }

        let report = sweep_orphans(&active, NOW, ORPHAN_MIN_AGE, &NO_PAUSE).unwrap();

        assert_eq!(report.deleted, 5000);
        assert_eq!(report.errors, 0);
        assert_eq!(names(tmp.path()), only(&["runtime-trace.jsonl"]));
    }

    #[test]
    fn a_directory_that_does_not_exist_yet_has_nothing_to_sweep() {
        // A fresh install has no log directory when the writer starts; the
        // writer creates it on its first write.
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("no-such-dir").join("runtime-trace.jsonl");

        let report = sweep_orphans(&missing, NOW, ORPHAN_MIN_AGE, &NO_PAUSE).unwrap();

        assert_eq!(report, SweepReport::default());
    }

    #[test]
    fn a_directory_that_cannot_be_listed_is_an_error_not_a_panic() {
        // The "directory" is a regular file, so listing it fails for a reason
        // other than not existing.
        let tmp = tempfile::tempdir().unwrap();
        let not_a_dir = tmp.path().join("state");
        fs::write(&not_a_dir, "a file where the log directory should be").unwrap();
        let active = not_a_dir.join("runtime-trace.jsonl");

        assert!(sweep_orphans(&active, NOW, ORPHAN_MIN_AGE, &NO_PAUSE).is_err());
    }

    #[test]
    fn only_one_sweep_runs_at_a_time() {
        let _guard = crate::writer::WRITER_TEST_LOCK.lock();

        let first = try_begin_sweep().expect("no sweep is running yet");
        assert!(try_begin_sweep().is_none(), "a second sweep must not start");
        drop(first);
        assert!(try_begin_sweep().is_some(), "the slot is free again");
    }

    #[test]
    fn under_contention_no_two_sweeps_ever_hold_the_slot_at_once() {
        // Threads fight over the slot in a tight loop. The race that matters is
        // at each release: every waiting thread sees a free slot at the same
        // moment, and a check-then-set slot lets several of them through. Each
        // winner announces itself in `inside`, works for a moment, and leaves
        // `inside` before releasing the slot, so with a correct slot `inside`
        // is never above one.
        const THREADS: usize = 8;
        const TARGET: usize = 4000;
        let _guard = crate::writer::WRITER_TEST_LOCK.lock();
        let inside = AtomicUsize::new(0);
        let overlaps = AtomicUsize::new(0);
        let completed = AtomicUsize::new(0);
        let deadline = Instant::now() + Duration::from_secs(30);

        thread::scope(|scope| {
            for _ in 0..THREADS {
                scope.spawn(|| {
                    while completed.load(Ordering::Relaxed) < TARGET && Instant::now() < deadline {
                        let Some(slot) = try_begin_sweep() else {
                            std::hint::spin_loop();
                            continue;
                        };
                        if inside.fetch_add(1, Ordering::SeqCst) != 0 {
                            overlaps.fetch_add(1, Ordering::SeqCst);
                        }
                        for _ in 0..200 {
                            std::hint::spin_loop();
                        }
                        inside.fetch_sub(1, Ordering::SeqCst);
                        completed.fetch_add(1, Ordering::Relaxed);
                        drop(slot);
                    }
                });
            }
        });

        assert_eq!(
            overlaps.load(Ordering::SeqCst),
            0,
            "two sweeps held the slot at the same time"
        );
        assert!(
            completed.load(Ordering::Relaxed) >= TARGET,
            "the threads must have taken the slot {TARGET} times; did they hit the deadline?"
        );
        assert!(
            try_begin_sweep().is_some(),
            "the slot is free once the last winner is gone"
        );
    }

    /// Installs a callback that runs for each candidate right before its
    /// removal, and clears it on drop so a failing assertion cannot leak it
    /// into a later test on the same thread.
    struct BeforeRemove;

    impl BeforeRemove {
        fn install(callback: impl FnMut(&Path) + 'static) -> Self {
            BEFORE_REMOVE.with(|slot| *slot.borrow_mut() = Some(Box::new(callback)));
            Self
        }
    }

    impl Drop for BeforeRemove {
        fn drop(&mut self) {
            BEFORE_REMOVE.with(|slot| *slot.borrow_mut() = None);
        }
    }

    fn is_named(path: &Path, name: &str) -> bool {
        path.file_name().and_then(|n| n.to_str()) == Some(name)
    }

    #[test]
    fn a_file_removed_between_listing_and_deletion_is_not_a_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let active = active_in(tmp.path());
        let raced = format!("runtime-trace.tmp.1.{}", nanos_ago(ORPHAN_MIN_AGE * 4));
        let other = format!("runtime-trace.tmp.2.{}", nanos_ago(ORPHAN_MIN_AGE * 4));
        seed(tmp.path(), &raced);
        seed(tmp.path(), &other);
        // Another sweep, or the temp file's own guard, removes it after it was
        // listed and judged an orphan but before this sweep deletes it.
        let _hook = BeforeRemove::install({
            let raced = raced.clone();
            move |path| {
                if is_named(path, &raced) {
                    fs::remove_file(path).unwrap();
                }
            }
        });

        let report = sweep_orphans(&active, NOW, ORPHAN_MIN_AGE, &NO_PAUSE).unwrap();

        assert_eq!(report.errors, 0, "a vanished file is not an error");
        assert_eq!(report.already_gone, 1);
        assert_eq!(report.deleted, 1, "the other orphan is still removed");
        assert!(names(tmp.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_file_swapped_for_a_symlink_before_deletion_costs_only_the_link() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("logs");
        fs::create_dir(&dir).unwrap();
        let outside = tmp.path().join("outside.txt");
        fs::write(&outside, "precious").unwrap();
        let name = format!("runtime-trace.tmp.1.{}", nanos_ago(ORPHAN_MIN_AGE * 4));
        seed(&dir, &name);
        // Someone who can write into the log directory swaps the regular file
        // for a link after the sweep checked it. Removing a link never follows
        // it, so only the link may go.
        let _hook = BeforeRemove::install({
            let (name, outside) = (name.clone(), outside.clone());
            move |path| {
                if is_named(path, &name) {
                    fs::remove_file(path).unwrap();
                    std::os::unix::fs::symlink(&outside, path).unwrap();
                }
            }
        });

        let report = sweep_orphans(&active_in(&dir), NOW, ORPHAN_MIN_AGE, &NO_PAUSE).unwrap();

        assert_eq!(report.deleted, 1);
        assert_eq!(fs::read_to_string(&outside).unwrap(), "precious");
        assert!(
            names(&dir).is_empty(),
            "the link was the only thing removed"
        );
    }

    #[test]
    fn a_file_swapped_for_a_directory_before_deletion_survives_and_does_not_stop_the_sweep() {
        let tmp = tempfile::tempdir().unwrap();
        let active = active_in(tmp.path());
        let swapped = format!("runtime-trace.tmp.1.{}", nanos_ago(ORPHAN_MIN_AGE * 4));
        let other = format!("runtime-trace.tmp.2.{}", nanos_ago(ORPHAN_MIN_AGE * 4));
        seed(tmp.path(), &swapped);
        seed(tmp.path(), &other);
        let _hook = BeforeRemove::install({
            let swapped = swapped.clone();
            move |path| {
                if is_named(path, &swapped) {
                    fs::remove_file(path).unwrap();
                    fs::create_dir(path).unwrap();
                    fs::write(path.join("inner"), "keep").unwrap();
                }
            }
        });

        let report = sweep_orphans(&active, NOW, ORPHAN_MIN_AGE, &NO_PAUSE).unwrap();

        assert_eq!(report.errors, 1, "the removal that failed is counted");
        assert_eq!(report.deleted, 1, "the sweep goes on to the next orphan");
        assert_eq!(names(tmp.path()), BTreeSet::from([swapped.clone()]));
        assert_eq!(
            fs::read_to_string(tmp.path().join(&swapped).join("inner")).unwrap(),
            "keep"
        );
    }

    fn seed_old_orphans(dir: &Path, count: u32) {
        let old = nanos_ago(ORPHAN_MIN_AGE * 4);
        for pid in 0..count {
            seed(dir, &format!("runtime-trace.tmp.{pid}.{old}"));
        }
    }

    #[test]
    fn the_sweep_pauses_after_every_nth_removal() {
        let tmp = tempfile::tempdir().unwrap();
        seed_old_orphans(tmp.path(), 600);
        let throttle = Throttle {
            every: 256,
            pause: Duration::from_millis(7),
        };
        let mut pauses = Vec::new();

        let report = sweep_with_pause(
            &active_in(tmp.path()),
            NOW,
            ORPHAN_MIN_AGE,
            &throttle,
            |pause| pauses.push(pause),
        )
        .unwrap();

        assert_eq!(report.deleted, 600);
        assert_eq!(
            pauses,
            vec![Duration::from_millis(7); 2],
            "600 removals with a pause every 256 pause after the 256th and the 512th"
        );
    }

    #[test]
    fn a_throttle_of_zero_never_pauses_and_never_divides_by_zero() {
        let tmp = tempfile::tempdir().unwrap();
        seed_old_orphans(tmp.path(), 10);
        let throttle = Throttle {
            every: 0,
            pause: Duration::from_millis(7),
        };
        let mut pauses = Vec::new();

        let report = sweep_with_pause(
            &active_in(tmp.path()),
            NOW,
            ORPHAN_MIN_AGE,
            &throttle,
            |pause| pauses.push(pause),
        )
        .unwrap();

        assert_eq!(report.deleted, 10);
        assert!(pauses.is_empty());
    }

    /// Real-clock variant of the seeding, for the paths that read the clock
    /// themselves instead of taking `now` as a parameter.
    fn real_nanos_ago(age: Duration) -> i64 {
        chrono::Utc::now().timestamp_nanos_opt().unwrap() - i64::try_from(age.as_nanos()).unwrap()
    }

    #[test]
    fn the_background_thread_removes_orphans_and_frees_the_slot() {
        let _guard = crate::writer::WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        let active = active_in(tmp.path());
        let old = format!(
            "runtime-trace.tmp.99999.{}",
            real_nanos_ago(ORPHAN_MIN_AGE * 4)
        );
        let fresh = format!(
            "runtime-trace.tmp.99999.{}",
            real_nanos_ago(Duration::from_secs(60))
        );
        seed(tmp.path(), &old);
        seed(tmp.path(), &fresh);
        let slot = try_begin_sweep().expect("no sweep is running yet");

        let handle = start_sweep_thread(active, slot).unwrap();
        handle.join().expect("the sweep thread must not panic");

        assert_eq!(names(tmp.path()), BTreeSet::from([fresh]));
        assert!(
            try_begin_sweep().is_some(),
            "a finished sweep must free the slot"
        );
    }

    #[test]
    fn the_background_thread_survives_an_unreadable_directory_and_frees_the_slot() {
        let _guard = crate::writer::WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        let not_a_dir = tmp.path().join("state");
        fs::write(&not_a_dir, "a file where the log directory should be").unwrap();
        let active = not_a_dir.join("runtime-trace.jsonl");
        let slot = try_begin_sweep().expect("no sweep is running yet");

        let handle = start_sweep_thread(active, slot).unwrap();

        handle
            .join()
            .expect("a failed listing must be logged, not panic");
        assert!(try_begin_sweep().is_some());
    }

    #[test]
    fn a_sweep_requested_while_another_runs_is_skipped() {
        let _guard = crate::writer::WRITER_TEST_LOCK.lock();
        let tmp = tempfile::tempdir().unwrap();
        let active = active_in(tmp.path());
        let old = format!(
            "runtime-trace.tmp.99999.{}",
            real_nanos_ago(ORPHAN_MIN_AGE * 4)
        );
        seed(tmp.path(), &old);
        let running = try_begin_sweep().expect("no sweep is running yet");

        spawn_orphan_sweep(&active);
        assert!(
            tmp.path().join(&old).exists(),
            "a config reload during a sweep must not start a second one"
        );

        drop(running);
        spawn_orphan_sweep(&active);
        assert!(
            !tmp.path().join(&old).exists(),
            "once the first sweep is done the next request runs"
        );
    }
}
