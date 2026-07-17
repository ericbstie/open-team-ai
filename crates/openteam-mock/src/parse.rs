//! The read side of the prompt-legibility contract (ADR 0021, grammars pinned
//! in ADR 0016, placeholders and open details in implementation-pins §2–3).
//!
//! Identity is NEVER read from content — it comes solely from the `user` field
//! plus the `X-OpenTeam-*` headers (ADR 0008). What this module reads is
//! **world state**: the `##`-headed sections of the LAST `role:"user"` message
//! (the board rendered in the request *is* the arc's stateless memory), the
//! turn-local `assistant`/`tool` messages after it (ADR 0015's inner loop),
//! and the callable verbs from the request's `tools` array (ADR 0013).
//!
//! Parsing is deliberately defensive: this is a mock, not a validator — absent
//! sections and unparseable lines are tolerated (skipped), never an error.

use std::collections::BTreeMap;

use openteam_wire::{
    ChatCompletionRequest, ChatMessage, ToolChoice, ToolChoiceMode, ToolDef, ToolType,
};

/// Everything the arc can see: the rendered world of the last `user` message,
/// the turn-local messages after it, and the tools index.
#[derive(Debug, Clone, Default)]
pub struct RenderedWorld {
    /// The `## Goal` section text (first line), if present.
    pub goal: Option<String>,
    /// The `## Board digest` section, if present.
    pub board: Option<BoardDigest>,
    /// The `## Claimed task` line, if present and non-placeholder (⟺ Working).
    pub claimed: Option<ClaimedTask>,
    /// The `## Recent activity` window, if the section is present.
    pub recent_activity: Option<RecentActivity>,
    /// Parsed `## Fresh messages` lines, oldest first.
    pub fresh_messages: Vec<FreshMessage>,
    /// Parsed `## Directives` lines (the orchestrator's pending-slot view).
    pub directives: Vec<DirectiveLine>,
    /// The `## Directive outcomes` per-tier usage, if the section is present.
    pub directive_outcomes: Option<DirectiveOutcomes>,
    /// Parsed `## Metrics digest` utilization lines (the meta's agent view).
    pub utilization: Vec<UtilizationLine>,
    /// Turn-local state: the assistant/tool messages after the last user
    /// message (ADR 0015's within-turn inner loop).
    pub turn_local: TurnLocal,
    /// The callable verbs, learned solely from the `tools` array (ADR 0013).
    pub tools: ToolIndex,
}

/// One `## Board digest` section: task lines plus the folded run-health line.
#[derive(Debug, Clone, Default)]
pub struct BoardDigest {
    pub tasks: Vec<TaskLine>,
    /// `(working, idle, asleep)` team-agent counts from the `run-health:` line.
    pub run_health_agents: Option<(u32, u32, u32)>,
}

impl BoardDigest {
    /// Total visible task count `n` (terminal states included — tasks are
    /// never destroyed, so `n` never decreases; that monotonicity is what
    /// bounds decomposition at ≤2 batches, ADR 0021).
    pub fn count(&self) -> usize {
        self.tasks.len()
    }

    pub fn non_terminal(&self) -> impl Iterator<Item = &TaskLine> {
        self.tasks
            .iter()
            .filter(|task| matches!(task.state, TaskState::Open | TaskState::Claimed { .. }))
    }

    pub fn all_terminal(&self) -> bool {
        self.non_terminal().count() == 0
    }

    /// The lowest-id Open task (the ADR 0021 claim tie-break ≈ FIFO).
    pub fn lowest_open(&self) -> Option<&TaskLine> {
        self.tasks
            .iter()
            .filter(|task| matches!(task.state, TaskState::Open))
            .min_by_key(|task| task.id)
    }

    pub fn done_titles(&self) -> Vec<&str> {
        self.tasks
            .iter()
            .filter(|task| matches!(task.state, TaskState::Done))
            .map(|task| task.title.as_str())
            .collect()
    }

