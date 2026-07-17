//! `openteam tui` — a very simplified, Claude-Code-style full-screen terminal
//! UI over the harness (a companion to `openteam run`).
//!
//! A synchronous `ratatui` + crossterm loop runs on the main thread while
//! harness sessions run on the owned tokio runtime. The user types a goal;
//! Enter spawns a session against a fresh in-process mock; a background tailer
//! streams `events.jsonl` into the transcript as one-line activity; the run's
//! report is appended on completion. The terminal is always restored via a
//! `Drop` guard, on every exit path.

use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use openteam_core::{
    Address, Event, EventKind, ReqwestLlmClient, RunConfig, RunFinishReason, SystemClock,
};
use openteam_mock::{AppState, serve};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::cursor::Show;
use ratatui::crossterm::event::{
    self, Event as CtEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Clear, Paragraph};
use tokio::runtime::{Handle, Runtime};
use url::Url;

use crate::cli::TuiArgs;

/// The braille spinner cycled once per UI tick while a run is in flight.
const SPINNER: &str = "⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏";
/// Longest string rendered in an activity line before an ellipsis.
const TRUNCATE_AT: usize = 60;
/// The loop's input-poll timeout — also the spinner / elapsed-timer cadence.
const TICK: Duration = Duration::from_millis(100);

/// A message from a harness session (or its tailer) to the UI thread.
enum Update {
    /// One human-readable activity line derived from an event.
    Activity(String),
    /// The finished run's report and exit code.
    Report { text: String, exit_code: u8 },
    /// The session failed before producing a report.
    Failed(String),
}

/// Where the harness is in its lifecycle, as reflected in the header.
enum RunState {
    Idle,
    Running(Instant),
    Done,
}

/// How a transcript entry is styled.
#[derive(Clone, Copy)]
enum EntryKind {
    User,
    Activity,
    Report,
}

/// One logical (pre-wrap) transcript line.
struct Entry {
    kind: EntryKind,
    text: String,
}

/// A key-handler outcome: keep looping or quit.
enum Flow {
    Continue,
    Quit,
}

/// The run knobs carried from `TuiArgs` to each session — a `Copy` bundle so it
/// rides into the spawned task as one value.
#[derive(Clone, Copy)]
struct RunParams {
    agents: usize,
    meta_agents: usize,
    seed: Option<u64>,
}

/// The whole UI state — owned by the main thread, mutated in place.
struct App {
    input: String,
    /// Cursor position as a char index into `input`.
    cursor: usize,
    transcript: Vec<Entry>,
    state: RunState,
    /// Top visual line of the transcript viewport.
    scroll: usize,
    /// Stick to the bottom as new content arrives (until the user scrolls up).
    follow: bool,
    spinner: usize,
    /// Transcript viewport height from the last frame (for page scrolling).
    viewport_lines: usize,
    params: RunParams,
}

impl App {
    fn new(params: RunParams) -> Self {
        Self {
            input: String::new(),
            cursor: 0,
            transcript: vec![Entry {
                kind: EntryKind::Activity,
                text: "welcome to openteam — type a goal and press Enter".to_string(),
            }],
            state: RunState::Idle,
            scroll: 0,
            follow: true,
            spinner: 0,
            viewport_lines: 1,
            params,
        }
    }

    fn is_running(&self) -> bool {
        matches!(self.state, RunState::Running(_))
    }

    fn push(&mut self, kind: EntryKind, text: String) {
        self.transcript.push(Entry { kind, text });
    }

    fn apply_update(&mut self, update: Update) {
        match update {
            Update::Activity(text) => self.push(EntryKind::Activity, text),
            Update::Report { text, exit_code } => {
                self.push(
                    EntryKind::Activity,
                    format!("── report (exit {exit_code}) ──"),
                );
                for line in text.lines() {
                    self.push(EntryKind::Report, line.to_string());
                }
                self.state = RunState::Done;
            }
            Update::Failed(message) => {
                self.push(EntryKind::Activity, format!("✗ {message}"));
                self.state = RunState::Done;
            }
        }
    }

