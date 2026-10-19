# project-boards: a Linear backend (phase 0 and 0.4)

**Status:** Design approved in brainstorming (2026-10-15); pending written-spec review
**Date:** 2026-10-15
**Builds on:** `2026-09-30-project-boards-design.md` (the main spec). Sections 3–7 of that spec still hold for GitHub; this document only says what changes.
**Research:** `docs/superpowers/research/2026-10-14-github-coupling-map.md`, `docs/superpowers/research/2026-10-14-linear-api.md`. Linear facts below were re-checked on 2026-10-15 against Linear's published `schema.graphql` (commit `ecbd310`, 2026-10-14) and the official developer pages; anything still unverified is a phase 0 probe (section 9).

## 1. Purpose

Make project-boards work with Linear as well as GitHub Projects, so a herdr user whose team plans in Linear can see their board without leaving the terminal.

- **Phase 0 (no release):** a Linear testbed workspace, Linear's schema pinned in the repository, a seed program, and probes that settle the open facts in section 9.
- **0.4 — Linear viewer:** a provider seam in the code, with GitHub behaviour unchanged and every existing test green, plus a read-only Linear viewer: project and team boards, table and board layouts, built-in tabs and shared custom views, the `f` filter, quick search, item detail, caching and polling.
- **0.5 — Linear editing:** outlined in section 11 only; it gets its own spec once 0.4 has shipped and the probes have answered.
- **0.6 — Views and bulk:** the main spec's former 0.4 (save and create views, local group-by and sort, visual select and bulk edits, mouse drag) moves here, so it is built on the provider seam instead of adding more GitHub-only code to untangle.

### Success criteria for 0.4

From herdr alone, a user with a Linear personal API key can:

1. Open a Linear project or team board from the picker, and have it reopen directly next time from the same repository.
2. See it as a table or a kanban board with columns from the workflow states, switch between the built-in tabs and the team's shared custom views, filter it with `f` and search it with `/`.
3. Open an issue and read its description, comments, parent, sub-issues, project, cycle, priority, estimate, due date and attachments.
4. See other people's changes appear within one poll interval.

And a GitHub user notices no difference, except the picker gaining a Linear section when a Linear key is configured, and the board cache being refetched once.

## 2. Scope

### In scope

- Phase 0 and 0.4 as above.
- Linear personal API keys.
- One Linear workspace at a time (the one the configured key belongs to).

### Out of scope for 0.4

- Any Linear write. Every edit key on a Linear board is disabled with the reason "Editing Linear boards arrives in 0.5".
- OAuth (with PKCE). A later release.
- Several Linear workspaces at once. The board key leaves room for it (section 5).
- Guessing a team from the branch name. Deferred; remembering a board per repository covers repeat opens.
- Cycle boards and custom views as boards of their own. A board is a project or a team (section 3).
- Initiatives, roadmaps, project updates, triage handling beyond showing the Triage column, the `linear://` desktop link.

## 3. Decisions

| Area | Decision |
|---|---|
| Board kinds | Two: a **project board** (a Linear project's issues; it can span teams) and a **team board** (all of one team's issues). The picker lists projects first, then teams. |
| Columns | The Status field's options are the workflow states of every team on the board, merged by name (the existing same-name column rule), ordered by state kind (triage, backlog, unstarted, started, completed, canceled, duplicate) and then by position. A team board is the one-team case. |
| Provider dispatch | An enum `Backend { Github(Github), Linear(Linear) }` matched at the `Syncer` and `ops::send` seam. No `dyn Provider` trait. Provider-only lookups stay inherent methods of each client. |
| Linear properties | Mapped onto existing field kinds as fixed fields (section 4). `OptionColor` gains `Hex`; `SelectOption` gains an optional state `kind`. |
| Issues | Linear issues are `ItemContent::Issue`. `ContentRef` becomes `{ container, key, number, url }`. Open or closed comes from the state kind. |
| Board identity | `BoardKey` replaces `BoardRef`. GitHub's string form is unchanged (`owner/3`); Linear's is `linear:project:<uuid>` or `linear:team:<uuid>`. `state.json` and `panes.json` need no migration. |
| Credentials | `LINEAR_API_KEY`, then `linear_api_key_command` from `config.toml`, run without a shell. Header `Authorization: <key>`, no `Bearer`. |
| Opening a board | Remember the board per repository for any provider; one picker with a section per provider that has credentials; the setup screen only when no provider has credentials. |
| Filters | Views are server-built item id lists (as today). `f` on a Linear board is the same GitHub-style syntax, evaluated on the client over the view's loaded items. |
| Sync | A `SyncProfile` per provider replaces the GitHub constants. Linear polls with `updatedAt` and `includeArchived: true`, with every nested connection bounded. |
| Errors | One provider-neutral `BackendError` replaces `GithubError`, with a `remedy()` helper. |
| Testing | A second pinned schema, recorded Linear fixtures with a scrubber, live tests behind `--features live` guarded against any workspace but the testbed. |
| Milestones | Phase 0, then 0.4 Linear viewer, 0.5 Linear editing, 0.6 Views and bulk, then 1.0. |

