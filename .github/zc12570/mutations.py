#!/usr/bin/env python3
"""Mutations of the orphan-sweep change, for a throwaway verification workflow.

    mutations.py list
    mutations.py check             every pattern occurs exactly once (no build)
    mutations.py apply  <id>       edit the working tree
    mutations.py verdict <id> <log> <rc>

A mutation is detected when the test run fails because a test failed, not
because the mutated code does not compile. CONTROL applies nothing and must
pass.
"""

import pathlib
import re
import sys

SWEEP = "crates/zeroclaw-log/src/orphan_sweep.rs"
GUARD = "crates/zeroclaw-log/src/temp_guard.rs"
WRITER = "crates/zeroclaw-log/src/writer.rs"

NAME_FILTER = """        if age < threshold {
            report.kept_recent += 1;
            continue;
        }
"""
IDLE_FILTER = """        if idle < threshold {
            report.kept_recent += 1;
            continue;
        }
"""
MIGRATION_WARN = (
    '"log: legacy JSONL migration failed; daemon continuing with mixed-shape file"'
)
SLOT = """pub(crate) fn try_begin_sweep() -> Option<SweepSlot> {
    SWEEP_RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .ok()
        .map(|_| SweepSlot(()))
}"""
SLOT_NOT_ATOMIC = """pub(crate) fn try_begin_sweep() -> Option<SweepSlot> {
    if SWEEP_RUNNING.load(Ordering::Acquire) {
        None
    } else {
        SWEEP_RUNNING.store(true, Ordering::Release);
        Some(SweepSlot(()))
    }
}"""
ZERO_STAMP = """        if stamp_nanos == 0 {
            report.kept_future += 1;
            continue;
        }
"""
FUTURE_MTIME = (
    "            .and_then(|modified_nanos| elapsed_since(now_nanos, modified_nanos))\n"
)

# id: (what it breaks, [(file, old, new)], expected failing tests (any of))
MUTATIONS = {
    "M1": (
        "no filter on the age in the name",
        [(SWEEP, NAME_FILTER, "        let _ = threshold;\n")],
        ["a_recent_name_on_a_file_that_was_modified_long_ago_is_kept"],
    ),
    "M2": (
        "accepts any kind of entry",
        [(SWEEP, "            Ok(kind) if kind.is_file() => {}", "            Ok(_) => {}")],
        ["a_directory_with_an_orphan_name_is_left_alone"],
    ),
    "M3": (
        "the name reader accepts extra segments",
        [
            (
                GUARD,
                "    if parts.next().is_some() {\n        return None;\n    }\n",
                "",
            )
        ],
        ["only_the_exact_orphan_shape_is_ever_touched"],
    ),
    "M3b": (
        "the name reader does not require ASCII digits",
        [
            (
                GUARD,
                "    ascii_digits(pid_part)?.parse::<u32>().ok()?;",
                "    pid_part.parse::<u32>().ok()?;",
            ),
            (
                GUARD,
                "    ascii_digits(nanos_part)?.parse().ok()",
                "    nanos_part.parse().ok()",
            ),
        ],
        ["only_the_exact_orphan_shape_is_ever_touched"],
    ),
    "M4": (
        "the one-sweep slot is not atomic",
        [(SWEEP, SLOT, SLOT_NOT_ATOMIC)],
        ["under_contention_no_two_sweeps_ever_hold_the_slot_at_once"],
    ),
    "M5": (
        "no check for a name stamped 0",
        [(SWEEP, ZERO_STAMP, "")],
        ["a_name_stamped_zero_is_kept_because_its_age_is_unknown"],
    ),
    "M6": (
        "the name check never keeps",
        [(SWEEP, "        if age < threshold {", "        if false {")],
        ["a_recent_name_on_a_file_that_was_modified_long_ago_is_kept"],
    ),
    "M7": (
        "the name check always keeps",
        [(SWEEP, "        if age < threshold {", "        if true {")],
        ["old_trim_and_migration_orphans_of_any_pid_are_removed"],
    ),
    "M10": (
        "no check of the modification time",
        [(SWEEP, IDLE_FILTER, "        let _ = idle;\n")],
        ["an_old_name_on_a_file_that_was_modified_recently_is_kept"],
    ),
    "M11": (
        "the idle threshold is exclusive",
        [(SWEEP, "        if idle < threshold {", "        if idle <= threshold {")],
        ["the_idle_threshold_is_inclusive"],
    ),
    "M12": (
        "a modification time in the future counts as idle",
        [
            (
                SWEEP,
                FUTURE_MTIME,
                "            .map(|modified_nanos| {\n"
                "                elapsed_since(now_nanos, modified_nanos).unwrap_or(Duration::MAX)\n"
                "            })\n",
            )
        ],
        ["a_modification_time_in_the_future_is_kept"],
    ),
    "M13": (
        "the name threshold is exclusive",
        [(SWEEP, "        if age < threshold {", "        if age <= threshold {")],
        ["a_recent_orphan_survives_and_the_threshold_is_inclusive"],
    ),
    "M14": (
        "the thread-start failure is not reported",
        [
            (
                SWEEP,
                '            "log: could not start the orphan temp-file sweep thread"',
                '            "log: mutation"',
            )
        ],
        ["a_sweep_thread_that_cannot_start_is_a_warning_and_leaves_the_slot_free"],
    ),
    "M8": (
        "startup panics when the migration fails",
        [(WRITER, MIGRATION_WARN + "\n        );", MIGRATION_WARN + '\n        );\n        panic!("mutation");')],
        ["startup_goes_on_when_the_migration_fails_and_the_next_start_completes_it"],
    ),
    "M9": (
        "startup does not say that the migration failed",
        [(WRITER, MIGRATION_WARN, '"log: mutation"')],
        ["startup_goes_on_when_the_migration_fails_and_the_next_start_completes_it"],
    ),
    "CONTROL": ("no change; must pass", [], []),
}