    fn tick(&mut self) {
        self.spinner = self.spinner.wrapping_add(1);
    }

    fn clear_transcript(&mut self) {
        self.transcript.clear();
        self.scroll = 0;
        self.follow = true;
    }

    fn scroll_up(&mut self, n: usize) {
        self.follow = false;
        self.scroll = self.scroll.saturating_sub(n);
    }

    fn scroll_down(&mut self, n: usize) {
        // Grow past the clamp; the renderer re-enables `follow` at the bottom.
        self.scroll = self.scroll.saturating_add(n);
    }

    /// Byte offset of char index `idx`, or the string length past the end.
    fn byte_at(&self, idx: usize) -> usize {
        self.input
            .char_indices()
            .nth(idx)
            .map_or(self.input.len(), |(byte, _)| byte)
    }

    fn insert(&mut self, c: char) {
        let byte = self.byte_at(self.cursor);
        self.input.insert(byte, c);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self.byte_at(self.cursor - 1);
        let end = self.byte_at(self.cursor);
        self.input.replace_range(start..end, "");
        self.cursor -= 1;
    }

    fn delete(&mut self) {
        if self.cursor >= self.input.chars().count() {
            return;
        }
        let start = self.byte_at(self.cursor);
        let end = self.byte_at(self.cursor + 1);
        self.input.replace_range(start..end, "");
    }

    fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn move_right(&mut self) {
        if self.cursor < self.input.chars().count() {
            self.cursor += 1;
        }
    }
}

/// Restores the terminal on drop — every exit path, including `?` and panics.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut out = io::stdout();
        let _ = execute!(out, LeaveAlternateScreen, Show);
    }
}

/// Entry point for `openteam tui`: takes ownership of the runtime (harness
/// sessions are spawned onto it) and drives the synchronous UI loop.
pub fn run(runtime: Runtime, args: TuiArgs) -> ExitCode {
    match run_ui(&runtime, args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // The guard has already restored the terminal by this point.
            eprintln!("openteam tui: {error:#}");
            ExitCode::from(1)
        }
    }
}

fn run_ui(runtime: &Runtime, args: TuiArgs) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    let mut stdout = io::stdout();
    // Note: no mouse capture — leaving it off keeps the terminal's native
    // text selection (so the report stays copy-pasteable); scrolling is on the
    // keyboard (arrows / PgUp / PgDn).
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal: Terminal<CrosstermBackend<Stdout>> = Terminal::new(backend)?;

    let (tx, rx) = mpsc::channel::<Update>();
    let handle = runtime.handle().clone();
    let mut app = App::new(RunParams {
        agents: args.agents,
        meta_agents: args.meta_agents,
        seed: args.seed,
    });

    loop {
        terminal.draw(|frame| ui(frame, &mut app))?;

        if event::poll(TICK)?
            && let CtEvent::Key(key) = event::read()?
            && let Flow::Quit = handle_key(&mut app, key, &handle, &tx)
        {
            break;
        }

        while let Ok(update) = rx.try_recv() {
            app.apply_update(update);
        }
        app.tick();
    }
    Ok(())
}

fn handle_key(app: &mut App, key: KeyEvent, handle: &Handle, tx: &Sender<Update>) -> Flow {
    if key.kind == KeyEventKind::Release {
        return Flow::Continue;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Esc => return Flow::Quit,
        KeyCode::Char('c') if ctrl => return Flow::Quit,
        KeyCode::Char('l') if ctrl => app.clear_transcript(),
        KeyCode::Enter => submit(app, handle, tx),
        KeyCode::Up => app.scroll_up(1),
        KeyCode::Down => app.scroll_down(1),
        KeyCode::PageUp => app.scroll_up(app.viewport_lines),
        KeyCode::PageDown => app.scroll_down(app.viewport_lines),
        KeyCode::Backspace => app.backspace(),
        KeyCode::Delete => app.delete(),
        KeyCode::Left => app.move_left(),
        KeyCode::Right => app.move_right(),
        KeyCode::Home => app.cursor = 0,
        KeyCode::End => app.cursor = app.input.chars().count(),
        KeyCode::Char(c) if !ctrl && !alt => app.insert(c),
        _ => {}
    }
    Flow::Continue
}

