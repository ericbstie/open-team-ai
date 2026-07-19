//! Run artifacts: the streamed `events.jsonl` spine plus the finalized
//! snapshots written on every termination path (ADR 0022).

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::board::{Board, Task, Team};
use crate::event::Event;
use crate::ids::RunId;
use crate::knowledge::KnowledgeEntry;

/// The append+flush-per-event `events.jsonl` writer — complete up to the
/// last committed event even on a cap-forced or crash kill (ADR 0022).
#[derive(Debug)]
pub(crate) struct EventsWriter {
    writer: BufWriter<File>,
}

impl EventsWriter {
    pub(crate) fn create(dir: &Path) -> std::io::Result<Self> {
        let file = File::create(dir.join("events.jsonl"))?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    pub(crate) fn append(&mut self, event: &Event) -> std::io::Result<()> {
        let line = serde_json::to_string(event)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()
    }
}

/// Resolve and create the run directory: the `--out-dir` override, else
/// `.openteam/runs/<run-id>/` (cwd-relative, ADR 0022).
pub(crate) fn create_run_dir(out_dir: Option<&Path>, run_id: RunId) -> std::io::Result<PathBuf> {
    let dir = match out_dir {
        Some(dir) => dir.to_path_buf(),
        None => PathBuf::from(".openteam")
            .join("runs")
            .join(run_id.to_string()),
    };
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// The `board.json` snapshot (ADR 0022) — an **owned, public** type so the
/// stream server can fold `events.jsonl` into a `Board` and serialize the
/// identical shape (ADR 0030's second granted core change). Serialized from a
/// struct — not a `serde_json::Value`, whose object keys sort alphabetically —
/// so keys emit in the transcript's pinned field order: `run_id`, `goal`,
/// `seed`, `tasks`, `teams` (dry-run transcript §8).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BoardSnapshot {
    pub run_id: RunId,
    pub goal: String,
    pub seed: u64,
    pub tasks: Vec<Task>,
    pub teams: Vec<Team>,
}

impl BoardSnapshot {
    /// Build the snapshot from a live [`Board`] (ADR 0022): tasks in id
    /// (== creation) order, teams in formation order, dissolved included.
    /// `Task` and `Team` declare their fields in the transcript's §8 order, so
    /// struct serialization preserves it end to end.
    pub fn new(run_id: RunId, goal: impl Into<String>, seed: u64, board: &Board) -> Self {
        let mut tasks: Vec<Task> = board.tasks().cloned().collect();
        tasks.sort_by_key(|t| t.id);
        Self {
            run_id,
            goal: goal.into(),
            seed,
            tasks,
            teams: board.teams().to_vec(),
        }
    }
}

/// Build the final `board.json` snapshot (ADR 0022) — the runtime's call site;
/// [`BoardSnapshot::new`] is the same construction the stream server reuses.
pub(crate) fn board_snapshot(run_id: RunId, goal: &str, seed: u64, board: &Board) -> BoardSnapshot {
    BoardSnapshot::new(run_id, goal, seed, board)
}

/// Create `<run-dir>/run.lock` and hold an **exclusive OS advisory
/// (`flock`)** lock on it for the run's lifetime (ADR 0027): the returned
/// [`File`] must be kept alive by the caller, and the kernel releases the
/// lock on *any* process death, including `SIGKILL`. The file carries no
/// data and touches no event schema. `flock(2)`'s per-open-file-description
/// semantics are load-bearing for the stream server's liveness classifier
/// (a separate `open()` conflicts, ADR 0030).
pub(crate) fn acquire_run_lock(dir: &Path) -> std::io::Result<File> {
    let file = File::create(dir.join("run.lock"))?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
        .map_err(std::io::Error::from)?;
    Ok(file)
}

/// Write the three finalized snapshots — `board.json`, `knowledge.jsonl`,
/// `report.md` — on every termination path, clean or capped (ADR 0006/0022).
pub(crate) fn write_final_snapshots(
    dir: &Path,
    board_snapshot: &BoardSnapshot,
    entries: &[KnowledgeEntry],
    report: &str,
) -> std::io::Result<()> {
    let board_pretty = serde_json::to_string_pretty(board_snapshot)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    fs::write(dir.join("board.json"), board_pretty + "\n")?;

    let mut knowledge = String::new();
    for entry in entries {
        let line = serde_json::to_string(entry)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        knowledge.push_str(&line);
        knowledge.push('\n');
    }
    fs::write(dir.join("knowledge.jsonl"), knowledge)?;

    fs::write(dir.join("report.md"), report)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{EventKind, EventSource, RunCaps};
    use crate::ids::{EventId, TaskId, TeamId};
    use jiff::Timestamp;
    use openteam_wire::AgentId;

    #[test]
    fn events_writer_streams_one_json_line_per_event() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = EventsWriter::create(dir.path()).unwrap();
        let event = Event::new(
            EventId::new(0),
            Timestamp::UNIX_EPOCH,
            EventSource::System,
            EventKind::RunStarted {
                run_id: uuid::Uuid::nil(),
                seed: 42,
                goal: "g".into(),
                agents: 1,
                meta_agents: 0,
                parallel: 1,
                scenario: None,
                caps: RunCaps::default(),
            },
        );
        writer.append(&event).unwrap();
        let content = std::fs::read_to_string(dir.path().join("events.jsonl")).unwrap();
        assert_eq!(content.lines().count(), 1);
        let back: Event = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(back.id, EventId::new(0));
    }