## 4. Domain model changes

All of these land with GitHub behaviour unchanged.

### Board identity

```rust
pub enum BoardKey {
    Github { owner: String, number: u32 },  // shown and parsed as "owner/3" (unchanged)
    LinearProject { id: String },            // "linear:project:<uuid>"
    LinearTeam { id: String },               // "linear:team:<uuid>"
}
```

- `FromStr`: a string starting `linear:` is Linear; anything else parses as `owner/number`, exactly as `BoardRef` does today.
- `key()` (file-system safe): GitHub keeps `owner__number`; Linear gives `linear__project__<uuid>` and `linear__team__<uuid>`.
- Linear keys hold the UUID, never the team key or project name, which can be renamed.
- A Linear key contains no `/`, so `layout_overrides` entries (`<board>/<view>`, matched by the prefix `"<board>/"`) cannot collide.
- The workspace is not part of the key: one configured key reaches one workspace. If several workspaces are supported later, a form such as `linear:<workspace>:project:<uuid>` can be added the same way, with the bare `linear:` form meaning the default workspace.

### Items

- `ContentRef { container, key, number, url }`:
  - `container`: `owner/name` for GitHub, the team key for Linear;
  - `key`: the reference shown to people, `#12` or `ENG-123`;
  - `number`: kept for sorting and for matching typed references.
- `Item::ref_label()` returns `key` (still `new` for a placeholder and `Draft` for a draft).
- A Linear issue's `ContentState` and `StateReason` come from its workflow state's kind:

  | State kind | `ContentState` | `StateReason` |
  |---|---|---|
  | completed | Closed | Completed |
  | canceled | Closed | NotPlanned |
  | duplicate | Closed | Duplicate |
  | triage, backlog, unstarted, started | Open | — |

- `PullRequest`, `Draft` and `Redacted` never occur on a Linear board.
- `FieldValue::Users` holds the Linear user's display name. `ContentFields.assignee_ids` maps it to the user's UUID, as it maps logins to node ids for GitHub.

### Fields of a Linear board

The Linear client builds the board's `Project` with these fields. Their ids are fixed strings so cached views and state stay valid across runs.

| Linear | Field id | Field kind | Notes |
|---|---|---|---|
| Workflow state | `linear:status` | SingleSelect | Options are the states (section 3, Columns); each option carries its `kind` and hex colour. The default column field. |
| Priority | `linear:priority` | SingleSelect | Fixed options: Urgent, High, Medium, Low, No priority, in that order. |
| Estimate | `linear:estimate` | Number | Read as a float. |
| Due date | `linear:due` | Date | A date with no time zone; never converted through UTC. |
| Cycle | `linear:cycle` | Iteration | Start and end give `start_date` and `duration_days`; past cycles are `completed`. Absent when no team on the board has cycles enabled. |
| Single-select label group | `linear:group:<uuid>` | SingleSelect | One field per group, named after it; options are its labels. Can be a column field or a swimlane. |
| Project | `linear:project` | SingleSelect | Team boards only; options are the team's projects. |
| Labels | built-in `Labels` | Labels | Labels outside single-select groups, including multi-select groups. |
| Assignee | built-in `Assignees` | Assignees | At most one. |
| Title, created, updated, closed, parent | the existing built-ins | | `closed` is `completedAt` or `canceledAt`. |
| Sub-issues | built-in `SubIssuesProgress` | | Filled only in the item detail; list queries do not fetch children (section 6). |

