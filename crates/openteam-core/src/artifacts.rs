//! Run artifacts: the streamed `events.jsonl` spine plus the finalized
//! snapshots written on every termination path (ADR 0022).

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::board::Board;
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

/// The pretty-printed final `board.json` snapshot (ADR 0022).
pub(crate) fn board_snapshot(
    run_id: RunId,
    goal: &str,
    seed: u64,
    board: &Board,
) -> serde_json::Value {
    let mut tasks: Vec<&crate::board::Task> = board.tasks().collect();
    tasks.sort_by_key(|t| t.id);
    serde_json::json!({
        "run_id": run_id,
        "goal": goal,
        "seed": seed,
        "tasks": tasks,
        "teams": board.teams(),
    })
}

/// Write the three finalized snapshots — `board.json`, `knowledge.jsonl`,
/// `report.md` — on every termination path, clean or capped (ADR 0006/0022).
pub(crate) fn write_final_snapshots(
    dir: &Path,
    board_snapshot: &serde_json::Value,
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
    use crate::ids::EventId;
    use jiff::Timestamp;

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
}