/// Spawn a harness session for the current input, if idle and non-empty.
fn submit(app: &mut App, handle: &Handle, tx: &Sender<Update>) {
    if app.is_running() {
        return;
    }
    let goal = app.input.trim().to_string();
    if goal.is_empty() {
        return;
    }
    app.push(EntryKind::User, goal.clone());
    app.input.clear();
    app.cursor = 0;
    app.state = RunState::Running(Instant::now());
    app.follow = true;

    let tx = tx.clone();
    let params = app.params;
    handle.spawn(run_session(goal, params, tx));
}

/// A unique, freshly-created temp directory for one run's artifacts.
fn unique_out_dir() -> io::Result<PathBuf> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!("openteam-tui-{nanos}-{n}"));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Run one harness session end to end, forwarding activity and the report
/// through `tx`. Every failure path reports via [`Update::Failed`] and returns.
async fn run_session(goal: String, params: RunParams, tx: Sender<Update>) {
    let out_dir = match unique_out_dir() {
        Ok(dir) => dir,
        Err(error) => {
            let _ = tx.send(Update::Failed(format!(
                "could not create temp dir: {error}"
            )));
            return;
        }
    };

    let (addr, mock) = match serve(AppState::builtin(), 0).await {
        Ok(bound) => bound,
        Err(error) => {
            let _ = tx.send(Update::Failed(format!("mock failed to start: {error}")));
            let _ = std::fs::remove_dir_all(&out_dir);
            return;
        }
    };

    let url = match Url::parse(&format!("http://{addr}")) {
        Ok(url) => url,
        Err(error) => {
            let _ = tx.send(Update::Failed(format!(
                "mock address is not a URL: {error}"
            )));
            mock.shutdown().await;
            let _ = std::fs::remove_dir_all(&out_dir);
            return;
        }
    };

    let mut config = RunConfig::new(goal);
    config.agents = params.agents;
    config.meta_agents = params.meta_agents;
    config.parallel = params.agents;
    config.out_dir = Some(out_dir.clone());
    if let Some(seed) = params.seed {
        config.seed = seed;
    }

    // Tail `events.jsonl` for the duration of the run.
    let done = Arc::new(AtomicBool::new(false));
    let tailer = tokio::spawn(tail_events(
        out_dir.join("events.jsonl"),
        tx.clone(),
        done.clone(),
    ));

    let transport = Arc::new(ReqwestLlmClient::new(url, None));
    let outcome = openteam_core::run(config, transport, Arc::new(SystemClock)).await;

    // Signal the tailer; it does one final poll before exiting so trailing
    // lines flush, then we await it.
    done.store(true, Ordering::SeqCst);
    let _ = tailer.await;
    mock.shutdown().await;

    match outcome {
        Ok(outcome) => {
            let _ = tx.send(Update::Report {
                text: outcome.report,
                exit_code: outcome.exit_code,
            });
        }
        Err(error) => {
            let _ = tx.send(Update::Failed(format!("harness error: {error}")));
        }
    }
    let _ = std::fs::remove_dir_all(&out_dir);
}

/// Poll `events.jsonl` until the run signals `done`, forwarding each newly
/// complete line's [`describe`] rendering. Parse errors (partial lines) are
/// ignored silently.
async fn tail_events(path: PathBuf, tx: Sender<Update>, done: Arc<AtomicBool>) {
    let mut emitted = 0usize;
    loop {
        // Read the flag first so the flush that follows a set flag is final.
        let finished = done.load(Ordering::SeqCst);
        emitted = flush_new_lines(&path, emitted, &tx);
        if finished {
            break;
        }
        tokio::time::sleep(Duration::from_millis(120)).await;
    }
}