### Colours and option kinds

- `OptionColor::Hex(u32)` joins the eight GitHub colours. It is drawn like label pills are today: exact with `COLORTERM=truecolor`, otherwise the nearest of 256.
- `SelectOption { id, name, color, kind: Option<OptionKind> }`. `OptionKind` is the workflow state kind. GitHub options have `None`. An unknown kind string from Linear becomes `None` and sorts after the known kinds.
- Priority options use named colours: Urgent red, High orange, Medium yellow, Low blue. No priority has no pill.

### Views

- `View.filter: String` becomes `View.filter: ViewFilter`:

  ```rust
  pub enum ViewFilter {
      Github(String),           // GitHub filter syntax, sent as items(query:)
      LinearView(String),       // a custom view id; evaluated by customView.issues
      Linear(serde_json::Value) // an IssueFilter built in code (built-in tabs)
  }
  ```

- `LayoutOverride.github_layout` becomes `server_layout`, with `#[serde(alias = "github_layout")]` so existing `state.json` files still load.

### Cache

`CACHE_VERSION` goes from 3 to 4 because `ContentRef`, `SelectOption` and `View` change shape. Each existing cache is discarded once and refetched; nothing the user chose is lost, because choices live in `state.json`.

## 5. Architecture

### The provider seam

```rust
pub enum Backend {
    Github(Github),
    Linear(Linear),
}
```

- `Syncer` holds `Arc<Backend>` instead of `Arc<Github>`. Each `Syncer` job matches once and calls the provider's own method: resolve the board, fetch the board's fields and views, a page of items, a view's id list, an item's detail, the poll.
- `ops::send::send(backend, project, op)` matches on the backend. In 0.4 the Linear arm is unreachable, because `caps` allows no Linear edit; it returns `BackendError::Api("not supported")` so a bug cannot panic.
- GitHub-only lookups (repository candidates, pushable repositories, content search) stay as `Github` methods; their `SyncJob`s run only for GitHub boards.
- Each backend supplies:
  - `sync_profile() -> SyncProfile` (section 7);
  - `provider() -> Provider` (`Github` or `Linear`), used for wording, remedies and the picker;
  - an `EditContext` read-only reason: none for GitHub; "Editing Linear boards arrives in 0.5" for Linear.

### The Linear module

`src/linear/`, mirroring `src/github/`: `client.rs` (the facade: model types in, model types out), `convert.rs` (hand decoding from `serde_json::Value`), `transport.rs`, `token.rs`, `fixture.rs`, `record.rs`.

- **Transport:** reuses the `Transport` trait's request and response shape. `HttpTransport` gains the endpoint, the header style and the rate-limit header names as parameters, so GitHub and Linear differ only in configuration. `FixtureTransport` and `RecordingTransport` are shared; the Linear recorder adds the scrubber (section 10).
- **Operations:** every Linear query is a named operation in `crates/queries/graphql/linear/linear.graphql`, validated by `graphql_client` against `crates/queries/graphql/linear/schema.graphql`. Generated types never leave `crates/queries` and `src/linear/`.
- **The boundary rule** from the main spec now reads: types generated from either provider's schema never leave that provider's module and `ops`.

### Credentials

- Linear: `LINEAR_API_KEY`, then `linear_api_key_command` in `config.toml`:

  ```toml
  linear_api_key_command = ["security", "find-generic-password", "-s", "project-boards-linear", "-w"]
  ```

  The command runs directly, without a shell. Its standard output, trimmed, is the key. It is never logged, cached or written to a state file. The existing `Token` type (redacted in `Debug`) holds it.