    /// Handles currently claiming a task (steer-message targets).
    pub fn claimants(&self) -> Vec<&str> {
        self.tasks
            .iter()
            .filter_map(|task| match &task.state {
                TaskState::Claimed { by } => Some(by.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The most common non-`-` team tag on the visible board (batch-two tasks
    /// join the team batch one formed).
    pub fn dominant_team(&self) -> Option<&str> {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for task in &self.tasks {
            if let Some(team) = &task.team {
                *counts.entry(team.as_str()).or_default() += 1;
            }
        }
        counts
            .into_iter()
            .max_by_key(|&(_, count)| count)
            .map(|(team, _)| team)
    }
}

/// One `- task <id> [<state>] team:<tag|->  "<title>"` line (ADR 0016).
#[derive(Debug, Clone)]
pub struct TaskLine {
    pub id: u64,
    pub state: TaskState,
    /// `None` renders as the untagged `-`.
    pub team: Option<String>,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskState {
    Open,
    Claimed { by: String },
    Done,
    Cancelled,
}

/// The `## Claimed task` line: `task <id> — "<title>" (team <t>)` (ADR 0016).
#[derive(Debug, Clone)]
pub struct ClaimedTask {
    pub id: u64,
    pub title: String,
    /// `None` for the untagged `(team -)` rendering.
    pub team: Option<String>,
}

/// The `## Recent activity` window: the degradation marker plus the
/// work-action count (ADR 0016/0021, implementation-pins §3).
#[derive(Debug, Clone, Default)]
pub struct RecentActivity {
    /// True when the first content line starts with `(degraded` — the
    /// ADR 0021 completion shortcut, never a block.
    pub degraded: bool,
    /// Lines whose verb ∈ {write_knowledge, post_message, search_knowledge}.
    pub work_actions: usize,
}

/// One `- msg <id> from <sender> (<kind>): "<body>"` line (ADR 0016).
#[derive(Debug, Clone)]
pub struct FreshMessage {
    pub id: u64,
    pub from: String,
    /// `direct`, `team:<t>`, or `broadcast` — the raw parenthesized kind.
    pub kind: String,
    pub body: String,
}

/// One `- directive <id> [<tier>, <state>] <kind>{<args>} from <meta>` line
/// (ADR 0016 — kind + args rendered, not just the id, so the orchestrator can
/// act on it).
#[derive(Debug, Clone)]
pub struct DirectiveLine {
    pub id: u64,
    /// Lowercase tier: `judgment` / `mechanical`.
    pub tier: String,
    /// Lowercase state: `pending`, …
    pub state: String,
    pub kind: String,
    pub args: DirectiveArgs,
    pub from: String,
}

impl DirectiveLine {
    pub fn is_pending(&self) -> bool {
        self.state == "pending"
    }
}

/// The `key:value` args gist inside a directive line's braces
/// (implementation-pins §3: comma-space separated, bare values for
/// handles/slugs/ints, quoted strings, `[a b]` lists).
#[derive(Debug, Clone, Default)]
pub struct DirectiveArgs {
    pub pairs: Vec<(String, String)>,
}

impl DirectiveArgs {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// A `[a b c]` list value split on whitespace.
    pub fn get_list(&self, key: &str) -> Option<Vec<String>> {
        let value = self.get(key)?;
        let inner = value.strip_prefix('[')?.strip_suffix(']')?;
        Some(inner.split_whitespace().map(str::to_owned).collect())
    }
}

/// The meta's per-tier already-issued bound, read statelessly from its
/// `## Directive outcomes` slot (ADR 0020/0021 — ≤1 per tier per run).
#[derive(Debug, Clone, Copy, Default)]
pub struct DirectiveOutcomes {
    pub judgment_used: bool,
    pub mechanical_used: bool,
}

/// One metrics-digest utilization line:
/// `- <agent>: <Idle|Working (task N)|Asleep>, <specialty>` (ADR 0016 — the
/// specialty is rendered so the meta arc can find "an Idle generalist").
#[derive(Debug, Clone)]
pub struct UtilizationLine {
    pub agent: String,
    pub state: AgentStateLine,
    pub specialty: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStateLine {
    Idle,
    Working { task: u64 },
    Asleep,
}

/// The within-turn inner-loop messages after the last `user` message
/// (ADR 0015): what this turn already did.
#[derive(Debug, Clone, Default)]
pub struct TurnLocal {
    /// Verb names of the assistant tool calls already made this turn.
    pub called_verbs: Vec<String>,
    /// Tool-outcome statuses parsed from the `role:"tool"` replies' JSON
    /// `{"status": …}` content (ADR 0017 envelope).
    pub statuses: Vec<ToolStatus>,
    /// Count of `role:"tool"` messages seen after the last user message.
    pub tool_messages: usize,
}

impl TurnLocal {
    /// True when this turn already acted — the arc's next completion must
    /// yield (ADR 0021 amendment: one verb per turn, then yield).
    pub fn acted(&self) -> bool {
        self.tool_messages > 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Ok,
    Rejected,
    Invalid,
    Unknown,
}

/// The callable verbs and their parameter schemas, learned solely from the
/// request's `tools` array (ADR 0013).
#[derive(Debug, Clone, Default)]
pub struct ToolIndex {
    schemas: BTreeMap<String, Option<serde_json::Value>>,
    /// `tool_choice: "none"` suppresses calling entirely.
    calls_suppressed: bool,
}

impl ToolIndex {
    pub fn available(&self, name: &str) -> bool {
        !self.calls_suppressed && self.schemas.contains_key(name)
    }

    pub fn schema(&self, name: &str) -> Option<&serde_json::Value> {
        self.schemas.get(name).and_then(|schema| schema.as_ref())
    }

    /// Shallow structural fit of an args object against the verb's JSON
    /// Schema: required keys present, keys ⊆ properties. This is what makes
    /// "the arc never emits an `invalid` call" structural rather than hoped
    /// (ADR 0021/0025) even against an unfamiliar tools array.
    pub fn args_fit(&self, name: &str, args: &serde_json::Value) -> bool {
        if !self.available(name) {
            return false;
        }
        let Some(object) = args.as_object() else {
            return false;
        };
        let Some(schema) = self.schema(name) else {
            // A tool without `parameters` declares an empty parameter list.
            return object.is_empty();
        };
        let properties = schema.get("properties").and_then(|p| p.as_object());
        if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
            for key in required.iter().filter_map(|k| k.as_str()) {
                if !object.contains_key(key) {
                    return false;
                }
            }
        }
        if let Some(properties) = properties {
            for key in object.keys() {
                if !properties.contains_key(key) {
                    return false;
                }
            }
        }
        true
    }
}

impl RenderedWorld {
    /// Parse everything the arc reads from one request. Never fails: absent
    /// sections stay `None`/empty (this is a mock, not a validator).
    pub fn parse(req: &ChatCompletionRequest) -> Self {
        let mut world = Self {
            tools: parse_tools(req),
            ..Self::default()
        };

        let last_user = req
            .messages
            .iter()
            .rposition(|message| matches!(message, ChatMessage::User { .. }));

        if let Some(index) = last_user {
            if let ChatMessage::User { content, .. } = &req.messages[index] {
                world.parse_sections(&content.rendered_text());
            }
            world.turn_local = parse_turn_local(&req.messages[index + 1..]);
        }
        world
    }

    fn parse_sections(&mut self, text: &str) {
        for (header, lines) in split_sections(text) {
            match header.as_str() {
                "Goal" => {
                    self.goal = lines.iter().find(|line| !line.is_empty()).cloned();
                }
                "Board digest" => self.board = Some(parse_board(&lines)),
                "Claimed task" => self.claimed = parse_claimed(&lines),
                "Recent activity" => self.recent_activity = Some(parse_recent(&lines)),
                "Fresh messages" => self.fresh_messages = parse_fresh(&lines),
                "Directives" => self.directives = parse_directives(&lines),
                "Directive outcomes" => {
                    self.directive_outcomes = Some(parse_outcomes(&lines));
                }
                "Metrics digest" => self.utilization = parse_utilization(&lines),
                _ => {}
            }
        }
    }
}

/// Split a `user` message into its `##`-headed sections (ADR 0016: one
/// `##`-headed markdown block per section, fixed order per policy).
fn split_sections(text: &str) -> Vec<(String, Vec<String>)> {
    let mut sections = Vec::new();
    let mut current: Option<(String, Vec<String>)> = None;
    for line in text.lines() {
        if let Some(header) = line.strip_prefix("## ") {
            if let Some(section) = current.take() {
                sections.push(section);
            }
            current = Some((header.trim().to_owned(), Vec::new()));
        } else if let Some((_, lines)) = current.as_mut() {
            lines.push(line.trim_end().to_owned());
        }
    }
    if let Some(section) = current.take() {
        sections.push(section);
    }
    sections
}

fn is_placeholder(line: &str) -> bool {
    matches!(line, "(empty)" | "(none)" | "(none issued)")
}

fn parse_board(lines: &[String]) -> BoardDigest {
    let mut digest = BoardDigest::default();
    for line in lines {
        if let Some(rest) = line.strip_prefix("- task ") {
            if let Some(task) = parse_task_line(rest) {
                digest.tasks.push(task);
            }
        } else if let Some(rest) = line.strip_prefix("run-health:") {
            digest.run_health_agents = parse_run_health_agents(rest);
        }
    }
    digest
}

/// Parse `<id> [<state>] team:<tag|->  "<title>"`.
fn parse_task_line(rest: &str) -> Option<TaskLine> {
    let (id_str, rest) = rest.split_once(' ')?;
    let id: u64 = id_str.parse().ok()?;
    let rest = rest.trim_start();
    let inner = rest.strip_prefix('[')?;
    let (state_str, rest) = inner.split_once(']')?;
    let state = if state_str == "Open" {
        TaskState::Open
    } else if let Some(by) = state_str.strip_prefix("Claimed by ") {
        TaskState::Claimed { by: by.to_owned() }
    } else if state_str == "Done" {
        TaskState::Done
    } else if state_str == "Cancelled" {
        TaskState::Cancelled
    } else {
        return None;
    };
    let rest = rest.trim_start();
    let tag_rest = rest.strip_prefix("team:")?;
    let (tag, rest) = tag_rest.split_once(char::is_whitespace)?;
    let team = (tag != "-").then(|| tag.to_owned());
    let title = between_quotes(rest)?.to_owned();
    Some(TaskLine {
        id,
        state,
        team,
        title,
    })
}

/// Parse the `agents <w>W/<i>I/<s>S` clause of the run-health line
/// (implementation-pins §3) — the arc's only source for the team-agent count
/// when forming a team on an empty board.
fn parse_run_health_agents(rest: &str) -> Option<(u32, u32, u32)> {
    let after = rest.split("agents ").nth(1)?;
    let clause = after.split_whitespace().next()?;
    let mut parts = clause.split('/');
    let working = parts.next()?.strip_suffix('W')?.parse().ok()?;
    let idle = parts.next()?.strip_suffix('I')?.parse().ok()?;
    let asleep = parts.next()?.strip_suffix('S')?.parse().ok()?;
    Some((working, idle, asleep))
}

/// Parse `task <id> — "<title>" (team <t>)`; presence ⟺ Working (ADR 0016).
fn parse_claimed(lines: &[String]) -> Option<ClaimedTask> {
    let line = lines
        .iter()
        .find(|line| !line.is_empty() && !is_placeholder(line))?;
    let rest = line.strip_prefix("task ")?;
    let (id_str, rest) = rest.split_once(' ')?;
    let id: u64 = id_str.parse().ok()?;
    let title = between_quotes(rest)?.to_owned();
    let team = rest
        .rsplit_once("(team ")
        .and_then(|(_, tail)| tail.strip_suffix(')'))
        .and_then(|tag| (tag != "-").then(|| tag.to_owned()));
    Some(ClaimedTask { id, title, team })
}

const WORK_VERBS: [&str; 3] = ["write_knowledge", "post_message", "search_knowledge"];

/// Count work-actions and read the `(degraded: <n> dropped)` marker
/// (implementation-pins §3): the mock reads only the verb name before `{`.
fn parse_recent(lines: &[String]) -> RecentActivity {
    let mut window = RecentActivity::default();
    let first_content = lines
        .iter()
        .find(|line| !line.is_empty() && !is_placeholder(line));
    if let Some(first) = first_content {
        window.degraded = first.starts_with("(degraded");
    }
    for line in lines {
        let Some(rest) = line.strip_prefix("- [turn ") else {
            continue;
        };
        let Some((_, action)) = rest.split_once("] ") else {
            continue;
        };
        let verb = action
            .split(|c: char| c == '{' || c.is_whitespace())
            .next()
            .unwrap_or_default();
        if WORK_VERBS.contains(&verb) {
            window.work_actions += 1;
        }
    }
    window
}

/// Parse `- msg <id> from <sender> (<kind>): "<body>"`.
fn parse_fresh(lines: &[String]) -> Vec<FreshMessage> {
    lines
        .iter()
        .filter_map(|line| {
            let rest = line.strip_prefix("- msg ")?;
            let (id_str, rest) = rest.split_once(" from ")?;
            let id: u64 = id_str.parse().ok()?;
            let (from, rest) = rest.split_once(" (")?;
            let (kind, rest) = rest.split_once("):")?;
            let body = between_quotes(rest)?.to_owned();
            Some(FreshMessage {
                id,
                from: from.to_owned(),
                kind: kind.to_owned(),
                body,
            })
        })
        .collect()
}

/// Parse `- directive <id> [<tier>, <state>] <kind>{<args>} from <meta>`.
fn parse_directives(lines: &[String]) -> Vec<DirectiveLine> {
    lines
        .iter()
        .filter_map(|line| {
            let rest = line.strip_prefix("- directive ")?;
            let (id_str, rest) = rest.split_once(' ')?;
            let id: u64 = id_str.parse().ok()?;
            let inner = rest.trim_start().strip_prefix('[')?;
            let (bracket, rest) = inner.split_once(']')?;
            let (tier, state) = bracket.split_once(", ")?;
            let rest = rest.trim_start();
            let (kind, rest) = rest.split_once('{')?;
            let (args_str, rest) = split_braced(rest)?;
            let from = rest.trim_start().strip_prefix("from ")?.to_owned();
            Some(DirectiveLine {
                id,
                tier: tier.to_owned(),
                state: state.to_owned(),
                kind: kind.to_owned(),
                args: parse_args(args_str),
                from,
            })
        })
        .collect()
}

/// Parse `- directive <id> [<tier>] <kind>{…} — <status>` into per-tier usage:
/// any outcome line of a tier (pending/fulfilled/declined) means the meta has
/// already spent that tier (ADR 0020 amendment).
fn parse_outcomes(lines: &[String]) -> DirectiveOutcomes {
    let mut outcomes = DirectiveOutcomes::default();
    for line in lines {
        let Some(rest) = line.strip_prefix("- directive ") else {
            continue;
        };
        let Some(inner) = rest.split_once('[').map(|(_, tail)| tail) else {
            continue;
        };
        let Some((tier, _)) = inner.split_once(']') else {
            continue;
        };
        match tier {
            "judgment" => outcomes.judgment_used = true,
            "mechanical" => outcomes.mechanical_used = true,
            _ => {}
        }
    }
    outcomes
}

/// Parse the utilization block of the metrics digest — the only metrics lines
/// the mock reads (implementation-pins §3).
fn parse_utilization(lines: &[String]) -> Vec<UtilizationLine> {
    let mut in_block = false;
    let mut parsed = Vec::new();
    for line in lines {
        if line.starts_with("utilization:") {
            in_block = true;
            continue;
        }
        if !in_block {
            continue;
        }
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("- ") else {
            // The block ends at the first non-list line (e.g. `mailbox:`).
            break;
        };
        if let Some(entry) = parse_utilization_line(rest) {
            parsed.push(entry);
        }
    }
    parsed
}

/// Parse `<agent>: <Idle|Working (task N)|Asleep>, <specialty> [(idle k)]`.
fn parse_utilization_line(rest: &str) -> Option<UtilizationLine> {
    let (agent, rest) = rest.split_once(": ")?;
    let (state, tail) = if let Some(tail) = rest.strip_prefix("Working (task ") {
        let (task_str, tail) = tail.split_once(')')?;
        let task: u64 = task_str.parse().ok()?;
        (AgentStateLine::Working { task }, tail)
    } else if let Some(tail) = rest.strip_prefix("Idle") {
        (AgentStateLine::Idle, tail)
    } else if let Some(tail) = rest.strip_prefix("Asleep") {
        (AgentStateLine::Asleep, tail)
    } else {
        return None;
    };
    let specialty_part = tail.strip_prefix(", ")?;
    let specialty = specialty_part
        .split(" (")
        .next()
        .unwrap_or(specialty_part)
        .trim()
        .to_owned();
    Some(UtilizationLine {
        agent: agent.to_owned(),
        state,
        specialty,
    })
}

/// The text between the first and last `"` of a line fragment.
fn between_quotes(fragment: &str) -> Option<&str> {
    let start = fragment.find('"')?;
    let end = fragment.rfind('"')?;
    (end > start).then(|| &fragment[start + 1..end])
}

/// Split a `…} from meta-1` tail at the brace matching an already-consumed
/// `{`, respecting nested brackets and quoted strings.
fn split_braced(rest: &str) -> Option<(&str, &str)> {
    let mut depth = 1_usize;
    let mut in_quotes = false;
    for (offset, character) in rest.char_indices() {
        match character {
            '"' => in_quotes = !in_quotes,
            '{' | '[' if !in_quotes => depth += 1,
            '}' | ']' if !in_quotes => {
                depth -= 1;
                if depth == 0 {
                    return Some((&rest[..offset], &rest[offset + 1..]));
                }
            }
            _ => {}
        }
    }
    None
}

/// Split a `key:value, key:value` gist on top-level `, ` boundaries,
/// respecting quotes and `[…]` lists (implementation-pins §3).
fn parse_args(gist: &str) -> DirectiveArgs {
    let mut pairs = Vec::new();
    let mut depth = 0_usize;
    let mut in_quotes = false;
    let mut start = 0_usize;
    let bytes_len = gist.len();
    for (offset, character) in gist.char_indices() {
        match character {
            '"' => in_quotes = !in_quotes,
            '[' | '{' if !in_quotes => depth += 1,
            ']' | '}' if !in_quotes => depth = depth.saturating_sub(1),
            ',' if !in_quotes && depth == 0 => {
                push_pair(&mut pairs, &gist[start..offset]);
                start = offset + 1;
            }
            _ => {}
        }
    }
    if start < bytes_len {
        push_pair(&mut pairs, &gist[start..]);
    }
    DirectiveArgs { pairs }
}

fn push_pair(pairs: &mut Vec<(String, String)>, fragment: &str) {
    let fragment = fragment.trim();
    if fragment.is_empty() {
        return;
    }
    let Some((key, value)) = fragment.split_once(':') else {
        return;
    };
    let value = value.trim();
    let value = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value);
    pairs.push((key.trim().to_owned(), value.to_owned()));
}

/// Turn-local state: the assistant/tool messages after the last user message.
fn parse_turn_local(messages: &[ChatMessage]) -> TurnLocal {
    let mut local = TurnLocal::default();
    for message in messages {
        match message {
            ChatMessage::Assistant { tool_calls, .. } => {
                for call in tool_calls.iter().flatten() {
                    local.called_verbs.push(call.function.name.clone());
                }
            }
            ChatMessage::Tool { content, .. } => {
                local.tool_messages += 1;
                let status = serde_json::from_str::<serde_json::Value>(&content.rendered_text())
                    .ok()
                    .and_then(|value| {
                        value
                            .get("status")
                            .and_then(|s| s.as_str())
                            .map(str::to_owned)
                    });
                local.statuses.push(match status.as_deref() {
                    Some("ok") => ToolStatus::Ok,
                    Some("rejected") => ToolStatus::Rejected,
                    Some("invalid") => ToolStatus::Invalid,
                    _ => ToolStatus::Unknown,
                });
            }
            _ => {}
        }
    }
    local
}

fn parse_tools(req: &ChatCompletionRequest) -> ToolIndex {
    let mut index = ToolIndex {
        calls_suppressed: matches!(
            req.tool_choice,
            Some(ToolChoice::Mode(ToolChoiceMode::None))
        ),
        ..ToolIndex::default()
    };
    for tool in req.tools.iter().flatten() {
        let ToolDef {
            kind: ToolType::Function,
            function,
        } = tool;
        index
            .schemas
            .insert(function.name.clone(), function.parameters.clone());
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;
    use openteam_wire::{FunctionCall, MessageContent, ToolCall};

    fn user_request(sections: &str) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "openteam-mock".into(),
            messages: vec![
                ChatMessage::System {
                    content: MessageContent::Text("skeleton".into()),
                    name: None,
                },
                ChatMessage::User {
                    content: MessageContent::Text(sections.into()),
                    name: None,
                },
            ],
            tools: None,
            tool_choice: None,
            parallel_tool_calls: None,
            user: None,
            safety_identifier: None,
            prompt_cache_key: None,
            stream: None,
            n: None,
        }
    }

    #[test]
    fn board_digest_lines_parse_per_the_pinned_grammar() {
        let world = RenderedWorld::parse(&user_request(
            "## Board digest\n\
             - task 1 [Claimed by agent-1] team:t1  \"Draft the setup section\"\n\
             - task 2 [Done] team:t1  \"Draft the architecture overview\"\n\
             - task 3 [Open] team:-  \"Loose end\"\n\
             - task 4 [Cancelled] team:t1  \"Dropped\"\n\
             run-health: done 1/4 · agents 1W/2I/0S · mailbox depth 0 (max 1) · ticks-since-done 0",
        ));
        let board = world.board.expect("board section present");
        assert_eq!(board.count(), 4);
        assert_eq!(
            board.tasks[0].state,
            TaskState::Claimed {
                by: "agent-1".into()
            }
        );
        assert_eq!(board.tasks[0].team.as_deref(), Some("t1"));
        assert_eq!(board.tasks[0].title, "Draft the setup section");
        assert_eq!(board.tasks[1].state, TaskState::Done);
        assert_eq!(board.tasks[2].state, TaskState::Open);
        assert_eq!(board.tasks[2].team, None, "`-` is the untagged marker");
        assert_eq!(board.tasks[3].state, TaskState::Cancelled);
        assert_eq!(board.run_health_agents, Some((1, 2, 0)));
        assert!(!board.all_terminal());
        assert_eq!(board.lowest_open().map(|t| t.id), Some(3));
        assert_eq!(board.claimants(), vec!["agent-1"]);
        assert_eq!(board.dominant_team(), Some("t1"));
    }

    #[test]
    fn empty_board_placeholder_parses_as_zero_tasks() {
        let world = RenderedWorld::parse(&user_request(
            "## Board digest\n(empty)\nrun-health: done 0/0 · agents 0W/3I/0S · mailbox depth 0 (max 0) · ticks-since-done 0",
        ));
        let board = world.board.expect("board section present");
        assert_eq!(board.count(), 0);
        assert_eq!(board.run_health_agents, Some((0, 3, 0)));
    }

    #[test]
    fn claimed_task_presence_and_id() {
        let world = RenderedWorld::parse(&user_request(
            "## Claimed task\ntask 2 — \"Draft the architecture overview\" (team t1)",
        ));
        let claimed = world.claimed.expect("claimed present means Working");
        assert_eq!(claimed.id, 2);
        assert_eq!(claimed.title, "Draft the architecture overview");
        assert_eq!(claimed.team.as_deref(), Some("t1"));

        let idle = RenderedWorld::parse(&user_request("## Claimed task\n(none)"));
        assert!(idle.claimed.is_none(), "placeholder means Idle");

        let untagged =
            RenderedWorld::parse(&user_request("## Claimed task\ntask 7 — \"X\" (team -)"));
        assert_eq!(untagged.claimed.expect("present").team, None);
    }

    #[test]
    fn recent_activity_counts_work_actions_only() {
        let world = RenderedWorld::parse(&user_request(
            "## Recent activity\n\
             - [turn 4] claim_task{task:1} -> ok\n\
             - [turn 6] write_knowledge{\"Setup: install mise…\"} -> ok\n\
             - [turn 8] post_message{team:t1, …} -> ok\n\
             - [turn 9] search_knowledge{\"deps\"} -> rejected\n\
             - [turn 10] release_task{} -> ok",
        ));
        let window = world.recent_activity.expect("section present");
        assert_eq!(window.work_actions, 3, "claim/release are not work-actions");
        assert!(!window.degraded);
    }

    #[test]
    fn degradation_marker_is_read_from_the_first_content_line() {
        let world = RenderedWorld::parse(&user_request(
            "## Recent activity\n(degraded: 2 dropped)\n- [turn 9] write_knowledge{…} -> ok",
        ));
        let window = world.recent_activity.expect("section present");
        assert!(window.degraded);
        assert_eq!(window.work_actions, 1);
    }

    #[test]
    fn fresh_messages_parse() {
        let world = RenderedWorld::parse(&user_request(
            "## Fresh messages\n\
             - msg 1 from orchestrator (direct): \"Prioritize the setup section.\"\n\
             - msg 3 from agent-1 (team:t1): \"Setup section drafted.\"",
        ));
        assert_eq!(world.fresh_messages.len(), 2);
        assert_eq!(world.fresh_messages[0].id, 1);
        assert_eq!(world.fresh_messages[0].from, "orchestrator");
        assert_eq!(world.fresh_messages[0].kind, "direct");
        assert_eq!(world.fresh_messages[1].kind, "team:t1");
    }

    #[test]
    fn directive_lines_parse_kind_and_args() {
        let world = RenderedWorld::parse(&user_request(
            "## Directives\n\
             - directive 1 [judgment, pending] propose_respecialize{agent:agent-3, specialty:doc-reviewer} from meta-1\n\
             - directive 2 [judgment, pending] propose_reallocate{task:2, reason:\"stuck, needs a move\"} from meta-1\n\
             - directive 3 [judgment, pending] propose_rebalance{team:t1, members:[agent-1 agent-2]} from meta-1",
        ));
        assert_eq!(world.directives.len(), 3);
        let first = &world.directives[0];
        assert_eq!(first.id, 1);
        assert_eq!(first.tier, "judgment");
        assert!(first.is_pending());
        assert_eq!(first.kind, "propose_respecialize");
        assert_eq!(first.args.get("agent"), Some("agent-3"));
        assert_eq!(first.args.get("specialty"), Some("doc-reviewer"));
        assert_eq!(first.from, "meta-1");
        // Quoted values keep their embedded comma; quotes are stripped.
        assert_eq!(
            world.directives[1].args.get("reason"),
            Some("stuck, needs a move")
        );
        assert_eq!(world.directives[1].args.get("task"), Some("2"));
        assert_eq!(
            world.directives[2].args.get_list("members"),
            Some(vec!["agent-1".to_owned(), "agent-2".to_owned()])
        );
    }

    #[test]
    fn directive_outcomes_read_per_tier() {
        let none = RenderedWorld::parse(&user_request("## Directive outcomes\n(none issued)"));
        let outcomes = none.directive_outcomes.expect("section present");
        assert!(!outcomes.judgment_used && !outcomes.mechanical_used);

        let both = RenderedWorld::parse(&user_request(
            "## Directive outcomes\n\
             - directive 1 [judgment] propose_respecialize{agent:agent-3, specialty:doc-reviewer} — fulfilled by orchestrator\n\
             - directive 2 [mechanical] set_parallelism{target:2} — fulfilled by runtime",
        ));
        let outcomes = both.directive_outcomes.expect("section present");
        assert!(outcomes.judgment_used && outcomes.mechanical_used);
    }

    #[test]
    fn utilization_lines_parse_state_and_specialty() {
        let world = RenderedWorld::parse(&user_request(
            "## Metrics digest\n\
             throughput: 1 task_completed / 15 EventIds · latency: work median 6 EventIds\n\
             utilization:\n\
             \u{20}\u{20}- agent-1: Working (task 1), generalist\n\
             \u{20}\u{20}- agent-2: Idle, generalist (idle 0)\n\
             \u{20}\u{20}- agent-3: Asleep, doc-reviewer\n\
             mailbox: depth 0, max 1, oldest-pending-age 0",
        ));
        assert_eq!(world.utilization.len(), 3);
        assert_eq!(
            world.utilization[0].state,
            AgentStateLine::Working { task: 1 }
        );
        assert_eq!(world.utilization[0].specialty, "generalist");
        assert_eq!(world.utilization[1].state, AgentStateLine::Idle);
        assert_eq!(world.utilization[1].specialty, "generalist");
        assert_eq!(world.utilization[2].state, AgentStateLine::Asleep);
        assert_eq!(world.utilization[2].specialty, "doc-reviewer");
    }

    #[test]
    fn turn_local_reads_statuses_and_verbs() {
        let mut req = user_request("## Goal\nX");
        req.messages.push(ChatMessage::Assistant {
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "call_1".into(),
                kind: ToolType::Function,
                function: FunctionCall {
                    name: "claim_task".into(),
                    arguments: "{\"task\":1}".into(),
                },
            }]),
            refusal: None,
            name: None,
        });
        req.messages.push(ChatMessage::Tool {
            content: MessageContent::Text(
                "{\"status\":\"rejected\",\"code\":\"task_not_open\",\"message\":\"…\"}".into(),
            ),
            tool_call_id: "call_1".into(),
        });
        let world = RenderedWorld::parse(&req);
        assert!(world.turn_local.acted());
        assert_eq!(world.turn_local.called_verbs, vec!["claim_task"]);
        assert_eq!(world.turn_local.statuses, vec![ToolStatus::Rejected]);
    }

    #[test]
    fn absent_sections_are_tolerated() {
        let world = RenderedWorld::parse(&user_request("hello, plain client"));
        assert!(world.board.is_none());
        assert!(world.claimed.is_none());
        assert!(world.recent_activity.is_none());
        assert!(world.directives.is_empty());
        assert!(!world.turn_local.acted());
    }

    #[test]
    fn tools_index_and_args_fit() {
        let mut req = user_request("## Goal\nX");
        req.tools = Some(vec![ToolDef {
            kind: ToolType::Function,
            function: openteam_wire::FunctionDef {
                name: "claim_task".into(),
                description: None,
                parameters: Some(serde_json::json!({
                    "type": "object",
                    "properties": {"task": {"type": "integer"}},
                    "required": ["task"],
                    "additionalProperties": false
                })),
                strict: Some(false),
            },
        }]);
        let world = RenderedWorld::parse(&req);
        assert!(world.tools.available("claim_task"));
        assert!(!world.tools.available("finish_run"));
        assert!(
            world
                .tools
                .args_fit("claim_task", &serde_json::json!({"task": 1}))
        );
        assert!(
            !world.tools.args_fit("claim_task", &serde_json::json!({})),
            "missing required key"
        );
        assert!(
            !world
                .tools
                .args_fit("claim_task", &serde_json::json!({"task": 1, "x": 2})),
            "stray key"
        );
    }

    #[test]
    fn tool_choice_none_suppresses_calls() {
        let mut req = user_request("## Goal\nX");
        req.tools = Some(vec![ToolDef {
            kind: ToolType::Function,
            function: openteam_wire::FunctionDef {
                name: "sleep".into(),
                description: None,
                parameters: None,
                strict: None,
            },
        }]);
        req.tool_choice = Some(ToolChoice::Mode(ToolChoiceMode::None));
        let world = RenderedWorld::parse(&req);
        assert!(!world.tools.available("sleep"));
    }
}
