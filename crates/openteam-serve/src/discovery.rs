//! Run discovery and the **finished / live / aborted** classifier (ADR 0027).
//!
//! Discovery is filesystem-based over one root (`--dir`): each immediate
//! subdirectory with a readable `run_started` header (event 0) is a run, keyed
//! by the header's `run_id`. Classification is the bookend × flock trichotomy:
//!
//! | state        | `run_finished` bookend | `run.lock` flock |
//! |--------------|------------------------|------------------|
//! | **finished** | present                | (irrelevant)     |
//! | **live**     | absent                 | held             |
//! | **aborted**  | absent                 | free / missing   |
//!
//! `flock(2)`'s per-open-file-description semantics let the reader test the
//! lock with its own `open()`: a lock still held by `openteam run` (or by a
//! same-process test writer, ADR 0030) fails a non-blocking acquire.

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use openteam_core::{Event, EventKind, RunId};

use crate::tail::read_complete_lines;

/// A run's liveness (ADR 0027), surfaced lowercase on the wire (ADR 0028).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunState {
    Live,
    Finished,
    Aborted,
}

impl RunState {
    /// The lowercase wire token (`"live" | "finished" | "aborted"`, pins §9).
    pub(crate) fn as_wire_str(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Finished => "finished",
            Self::Aborted => "aborted",
        }
    }

    /// Terminal runs (finished or aborted) get a 204 on a caught-up SSE
    /// connect (ADR 0028) — consumed by the stream endpoint in step 6.
    #[allow(dead_code)]
    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::Finished | Self::Aborted)
    }
}

/// The `run_started` header of a run (event 0), plus the raw `data` object so
/// the snapshot's `run` block can carry the fields verbatim (ADR 0028).
#[derive(Debug, Clone)]
pub(crate) struct RunHeader {
    pub(crate) run_id: RunId,
    pub(crate) seed: u64,
    pub(crate) goal: String,
    /// Event 0's informational `at` breadcrumb — the list's `started_at`.
    pub(crate) started_at: Timestamp,
    /// The `run_started.data` object verbatim (ADR 0022), reused for the
    /// snapshot's `run` block.
    pub(crate) data: serde_json::Value,
}

/// One discovered run: its directory and parsed header.
#[derive(Debug, Clone)]
pub(crate) struct DiscoveredRun {
    pub(crate) path: PathBuf,
    pub(crate) header: RunHeader,
}

/// Parse a run's `run_started` header from event 0; `None` if the file is
/// missing, empty, or event 0 is not a parseable `run_started` (not a run).
pub(crate) fn run_header(dir: &Path) -> Option<RunHeader> {
    let lines = read_complete_lines(&dir.join("events.jsonl")).ok()?;
    let first = lines.first()?;
    let event: Event = serde_json::from_slice(first).ok()?;
    let EventKind::RunStarted {
        run_id, seed, goal, ..
    } = &event.kind
    else {
        return None;
    };
    // The raw `data` object of event 0 — verbatim run_started fields.
    let raw: serde_json::Value = serde_json::from_slice(first).ok()?;
    let data = raw.get("data")?.clone();
    Some(RunHeader {
        run_id: *run_id,
        seed: *seed,
        goal: goal.clone(),
        started_at: event.at,
        data,
    })
}

