//! Helpers that the tests of several modules share, so no production module has
//! to carry scaffolding for another module's tests.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::orphan_sweep::{ORPHAN_MIN_AGE, Throttle, sweep_orphans};

/// Set the last-modified time of `path`, for tests that need a file to look as
/// if it was last written at a given moment.
pub(crate) fn set_modified(path: &Path, time: SystemTime) {
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .and_then(|file| file.set_modified(time))
        .unwrap_or_else(|err| panic!("setting the modification time of {}: {err}", path.display()));
}

/// For a test that runs a sweep inside a trim or a migration: an action that
/// sweeps the directory of `active` once, with a clock `ahead` of the real one,
/// and the count of files it removed. A clock far enough ahead makes a live
/// temp file look abandoned by both of its readings.
///
/// With `still_written` the action first stamps the temp file the writer just
/// created as modified one minute before the swept "now": a writer that has run
/// for a long time and is still writing, seen by a sweep that has caught up
/// with it.
pub(crate) fn sweep_ahead_action(
    active: &Path,
    ahead: Duration,
    still_written: bool,
) -> (impl FnMut() + 'static, std::rc::Rc<std::cell::Cell<usize>>) {
    let removed = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let seen = std::rc::Rc::clone(&removed);
    let active = active.to_path_buf();
    let action = move || {
        let now = chrono::Utc::now().timestamp_nanos_opt().unwrap()
            + i64::try_from(ahead.as_nanos()).unwrap();
        if still_written {
            let temp = crate::temp_guard::last_created_temp()
                .expect("the writer created its temp file before this action runs");
            let modified = UNIX_EPOCH
                + Duration::from_nanos(u64::try_from(now).unwrap())
                    .saturating_sub(Duration::from_secs(60));
            set_modified(&temp, modified);
        }
        let no_pause = Throttle {
            every: 0,
            pause: Duration::ZERO,
        };
        let report = sweep_orphans(&active, now, ORPHAN_MIN_AGE, &no_pause).unwrap();
        seen.set(report.deleted);
    };
    (action, removed)
}

/// What a fmt subscriber writes, so a test can read the log lines it emitted.
#[derive(Clone, Default)]
pub(crate) struct CapturedWarnings(Arc<parking_lot::Mutex<Vec<u8>>>);

impl CapturedWarnings {
    pub(crate) fn text(&self) -> String {
        String::from_utf8(self.0.lock().clone()).unwrap()
    }
}

impl std::io::Write for CapturedWarnings {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedWarnings {
    type Writer = CapturedWarnings;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}