/// Emit activity for every complete line beyond `emitted`; returns the new
/// complete-line count. The trailing split element is always treated as an
/// incomplete (or empty) line and left for the next poll.
fn flush_new_lines(path: &Path, emitted: usize, tx: &Sender<Update>) -> usize {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<&str> = content.split('\n').collect();
    lines.pop();
    let total = lines.len();
    for line in lines.into_iter().skip(emitted) {
        if line.is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<Event>(line)
            && let Some(text) = describe(&event)
        {
            let _ = tx.send(Update::Activity(text));
        }
    }
    total.max(emitted)
}

/// Map an interesting event to a short activity line; noisy kinds return
/// `None` so the feed stays readable. Pure — unit-tested below.
fn describe(event: &Event) -> Option<String> {
    let source = &event.source;
    match &event.kind {
        EventKind::RunStarted { goal, agents, .. } => Some(format!(
            "run started · {agents} agents · goal: {}",
            truncate(goal, TRUNCATE_AT)
        )),
        EventKind::TaskCreated { task, title, .. } => {
            Some(format!("＋ task {task}: {}", truncate(title, TRUNCATE_AT)))
        }
        EventKind::TaskClaimed { task, .. } => Some(format!("▶ {source} claimed {task}")),
        EventKind::TaskCompleted { task, .. } => Some(format!("✓ {task} done")),
        EventKind::TaskReleased { task, .. } => Some(format!("↩ {source} released {task}")),
        EventKind::TaskCancelled { task, reason } => Some(format!(
            "✗ {task} cancelled: {}",
            truncate(reason, TRUNCATE_AT)
        )),
        EventKind::MessageSent { address, .. } => {
            Some(format!("✉ {source} → {}", render_address(address)))
        }
        EventKind::KnowledgeWritten { text, .. } => {
            Some(format!("❋ note: {}", truncate(text, TRUNCATE_AT)))
        }
        EventKind::TeamFormed { team, members } => {
            Some(format!("⧉ team {team} formed · {} members", members.len()))
        }
        EventKind::DirectiveIssued {
            directive, kind, ..
        } => Some(format!("⚙ {source} directive {directive}: {kind}")),
        EventKind::CapHit {
            cap,
            limit,
            observed,
        } => Some(format!("⚠ cap {} hit ({observed}/{limit})", cap.as_str())),
        EventKind::RunFinished { reason, exit_code } => Some(format!(
            "run finished ({}) exit {exit_code}",
            render_reason(reason)
        )),
        _ => None,
    }
}

fn render_address(address: &Address) -> String {
    match address {
        Address::Direct { to } => to.to_string(),
        Address::Team { team } => format!("team {team}"),
        Address::Broadcast => "broadcast".to_string(),
    }
}

fn render_reason(reason: &RunFinishReason) -> String {
    match reason {
        RunFinishReason::CleanFinish => "clean".to_string(),
        RunFinishReason::CapHit(cap) => format!("cap {}", cap.as_str()),
        RunFinishReason::HarnessError => "harness error".to_string(),
    }
}

/// Truncate to `max` chars, appending an ellipsis only when it actually cut.
fn truncate(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_string();
    }
    let mut out: String = chars.into_iter().take(max).collect();
    out.push('…');
    out
}

// ---- rendering ----------------------------------------------------------

fn ui(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    // Clear so shrinking content (e.g. after Ctrl-L) never ghosts.
    frame.render_widget(Clear, area);

    let [header, body, input, hint] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .areas(area);

    render_header(frame, header, app);
    render_transcript(frame, body, app);
    render_input(frame, input, app);
    render_hint(frame, hint, app);
}

fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let (status_text, status_style) = status(app);
    let title = " openteam · simplified TUI ";
    let width = area.width as usize;
    let used = title.chars().count() + status_text.chars().count();
    let pad = width.saturating_sub(used);
    let line = Line::from(vec![
        Span::styled(title, Style::new().add_modifier(Modifier::BOLD)),
        Span::raw(" ".repeat(pad)),
        Span::styled(status_text, status_style),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn status(app: &App) -> (String, Style) {
    match &app.state {
        RunState::Idle => ("Idle".to_string(), Style::new().fg(Color::DarkGray)),
        RunState::Running(since) => (
            format!(
                "Running {} {:.1}s ",
                spinner_char(app.spinner),
                since.elapsed().as_secs_f64()
            ),
            Style::new().fg(Color::Yellow),
        ),
        RunState::Done => ("Done ".to_string(), Style::new().fg(Color::Green)),
    }
}

fn spinner_char(i: usize) -> char {
    let count = SPINNER.chars().count().max(1);
    SPINNER.chars().nth(i % count).unwrap_or('·')
}

fn render_transcript(frame: &mut Frame, area: Rect, app: &mut App) {
    let block = Block::bordered().title(" transcript ");
    let inner = block.inner(area);
    let width = inner.width as usize;
    let height = (inner.height as usize).max(1);

    let lines = build_lines(app, width);
    let total = lines.len();
    let max_scroll = total.saturating_sub(height);
    let offset = if app.follow {
        max_scroll
    } else {
        app.scroll.min(max_scroll)
    };
    // Reaching the bottom re-arms auto-follow.
    if offset >= max_scroll {
        app.follow = true;
    }
    app.scroll = offset;
    app.viewport_lines = height;

    let paragraph = Paragraph::new(Text::from(lines))
        .block(block)
        .scroll((u16::try_from(offset).unwrap_or(u16::MAX), 0));
    frame.render_widget(paragraph, area);
}

/// Build the wrapped, styled visual lines for the transcript at `width`.
fn build_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for entry in &app.transcript {
        let style = match entry.kind {
            EntryKind::User => Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            EntryKind::Activity => Style::new().fg(Color::DarkGray),
            EntryKind::Report => {
                if entry.text.starts_with("# ") || entry.text.starts_with("## ") {
                    Style::new().add_modifier(Modifier::BOLD)
                } else {
                    Style::new()
                }
            }
        };
        let display = match entry.kind {
            EntryKind::User => format!("› {}", entry.text),
            _ => entry.text.clone(),
        };
        for visual in wrap(&display, width) {
            out.push(Line::styled(visual, style));
        }
    }
    out
}

/// Word-wrap `text` to `width` columns (char-counted), hard-breaking any word
/// longer than the width. Empty input yields a single empty line.
fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    for raw in text.split('\n') {
        let mut current = String::new();
        let mut current_w = 0usize;
        for word in raw.split(' ') {
            let word_w = word.chars().count();
            if word_w > width {
                if !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                }
                let mut chunk = String::new();
                let mut chunk_w = 0usize;
                for ch in word.chars() {
                    if chunk_w == width {
                        lines.push(std::mem::take(&mut chunk));
                        chunk_w = 0;
                    }
                    chunk.push(ch);
                    chunk_w += 1;
                }
                current = chunk;
                current_w = chunk_w;
                continue;
            }
            let sep = usize::from(!current.is_empty());
            if current_w + sep + word_w > width {
                lines.push(std::mem::take(&mut current));
                current.push_str(word);
                current_w = word_w;
            } else {
                if sep == 1 {
                    current.push(' ');
                }
                current.push_str(word);
                current_w += sep + word_w;
            }
        }
        lines.push(current);
    }
    lines
}

fn render_input(frame: &mut Frame, area: Rect, app: &App) {
    let inner_w = area.width.saturating_sub(2) as usize;
    // Horizontal scroll keeps the cursor in view for long inputs.
    let scroll_x = app.cursor.saturating_sub(inner_w.saturating_sub(1));
    let visible: String = app.input.chars().skip(scroll_x).take(inner_w).collect();

    let paragraph = Paragraph::new(visible).block(Block::bordered().title(" message "));
    frame.render_widget(paragraph, area);

    if inner_w > 0 {
        let col = (app.cursor - scroll_x) as u16;
        frame.set_cursor_position(Position {
            x: area.x + 1 + col,
            y: area.y + 1,
        });
    }
}