- The README shows storing the key in the macOS Keychain with `security add-generic-password -s project-boards-linear -a "$USER" -w` (which prompts for it), and names `op read`, `pass show` and `secret-tool lookup` as other sources.
- GitHub credentials are unchanged.
- Each provider's credentials resolve independently. A missing Linear key hides the Linear picker section behind one dim line naming the fix; a missing GitHub token does the same for GitHub. The setup screen appears only when no provider has credentials, and lists both fixes.

### Errors

```rust
pub enum BackendError {
    NoCredentials { provider: Provider },
    Unauthorized { provider: Provider },
    MissingPermission { provider: Provider, needed: Vec<String> },
    RateLimited { retry_after: Option<Duration> },
    Api(String),
    Network(String),
    Decode(String),
}
```

- `remedy()` gives the fix for the first three variants: `gh auth login` and `gh auth refresh -s …` for GitHub; "create a personal API key in Linear (Settings → Security & access) and store it" for Linear. `caps::Denial::message` uses the same helper, so the `gh` commands live in one place.
- GitHub maps exactly as `GithubError` does today; `InsufficientScopes` becomes `MissingPermission` with the scope list, which still feeds `EditContext`.
- Linear: HTTP 400 with `extensions.code == "RATELIMITED"` becomes `RateLimited`, with the wait taken from the `X-RateLimit-*-Reset` headers (there is no `retry-after`); an authentication failure becomes `Unauthorized`; a refused request becomes `MissingPermission` with no list. Phase 0 probe L6 confirms the exact codes.
- Log lines name the variant only, never the server's message text, as today.

### CLI, environment and herdr

- `open --project` accepts `OWNER/NUMBER` (unchanged), `linear:team:KEY` (a team key, resolved to its UUID), `linear:project:<slug-or-uuid>`, and a Linear project URL.
- `PB_BOARD` carries the canonical `BoardKey` string.
- The pane registry and `state.json` store `BoardKey` strings; their format for GitHub boards is unchanged.
- The manifest's description mentions Linear. Its actions are unchanged.
- `doctor` checks each configured provider. For Linear: the key resolves, `viewer` answers, and `rateLimitStatus` reports the remaining budget; it prints the workspace's URL key so the user can see which workspace the key reaches.

## 6. Opening and showing a board

### Choosing the board

`board_choice` keeps its order: `--project`, then the picker if asked, then the board remembered for the pane's repository, then the repository's linked GitHub boards, then the picker. The remembered board may be of either provider. A pane outside a git repository goes to the picker.

