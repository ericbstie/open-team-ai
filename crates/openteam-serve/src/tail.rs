//! Poll-based `events.jsonl` tailing under the **complete-line rule**
//! (ADR 0027).
//!
//! `notify`/inotify is rejected-for-now: polling is portable and trivially
//! deterministic to test. The hard rule: only **complete newline-terminated
//! lines** are emitted — `BufWriter`'s 8 KiB buffer can tear a >8 KiB event
//! line across `write(2)` calls, so a torn final line is left unconsumed and
//! re-read on the next poll, never parsed or emitted.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Split freshly-read bytes into complete newline-terminated lines — each
/// **without** its trailing `\n` — plus the count of bytes consumed. A partial
/// final line (no trailing `\n`) is left unconsumed: `consumed` stops at the
/// byte after the last `\n`, so the partial tail is re-read next poll.
pub(crate) fn split_complete_lines(buf: &[u8]) -> (Vec<&[u8]>, usize) {
    let Some(last_newline) = buf.iter().rposition(|&b| b == b'\n') else {
        // No complete line yet — leave everything for the next poll.
        return (Vec::new(), 0);
    };
    // Everything up to (not including) the last `\n` is complete lines; the
    // slice after it — if any — is the partial tail left for the next poll.
    // Splitting `buf[..last_newline]` yields exactly those lines, each without
    // a trailing `\n` (an empty slice splits to one empty line).
    let lines = buf[..last_newline].split(|&b| b == b'\n').collect();
    (lines, last_newline + 1)
}

/// A stateful poll tailer over one `events.jsonl`. Each [`poll`](Self::poll)
/// re-reads from the last consumed offset and returns the new complete lines;
/// a torn final line stays unconsumed until it completes.
pub(crate) struct Tailer {
    path: PathBuf,
    /// Bytes consumed so far — always at a line boundary (complete-line rule).
    offset: u64,
}

impl Tailer {
    pub(crate) fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            offset: 0,
        }
    }

    /// The byte offset consumed through so far (a line boundary).
    pub(crate) fn offset(&self) -> u64 {
        self.offset
    }

    /// Read and return new complete lines (bytes, no trailing `\n`) since the
    /// last poll, advancing the offset past them. A missing file yields no
    /// lines (a run dir may not have started writing yet).
    pub(crate) fn poll(&mut self) -> std::io::Result<Vec<Vec<u8>>> {
        let mut file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        file.seek(SeekFrom::Start(self.offset))?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        let (lines, consumed) = split_complete_lines(&buf);
        let owned = lines.into_iter().map(<[u8]>::to_vec).collect();
        self.offset += consumed as u64;
        Ok(owned)
    }
}

/// Read every complete line of a run's `events.jsonl` in one shot (offset 0) —
/// the one-off read the classifier and header/list readers use. A partial
/// final line is excluded (complete-line rule).
pub(crate) fn read_complete_lines(path: &Path) -> std::io::Result<Vec<Vec<u8>>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let (lines, _) = split_complete_lines(&bytes);
    Ok(lines.into_iter().map(<[u8]>::to_vec).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_utf8(lines: &[Vec<u8>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| String::from_utf8(line.clone()).unwrap())
            .collect()
    }

    #[test]
    fn split_yields_complete_lines_and_holds_a_partial_tail() {
        let (lines, consumed) = split_complete_lines(b"a\nbb\nccc");
        assert_eq!(lines, vec![b"a".as_slice(), b"bb".as_slice()]);
        assert_eq!(consumed, 5, "consumed through the second '\\n', not 'ccc'");

        // No newline yet — nothing complete.
        let (lines, consumed) = split_complete_lines(b"partial");
        assert!(lines.is_empty());
        assert_eq!(consumed, 0);

        // A lone newline is one empty line.
        let (lines, consumed) = split_complete_lines(b"\n");
        assert_eq!(lines, vec![b"".as_slice()]);
        assert_eq!(consumed, 1);
    }

    #[test]
    fn tailer_emits_a_torn_line_only_once_it_completes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(&path, b"{\"id\":0}\n{\"id\":1}\n").unwrap();
        let mut tailer = Tailer::new(&path);

        // First poll: both complete lines.
        assert_eq!(
            lines_utf8(&tailer.poll().unwrap()),
            vec!["{\"id\":0}".to_string(), "{\"id\":1}".to_string()]
        );

        // Append a torn line (no trailing newline). Bounded in poll cycles,
        // never wall-clock: several polls must NOT emit it (ADR 0030).
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, b"{\"id\":2"))
            .unwrap();
        for _ in 0..3 {
            assert!(
                tailer.poll().unwrap().is_empty(),
                "a torn final line is never emitted"
            );
        }

        // Complete the line; now it arrives, exactly once.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, b",\"x\":1}\n"))
            .unwrap();
        assert_eq!(
            lines_utf8(&tailer.poll().unwrap()),
            vec!["{\"id\":2,\"x\":1}".to_string()]
        );
        assert!(tailer.poll().unwrap().is_empty(), "no re-emission");
    }

    #[test]
    fn tailer_on_a_missing_file_yields_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut tailer = Tailer::new(dir.path().join("events.jsonl"));
        assert!(tailer.poll().unwrap().is_empty());
        assert_eq!(tailer.offset(), 0);
    }

    #[test]
    fn read_complete_lines_excludes_a_partial_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(&path, b"one\ntwo\npartial").unwrap();
        assert_eq!(
            lines_utf8(&read_complete_lines(&path).unwrap()),
            vec!["one".to_string(), "two".to_string()]
        );
    }
}