fn render_hint(frame: &mut Frame, area: Rect, app: &App) {
    let hint = if app.is_running() {
        "running…  ·  Esc/Ctrl-C quit  ·  Ctrl-L clear  ·  ↑/↓ PgUp/PgDn scroll"
    } else {
        "Enter send  ·  Esc/Ctrl-C quit  ·  Ctrl-L clear  ·  ↑/↓ PgUp/PgDn scroll"
    };
    frame.render_widget(
        Paragraph::new(Line::styled(hint, Style::new().fg(Color::DarkGray))),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Event {
        serde_json::from_str(line).expect("event line should deserialize")
    }

    #[test]
    fn describe_maps_interesting_kinds() {
        let started = parse(
            r#"{"id":0,"at":"2026-07-17T00:00:00Z","source":"system","kind":"run_started","data":{"run_id":"0192f1a0-7e3c-7abc-9def-000000000000","seed":42,"goal":"Write a short onboarding guide","agents":3,"meta_agents":1,"parallel":3,"scenario":null,"caps":{}}}"#,
        );
        assert_eq!(
            describe(&started).as_deref(),
            Some("run started · 3 agents · goal: Write a short onboarding guide")
        );

        let created = parse(
            r#"{"id":2,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"task_created","data":{"task":1,"title":"Draft the setup section","description":"x","team":"t1"}}"#,
        );
        assert_eq!(
            describe(&created).as_deref(),
            Some("＋ task 1: Draft the setup section")
        );

        let claimed = parse(
            r#"{"id":5,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"task_claimed","data":{"task":1,"team":"t1"}}"#,
        );
        assert_eq!(describe(&claimed).as_deref(), Some("▶ agent-1 claimed 1"));

        let message = parse(
            r#"{"id":10,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"message_sent","data":{"message":1,"address":{"Direct":{"to":"agent-1"}},"body":"hi","knowledge_ref":1}}"#,
        );
        assert_eq!(
            describe(&message).as_deref(),
            Some("✉ orchestrator → agent-1")
        );

        let finished = parse(
            r#"{"id":33,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"run_finished","data":{"reason":"CleanFinish","exit_code":0}}"#,
        );
        assert_eq!(
            describe(&finished).as_deref(),
            Some("run finished (clean) exit 0")
        );
    }

    #[test]
    fn describe_skips_noise() {
        let turn = parse(
            r#"{"id":4,"at":"2026-07-17T00:00:00Z","source":"orchestrator","kind":"turn_completed","data":{"first_call_seq":0,"last_call_seq":1,"tool_iters":1,"outcome":"Yielded","malformed":false,"usage":{"prompt":1,"completion":1,"total":2},"on_task":null}}"#,
        );
        assert!(describe(&turn).is_none());

        let delivered = parse(
            r#"{"id":12,"at":"2026-07-17T00:00:00Z","source":"agent-1","kind":"messages_delivered","data":{"delivered":[1]}}"#,
        );
        assert!(describe(&delivered).is_none());
    }

    #[test]
    fn truncate_appends_ellipsis_only_when_cutting() {
        assert_eq!(truncate("short", 60), "short");
        let long = "x".repeat(80);
        let cut = truncate(&long, 60);
        assert_eq!(cut.chars().count(), 61);
        assert!(cut.ends_with('…'));
    }

    #[test]
    fn wrap_breaks_at_width_and_hard_breaks_long_words() {
        assert_eq!(wrap("a b c", 3), vec!["a b".to_string(), "c".to_string()]);
        assert_eq!(
            wrap("abcdef", 3),
            vec!["abc".to_string(), "def".to_string()]
        );
        assert_eq!(wrap("", 5), vec![String::new()]);
    }
}