The picker has one section per provider with credentials: GitHub boards (the repository's linked boards first, as today), then Linear projects (open ones; completed and canceled projects are left out), then Linear teams. `ProjectSummary` gains `provider` and `kind`.

### Loading a Linear board

1. Resolve: for a project, the project and its teams; for a team, the team. Then each team's workflow states, label groups, cycles (when enabled) and, for a team board, its projects. These become the board's `Project` and fields (section 4).
2. Views: the built-in tabs, then the shared custom views from `customViews(filter: { team: … })` for each of the board's teams (Linear has no `Team.views`; project-scoped views are excluded by that query, see probe L4).
3. Items: `issues(filter: <board filter>, first: 100, orderBy: updatedAt)` page by page, up to `max_items`. The board filter is `{ project: { id: { eq } } }` or `{ team: { id: { eq } } }`.
4. The active view's id list: `customView.issues(filter: <board filter>)` for a custom view, or `issues(filter: { and: [<board filter>, <tab filter>] })` for a built-in tab, selecting ids only. Ids not in the item set are loaded by id, as GitHub's hydrate step does.
5. The cache paints first and is marked stale, as today.

Every nested connection in a list query is bounded (`labels(first: 10)`). Sub-issues are not fetched in lists; the detail loads them.

Linear's connections have no total count, so when `max_items` is reached the header says "loaded 2000+" instead of "loaded N of M".

### Tabs

| Tab | Layout | Filter on top of the board | Visible fields |
|---|---|---|---|
| Board | board, Status columns | hides canceled and duplicate issues, and completed issues finished more than 14 days ago | Priority, Labels, Assignee, Estimate |
| All issues | table | none | Status, Priority, Assignee, Labels, Cycle, Estimate, Due date, Updated |
| My issues | table | `assignee: { isMe: { eq: true } }` | as All issues |

Custom views follow their `viewPreferencesValues`:

- `layout` gives the layout;
- `issueGrouping` maps to Status, Priority, a label-group field, Project or Cycle when recognised; otherwise Status columns on a board and no grouping in a table;
- the `field*` booleans give the visible fields;
- `viewOrdering` maps to manual order (`sortOrder`), priority (`prioritySortOrder`), or created, updated or due date; anything else is manual order;
- the string values are recorded in phase 0 (probe L3) and only recognised values are mapped.

Sorting is always done on the client: Linear's server-side `sort` argument is marked internal, and `orderBy` offers only `createdAt` and `updatedAt`.

### Filtering

- The view's own filter is evaluated by Linear (custom views) or built in code (built-in tabs), and gives the view's id list.
- `f` takes the same syntax as on GitHub boards (`status:"In Progress"`, `assignee:@me`, `label:bug`, `priority:high`, `cycle:@current`, `-label:x`, `no:assignee`, free text) and is evaluated on the client over the view's loaded items. `x` clears it.
- `model/filter.rs` becomes a full evaluator over model types. On Linear boards `column_constraint` and `still_matches` use it, so they are exact. GitHub boards keep sending `f` to the server and keep today's estimate in `still_matches`.
- "Show archived items" sets `includeArchived: true` on the view's list and shows archived items with the existing marker. It stays a session toggle that `x` clears.
- Quick search (`/`) is unchanged.

### Cards, table and detail

- A card's header shows the state dot in the state's colour, `ENG-123`, and the assignee's display name. The title wraps; field pills follow; labels come last.
- Columns are ordered by state kind, then position; states with the same name merge.
- The detail renders the description and comments as Markdown. Comments are oldest first, replies indented one level under their parent. It shows read-only the parent, sub-issues with their states, project, cycle, priority, estimate, due date, and attachments as links (GitHub pull request URLs get the existing `⑂` pill). The newest 20 comments load first, as today.
- In Markdown, references like `ENG-123` whose prefix is one of the board's team keys jump to that item's detail when it is on the board and open the browser otherwise. `#123` autolinks are off on Linear boards.
- The header shows the board's name and a `Linear` badge.
- Text that says "GitHub" where it means the server names the board's provider instead.
- `o` opens the issue's `https://linear.app/…` URL.

## 7. Sync

`SyncProfile` per provider replaces the GitHub-only constants:

| | GitHub (unchanged) | Linear |
|---|---|---|
| Full-load page | 100 | 100 (probe L2 may change it) |
| Poll | `updated:>=<date>`, 20 per page | `updatedAt: { gt: <last poll> }`, `includeArchived: true`, `orderBy: updatedAt`, 50 per page |
| Poll sees archivals | no | yes |
| Recent window (confirmed edits outrank polls) | 5 minutes | 5 minutes until probe L1, then set from its result |
| Edits in flight | 4 | 4 (used from 0.5) |

- Poll intervals, focus and idle handling, and the 10-minute full refresh are unchanged and shared (`Scheduler` already knows nothing of providers).
- Budget: a 100-issue page is estimated at about 2,300 complexity points against the 10,000-point cap per query. Polling at 30 seconds uses about 120 requests an hour per pane, against 2,500 an hour per key.
- Linear's `X-RateLimit-Requests-*` and `X-RateLimit-Complexity-*` headers fill `RateInfo`. When either remaining budget falls below 10%, polling slows as it does for GitHub. `RateLimited` pauses polling until the reset time.

## 8. Errors and edge cases specific to Linear

- **A board that no longer exists** (deleted project, team the key cannot see): the same "board not found" screen as for GitHub, with the picker one key away; the remembered board for the repository is forgotten.
- **An unknown state kind:** the option's `kind` is `None`; its column sorts after the known kinds; it counts as open.
- **A team with cycles disabled:** no Cycle field for that team's issues; on a project board the field exists if any team has cycles.
- **Several teams with differently named states:** each name is its own column; same names merge.
- **The Free plan's limits** (250 issues, 2 teams): nothing special; the viewer works within them.
- **Search rate limit:** `searchIssues` is limited to 30 requests a minute. 0.4 does not search.

## 9. Phase 0: verify before building

Each probe is a step in `examples/probe_linear.rs`, run against the testbed only (section 10, guard). Results are written to `docs/phase0-findings.md` under a "Linear" heading.

| # | Question | Depends on it | Fallback if the answer is bad |
|---|---|---|---|
| L1 | How long until an `issueUpdate` shows in `issues(filter: updatedAt…)` and in `customView.issues`? | Recent window | Keep 5 minutes |
| L2 | What does a real 100-issue page cost (`X-Complexity`)? Is `first: 250` accepted? | Page sizes | Smaller pages |
| L3 | What string values do `viewPreferencesValues.layout`, `issueGrouping` and `viewOrdering` take, for views set up in Linear's web app? | Custom view mapping | Unknown values use the defaults (section 6) |
| L4 | Does the Free plan allow custom views? Are project-scoped views reachable through a public query? | Custom view tabs | Built-in tabs only |
| L5 | Does `customView.issues` apply the view's `showCompletedIssues`, or return everything? | Matching Linear's web app | Apply it on the client |
| L6 | What do a bad key, a revoked key and a refused request return (HTTP status and `extensions.code`)? | `BackendError` mapping | Map by HTTP status only |
| L7 | Does a trashed issue appear in an `updatedAt` poll? Does an archived one with `includeArchived: true`? | Poll vs full refresh | The full refresh catches them |
| L8 | Does `project.teams` list every team with issues in the project? | Merged columns | Collect teams from the issues' `team` |
| L9 | Does `issue(id: "<KEY>-1")` work, and is `searchIssues` limited to 30 a minute? | Reference jumps (0.4); add existing (0.5) | Look up by UUID only |

Also confirmed during design (2026-10-15, schema and official docs): `CustomView.issues` exists and ANDs an extra `filter`; workflow state types are `triage`, `backlog`, `unstarted`, `started`, `completed`, `canceled`, `duplicate` (a String); `IssueLabel.groupType` is `singleSelect` or `multiSelect`; `IssueCreateInput.id` accepts a client UUID v4; `Team` has no views field; server-side `sort` is internal; rate limiting returns HTTP 400 with `RATELIMITED` and no `retry-after`; personal keys use `Authorization: <key>`; `IssueConnection` has no total count; `estimate` and `priority` are floats on read and integers on write; the Free plan includes API access, 2 teams and 250 issues.

## 10. Testing

Test-driven throughout, with at least 80% line coverage at the release.

- **Schema:** Linear's `schema.graphql` is pinned in `crates/queries/graphql/linear/` with `scripts/update-schema.sh` extended to fetch it. The weekly schema-drift job checks both schemas.
- **Recorded fixtures:** `tests/fixtures/recorded/linear/<scenario>/<Operation>__N.json`, recorded from the testbed with `PB_RECORD=1` and replayed through `FixtureTransport` in plain `cargo test`. Hand-written fixtures, marked as such, cover rate limiting, authentication failures and unknown state kinds.
- **The scrubber:** the Linear recorder replaces every `name`, `displayName`, `email` and `avatarUrl` with fixed invented values, and the workspace URL key with an invented one, before writing a fixture. A test fails if any fixture contains a value the recorder replaced; the real values reach that check only through environment variables at recording time and are never written into the repository.
- **Live tests:** `tests/live_linear.rs`, behind `--features live`, configured by `tests/linear-testbed.toml` (workspace URL key, team ids, project id). They create their own throwaway issues and remove them even when an assertion fails.
- **The workspace guard:** the testbed key comes only from `PB_LINEAR_TESTBED_KEY` or its own Keychain entry, never `LINEAR_API_KEY`. Every live test, the seed program and every probe first check that `viewer.organization.urlKey` equals the URL key in `tests/linear-testbed.toml`, and stop before any other request if it does not.
- **The seed program:** `examples/seed_linear_testbed.rs`, idempotent. It creates two teams with invented keys, cycles on one team, a single-select label group "Type" and a multi-select group "Area", one project spanning both teams, about 40 issues covering every priority, estimates, due dates, parents and sub-issues, archived, canceled and duplicate issues and issues outside the project, and two shared custom views. It stays well below the Free plan's 250 issues.
- **GitHub:** every existing test passes unchanged at every slice, apart from constructor renames (`GithubError` → `BackendError`, `BoardRef` → `BoardKey`).
- **No real data:** fixtures, docs and tests hold invented data only. The user's real Linear workspace is never recorded or probed.

## 11. 0.5 outline: Linear editing (direction only)

Questions for the 0.5 spec, to be answered after 0.4 ships and with phase 0's results:

- How `Op` and `ContentId` generalise: Linear needs one content id (the issue UUID) and no Issue/PR/Draft split.
- Capability flags per provider for GitHub-only operations: drafts, convert, remove from project, milestones, close reasons, several assignees.
- Moves into a merged column: which team's state of that name the issue takes, and what happens when its team has none (fall back to a state of the same kind, or refuse).
- Close and reopen as "move to a state of kind completed, canceled or unstarted".
- Reordering with Linear's single `sortOrder`, shared by every view: a float between the neighbours.
- Creating issues with a client-generated UUID, so a retried create cannot make a duplicate; which team a new issue on a project board belongs to.
- Labels in single-select groups as field edits; Linear's one assignee in the assignee checklist.
- The recent window and the op queue's in-flight limit, from probe L1 and the rate-limit headers.

The 0.4 seams this relies on: `Backend` at `ops::send`, `BackendError`, `EditContext`'s provider read-only reason, option `kind`, and fixed field ids.

## 12. Delivery

Each slice is a short branch off `main` and a PR into `main`, merged by the maintainer with `--merge`. GitHub behaviour is unchanged at every merge and the full check passes on each PR. The first PR moves the version to `0.4.0-dev`.

| # | Slice | Contents |
|---|---|---|
| 1 | Neutral error | `BackendError` with `remedy()` replaces `GithubError`; `Denial` messages use it |
| 2 | Model generalised | `BoardKey`; `ContentRef { container, key, number, url }`; `OptionColor::Hex`; `SelectOption.kind`; `ViewFilter`; `server_layout` with the serde alias; cache version 4 |
| 3 | Backend seam | `Backend` at `Syncer` and `send`; `SyncProfile`; credentials per provider; setup screen only with no provider; `EditContext` read-only reason |
| 4 | Filter evaluator | `model/filter.rs` as a full client-side evaluator |
| P0 | Linear phase 0 | pinned schema, seed program, `tests/linear-testbed.toml`, probes L1–L9, findings (alongside slices 1–4; must finish before slice 5) |
| 5 | Linear client, read side | transport configuration, credentials, error mapping, the board's fields and views, full load, poll, view id lists, detail, the scrubber, recorded fixtures |
| 6 | Wiring | `Backend::Linear`, picker sections, CLI forms, `doctor`, badge, wording, reference jumps, live Linear tests |
| 7 | Release 0.4.0 | README (Linear setup), the main spec's milestone table (Views and bulk to 0.6, Linear at 0.4 and 0.5), smoke checklist, coverage, tag (maintainer approves first) |

## 13. Changes to the main spec

When 0.4 ships, the main spec (`2026-09-30-project-boards-design.md`) is amended:

- Section 1 and 2: the plugin covers GitHub Projects and Linear.
- Section 4: the module table gains `linear`; the boundary rule covers both schemas.
- Section 7, Authentication: Linear credentials.
- Section 7, Network: the plugin also talks to `api.linear.app` when a Linear key is configured.
- Section 8: `linear_api_key_command`.
- Section 11: milestones become 0.4 Linear viewer, 0.5 Linear editing, 0.6 Views and bulk, then 1.0.