/// Discover every run under `root`, sorted by `run_id` ascending — UUIDv7 ⇒
/// chronological (ADR 0022/0028). Directories without a valid header are
/// silently skipped (not runs).
pub(crate) fn discover(root: &Path) -> Vec<DiscoveredRun> {
    let mut runs: Vec<DiscoveredRun> = match std::fs::read_dir(root) {
        Ok(entries) => entries
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| {
                let path = entry.path();
                run_header(&path).map(|header| DiscoveredRun { path, header })
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    runs.sort_by_key(|run| run.header.run_id);
    runs
}

/// Locate one run by `run_id`. The default layout names each dir by its
/// `run_id` (ADR 0027; `--out-dir` runs are out of scope), so this is an
/// O(1) join validated against the header — no directory scan, and a
/// non-UUID `run_id` (or path-traversal attempt) fails to parse and 404s.
pub(crate) fn find_run(root: &Path, run_id: &str) -> Option<DiscoveredRun> {
    let parsed: RunId = run_id.parse().ok()?;
    let path = root.join(parsed.to_string());
    let header = run_header(&path)?;
    (header.run_id == parsed).then_some(DiscoveredRun { path, header })
}

/// Parse a run's complete events in order (unparseable lines skipped). The
/// snapshot fold and the SSE stream both read through here.
pub(crate) fn read_events(dir: &Path) -> Vec<Event> {
    read_complete_lines(&dir.join("events.jsonl"))
        .unwrap_or_default()
        .iter()
        .filter_map(|line| serde_json::from_slice(line).ok())
        .collect()
}

/// The last **complete** event as a raw JSON value — the run list's
/// `last_event_id` and (for finished runs) `finished` block read from here.
/// `None` when the log has no complete events yet.
pub(crate) fn last_event(dir: &Path) -> Option<serde_json::Value> {
    let lines = read_complete_lines(&dir.join("events.jsonl")).ok()?;
    serde_json::from_slice(lines.last()?).ok()
}

/// Complete events as `(EventId, verbatim line)` pairs — the SSE stream's
/// resume-filtered replay source. The line is the byte-verbatim `events.jsonl`
/// line (ADR 0028/0030's byte-golden `data:` payload); lines that aren't UTF-8
/// or lack a numeric `id` are skipped.
pub(crate) fn read_event_lines(dir: &Path) -> Vec<(u64, String)> {
    read_complete_lines(&dir.join("events.jsonl"))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|bytes| {
            let line = String::from_utf8(bytes).ok()?;
            let id = event_id(&line)?;
            Some((id, line))
        })
        .collect()
}

/// The `id` of one `events.jsonl` line, parsed cheaply (the live tailer's
/// per-line id for resume/dedupe arithmetic).
pub(crate) fn event_id(line: &str) -> Option<u64> {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()?
        .get("id")?
        .as_u64()
}

/// Classify a run directory's state (ADR 0027): bookend first, then flock.
pub(crate) fn classify(dir: &Path) -> RunState {
    if has_finished_bookend(dir) {
        RunState::Finished
    } else if lock_is_held(dir) {
        RunState::Live
    } else {
        RunState::Aborted
    }
}

/// True when the last *complete* line of `events.jsonl` is the `run_finished`
/// bookend (a torn final line is not a bookend — complete-line rule).
fn has_finished_bookend(dir: &Path) -> bool {
    let Ok(lines) = read_complete_lines(&dir.join("events.jsonl")) else {
        return false;
    };
    lines.last().is_some_and(|line| {
        serde_json::from_slice::<Event>(line)
            .is_ok_and(|event| matches!(event.kind, EventKind::RunFinished { .. }))
    })
}

/// True when `run.lock` exists and its exclusive advisory lock is currently
/// **held** by another open file description (i.e. `openteam run` is live, or a
/// same-process test writer holds it). A missing file, or one we can acquire,
/// is *not held* → the run aborted (ADR 0027/0030).
fn lock_is_held(dir: &Path) -> bool {
    let Ok(file) = std::fs::File::open(dir.join("run.lock")) else {
        return false;
    };
    match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        // We acquired it — it was free. Dropping `file` releases immediately.
        Ok(()) => false,
        // Contended: another open file description holds it → the run is live.
        Err(rustix::io::Errno::WOULDBLOCK) => true,
        // Any other error is unexpected for an openable lock file; treat as
        // not-held so a stuck reader can never falsely report a run live.
        Err(err) => {
            tracing::warn!(%err, dir = %dir.display(), "unexpected run.lock flock error");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustix::fs::{FlockOperation, flock};
    use std::fs::File;

    /// The canonical run_started header line for a fixed run_id.
    fn run_started_line(run_id: &str) -> String {
        format!(
            "{{\"id\":0,\"at\":\"2026-07-17T00:00:00Z\",\"source\":\"system\",\
             \"kind\":\"run_started\",\"data\":{{\"run_id\":\"{run_id}\",\"seed\":42,\
             \"goal\":\"g\",\"agents\":1,\"meta_agents\":0,\"parallel\":1,\
             \"scenario\":null,\"caps\":{{}}}}}}"
        )
    }

    fn run_finished_line() -> &'static str {
        "{\"id\":1,\"at\":\"2026-07-17T00:00:00Z\",\"source\":\"orchestrator\",\
         \"kind\":\"run_finished\",\"data\":{\"reason\":\"CleanFinish\",\"exit_code\":0}}"
    }

    const RUN_ID: &str = "0192f1a0-7e3c-7abc-9def-000000000000";

    /// Build a run dir named by `RUN_ID` with the given events.jsonl content.
    fn run_dir_with(root: &Path, events: &str) -> PathBuf {
        let dir = root.join(RUN_ID);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("events.jsonl"), events).unwrap();
        dir
    }

    #[test]
    fn classifier_reports_finished_when_the_bookend_is_present() {
        let root = tempfile::tempdir().unwrap();
        let events = format!("{}\n{}\n", run_started_line(RUN_ID), run_finished_line());
        let dir = run_dir_with(root.path(), &events);
        // Bookend wins regardless of the lock (no run.lock here at all).
        assert_eq!(classify(&dir), RunState::Finished);
    }

    #[test]
    fn classifier_reports_live_while_the_lock_is_held() {
        let root = tempfile::tempdir().unwrap();
        let dir = run_dir_with(root.path(), &format!("{}\n", run_started_line(RUN_ID)));
        // The test IS the writer: hold run.lock exactly as `openteam run`
        // does (a same-process open conflicts under flock, ADR 0030).
        let held = File::create(dir.join("run.lock")).unwrap();
        flock(&held, FlockOperation::LockExclusive).unwrap();
        assert_eq!(classify(&dir), RunState::Live);

        // Drop the lock with no bookend → aborted.
        drop(held);
        assert_eq!(classify(&dir), RunState::Aborted);
    }

    #[test]
    fn classifier_reports_aborted_when_no_lock_and_no_bookend() {
        let root = tempfile::tempdir().unwrap();
        let dir = run_dir_with(root.path(), &format!("{}\n", run_started_line(RUN_ID)));
        // No run.lock file at all, no bookend → the run died.
        assert_eq!(classify(&dir), RunState::Aborted);
    }

    #[test]
    fn discovery_lists_runs_sorted_by_run_id_and_skips_non_runs() {
        let root = tempfile::tempdir().unwrap();
        // Two valid runs (UUIDv7 ids sort chronologically).
        let earlier = "0192f1a0-0000-7000-8000-000000000000";
        let later = "0192f1a0-ffff-7000-8000-000000000000";
        for id in [later, earlier] {
            let dir = root.path().join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("events.jsonl"),
                format!("{}\n", run_started_line(id)),
            )
            .unwrap();
        }
        // A non-run subdir (no run_started header) is skipped.
        let junk = root.path().join("not-a-run");
        std::fs::create_dir_all(&junk).unwrap();
        std::fs::write(junk.join("events.jsonl"), "garbage\n").unwrap();

        let runs = discover(root.path());
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].header.run_id.to_string(), earlier);
        assert_eq!(runs[1].header.run_id.to_string(), later);

        // Lookup by id resolves to the right dir; a non-UUID id is rejected.
        assert!(find_run(root.path(), earlier).is_some());
        assert!(find_run(root.path(), "../etc").is_none());
        assert!(find_run(root.path(), "0192f1a0-dead-7000-8000-000000000000").is_none());
    }

    #[test]
    fn header_carries_verbatim_run_started_data() {
        let root = tempfile::tempdir().unwrap();
        let dir = run_dir_with(root.path(), &format!("{}\n", run_started_line(RUN_ID)));
        let header = run_header(&dir).unwrap();
        assert_eq!(header.run_id.to_string(), RUN_ID);
        assert_eq!(header.seed, 42);
        assert_eq!(header.goal, "g");
        assert_eq!(header.data["run_id"], RUN_ID);
        assert_eq!(header.data["caps"], serde_json::json!({}));
    }

    #[test]
    fn bookend_check_ignores_a_torn_final_line() {
        let root = tempfile::tempdir().unwrap();
        // run_finished present but its line is torn (no trailing newline) →
        // not yet a bookend; falls through to the flock test → aborted.
        let events = format!("{}\n{}", run_started_line(RUN_ID), run_finished_line());
        let dir = run_dir_with(root.path(), &events);
        assert_eq!(classify(&dir), RunState::Aborted);
    }
}