    /// Each key must appear in `json`, and in the given order.
    fn assert_key_order(json: &str, keys: &[&str]) {
        let positions: Vec<usize> = keys
            .iter()
            .map(|key| {
                json.find(&format!("\"{key}\""))
                    .unwrap_or_else(|| panic!("key {key:?} missing from {json}"))
            })
            .collect();
        for (pair, keys) in positions.windows(2).zip(keys.windows(2)) {
            assert!(
                pair[0] < pair[1],
                "key {:?} must precede {:?} in {json}",
                keys[0],
                keys[1],
            );
        }
    }

    #[test]
    fn board_json_keys_emit_in_the_transcripts_field_order() {
        let mut board = Board::new();
        let team = TeamId::parse("t1").unwrap();
        board
            .form_team(team.clone(), vec![AgentId::team(1)])
            .unwrap();
        board
            .create_task(
                TaskId::new(1),
                "Draft the setup section",
                "Install + build/test steps for a new contributor.",
                AgentId::orchestrator(),
                EventId::new(2),
                Some(team),
            )
            .unwrap();

        let snapshot = board_snapshot(uuid::Uuid::nil(), "g", 42, &board);
        let json = serde_json::to_string_pretty(&snapshot).unwrap();

        // The pinned orders of the dry-run transcript's §8 `board.json`
        // sample (ADR 0022) — insertion order, not alphabetical.
        assert_key_order(&json, &["run_id", "goal", "seed", "tasks", "teams"]);
        let tasks_section = &json[json.find("\"tasks\"").unwrap()..];
        assert_key_order(
            tasks_section,
            &[
                "id",
                "title",
                "description",
                "created_by",
                "origin_event",
                "team",
                "state",
            ],
        );
        let teams_section = &json[json.find("\"teams\"").unwrap()..];
        assert_key_order(teams_section, &["id", "members", "dissolved"]);
    }

    /// The public owned [`BoardSnapshot`] serializes byte-identically to what
    /// `write_final_snapshots` persists as `board.json` (ADR 0030's second
    /// granted core change): the stream server reuses this exact construction.
    #[test]
    fn public_board_snapshot_serializes_byte_equal_to_board_json() {
        let mut board = Board::new();
        board
            .form_team(TeamId::parse("t1").unwrap(), vec![AgentId::team(1)])
            .unwrap();
        board
            .create_task(
                TaskId::new(1),
                "Draft the setup section",
                "Install + build/test steps.",
                AgentId::orchestrator(),
                EventId::new(2),
                Some(TeamId::parse("t1").unwrap()),
            )
            .unwrap();

        let snapshot = BoardSnapshot::new(uuid::Uuid::nil(), "g", 42, &board);
        let dir = tempfile::tempdir().unwrap();
        write_final_snapshots(dir.path(), &snapshot, &[], "report").unwrap();
        let persisted = std::fs::read_to_string(dir.path().join("board.json")).unwrap();

        // `write_final_snapshots` writes `to_string_pretty + "\n"`; the public
        // constructor must reproduce it exactly.
        assert_eq!(
            persisted,
            serde_json::to_string_pretty(&snapshot).unwrap() + "\n"
        );
        // And the free-function `board_snapshot` is the same construction.
        assert_eq!(snapshot, board_snapshot(uuid::Uuid::nil(), "g", 42, &board));
    }

    /// `run.lock`'s exclusive `flock` uses per-open-file-description
    /// semantics (ADR 0027/0030): a second `open()` of the same file — even
    /// in the same process — conflicts, which is exactly what the stream
    /// server's liveness classifier relies on.
    #[test]
    fn run_lock_conflicts_with_a_second_open_in_the_same_process() {
        let dir = tempfile::tempdir().unwrap();
        let held = acquire_run_lock(dir.path()).unwrap();

        // A separate open + non-blocking exclusive flock must fail while held.
        let contender = File::open(dir.path().join("run.lock")).unwrap();
        let conflict = rustix::fs::flock(
            &contender,
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        );
        assert!(
            conflict.is_err(),
            "held run.lock must block a second locker"
        );

        // Dropping the holder releases the lock; the contender can take it.
        drop(held);
        rustix::fs::flock(
            &contender,
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )
        .expect("released run.lock must be re-lockable");
    }
}
