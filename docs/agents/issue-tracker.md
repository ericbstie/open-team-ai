# Issue tracker: GitHub

Issues and PRDs for this repo live as GitHub issues on `ericbstie/open-team-ai`.

**This environment has no `gh` CLI and no direct GitHub API access.** All issue
operations go through the GitHub MCP server tools (`mcp__github__*`). Load their
schemas first with ToolSearch, e.g. query:
`select:mcp__github__issue_write,mcp__github__issue_read,mcp__github__list_issues,mcp__github__sub_issue_write,mcp__github__add_issue_comment,mcp__github__search_issues`

Every call takes `owner: "ericbstie"`, `repo: "open-team-ai"`.

## Conventions

- **Create an issue**: `issue_write` with `method: "create"` plus `title`, `body`,
  `labels`. Labels auto-create on first use (verified) — no separate label setup.
- **Read an issue**: `issue_read` with `method: "get"` (body, state, assignees,
  database `id`); `method: "get_comments"` for comments; `method: "get_labels"`
  for labels; `method: "get_sub_issues"` for children.
- **List issues**: `list_issues` with `labels` / `state` filters.
- **Comment on an issue**: `add_issue_comment` with `issue_number`, `body`.
- **Apply / remove labels, assign, retitle, edit body**: `issue_write` with
  `method: "update"`. Note `labels` and `assignees` **replace** the full set —
  read first, then write the merged list.
- **Close**: `issue_write` update with `state: "closed"` and a `state_reason`
  (`completed` for resolved, `not_planned` for out-of-scope), after posting the
  resolution comment.
- **Numbers vs ids**: humans see issue `#number`; the sub-issue API needs the
  numeric **database id** returned as `id` by `issue_read`/`issue_write`. Don't
  mix them up.

## Pull requests as a triage surface

**PRs as a request surface: no.** _(Set to `yes` if this repo treats external PRs
as feature requests; `/triage` reads this flag.)_

## When a skill says "publish to the issue tracker"

Create a GitHub issue via `issue_write`.

## When a skill says "fetch the relevant ticket"

`issue_read` with `method: "get"`, then `method: "get_comments"`.

## Wayfinding operations

Used by `/wayfinder`. The **map** is a single issue with **child** issues as tickets.

- **Map**: a single issue labelled `wayfinder:map`, holding the Destination /
  Notes / Decisions-so-far / Not-yet-specified / Out-of-scope body. Find it with
  `list_issues` filtering `labels: ["wayfinder:map"]`.
- **Child ticket**: create the issue with `issue_write` (label it
  `wayfinder:<type>` — `research` / `prototype` / `grilling` / `task`), then
  attach it to the map with `sub_issue_write` `method: "add"`,
  `issue_number: <map number>`, `sub_issue_id: <child database id>`.
- **Blocking**: GitHub's native issue-dependencies API is **not** exposed through
  the MCP server here, so the body convention is canonical for this repo: a line
  `Blocked by: #<n>, #<m>` at the **top** of the child body. A ticket is
  unblocked when every issue listed there is closed. Keep the line current if
  blockers change.
- **Frontier query**: `issue_read` `method: "get_sub_issues"` on the map, keep
  the OPEN ones (their order is map order), drop any with an assignee or with an
  open issue in its `Blocked by:` line; first remaining wins.
- **Claim**: `issue_write` update setting `assignees: ["ericbstie"]` — the
  session's first write. An open, unassigned ticket is unclaimed.
- **Resolve**: post the answer as a comment (`add_issue_comment`), close the
  issue (`issue_write`, `state: "closed"`, `state_reason: "completed"`), then
  append a one-line context pointer (gist + link) to the map body's
  **Decisions so far** section via `issue_write` update on the map.