def read(path):
    return pathlib.Path(path).read_text(encoding="utf-8")


def check():
    bad = 0
    for ident, (_, edits, _) in MUTATIONS.items():
        for path, old, _ in edits:
            n = read(path).count(old)
            if n != 1:
                print(f"{ident}: pattern in {path} occurs {n} times: {old[:70]!r}")
                bad += 1
    print("every pattern occurs exactly once" if not bad else f"{bad} bad pattern(s)")
    return 1 if bad else 0


def apply(ident):
    what, edits, _ = MUTATIONS[ident]
    print(f"{ident}: {what}")
    for path, old, new in edits:
        text = read(path)
        n = text.count(old)
        if n != 1:
            print(f"ABORT: pattern in {path} occurs {n} times")
            return 4
        pathlib.Path(path).write_text(text.replace(old, new), encoding="utf-8")
    return 0


def verdict(ident, log, rc):
    what, _, expected = MUTATIONS[ident]
    text = read(log)
    failed = sorted(set(re.findall(r"^test (\S+) \.\.\. FAILED$", text, re.M)))
    compile_error = bool(re.search(r"^error(\[E\d+\])?:", text, re.M)) and not failed
    lines = [f"### {ident}: {what}", f"cargo exit code: {rc}"]
    if ident == "CONTROL":
        ok = rc == 0 and not failed
        lines.append("PASS: the unmutated tree passes" if ok else "FAIL: the control fails")
    elif compile_error:
        ok = False
        lines.append("FAIL: the mutated code does not compile (not a detection)")
    elif rc == 0:
        ok = False
        lines.append("FAIL: NOT DETECTED, every test passed on the mutated code")
    else:
        ok = bool(failed)
        lines.append(f"DETECTED by {len(failed)} test(s):")
        lines += [f"- {name}" for name in failed]
        hit = [name for name in failed if any(e in name for e in expected)]
        lines.append(
            "the expected test is among them" if hit else
            f"NOTE: none of the expected tests failed ({expected})"
        )
    out = "\n".join(lines)
    print(out)
    summary = pathlib.Path(__import__("os").environ.get("GITHUB_STEP_SUMMARY", "/dev/null"))
    with summary.open("a", encoding="utf-8") as handle:
        handle.write(out + "\n\n")
    return 0 if ok else 1


def main(argv):
    cmd = argv[1] if len(argv) > 1 else ""
    if cmd == "list":
        print(" ".join(MUTATIONS))
        return 0
    if cmd == "check":
        return check()
    if cmd == "apply" and len(argv) == 3 and argv[2] in MUTATIONS:
        return apply(argv[2])
    if cmd == "verdict" and len(argv) == 5 and argv[2] in MUTATIONS:
        return verdict(argv[2], argv[3], int(argv[4]))
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
