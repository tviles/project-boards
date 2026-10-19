# Phase 0 findings (2026-10-08)

| # | Question | Answer | Consequence |
|---|---|---|---|
| 0 | Index lag after a write | A new draft stayed invisible for more than 73 s in all three item-list forms (`query: ""`, no `query`, `orderBy` POSITION). This is a lower bound, measured once, by adding a draft with our own token | Changes made by others appear a minute or more late. Input to the 0.2 plan: mutation responses must update the confirmed layer directly, and confirmed edits must outrank stale poll results for several minutes. 0.1 is read-only: no code change |
| 1 | `updated:>=` in `items(query:)` | `updated:>=<datetime>` returns 0 (solid). `updated:>=<date>` returns items (all 13), but this is not yet shown to exclude older ones: every item was probably updated on the pivot day, so it cannot be told apart from the date filter being ignored | INCREMENTAL_MODE = Date (decision kept). To be re-verified once the testbed has items updated on different days, e.g. at Task 10 fixture recording |
| 2 | Web filter syntax parity | Every check matches except `is:issue` (server 12, client 10). Inferred, not tested: it also counts draft issues (10 issues + 2 drafts = 12) | Documented; evaluated client-side in a later milestone |
| 3 | View filter round trip; group-by/sort writable | Filter write persists and reads back exactly across runs: the Probe view already held `label:bug -status:Done` at the start of run 2 (written by run 1) and run 2's views query read it back exactly. Within run 2 the write was idempotent, so its response comparison alone proves little. `UpdateProjectV2ViewInput` has no group field (awk check printed `0`); group-by/sort read-back was empty but inconclusive, because the manual view settings had not been applied | Local overrides as designed. Re-check read-back when fixtures are recorded in Task 10, after the settings are applied |
| 4 | Mouse drag reaches a tab pane | Yes (tab log: 11 Down, 129 Drag, 11 Up). Not exercised in the overlay (no clicks there) | Drag in 0.4 as designed |
| 5 | Focus events reach plugin panes | Yes (FocusGained/FocusLost in both logs) | FOCUS_EVENTS_EXPECTED = yes |
| 6 | Esc in overlay | Reaches the plugin (4 Esc key events in the overlay log). The overlay looked like a second tab to Tyler | Documented in README, but the overlay placement semantics are not independently confirmed: Tyler saw the overlay as "a second tab", and the overlay log's Resize(144, 46) is 2 cells smaller each way than the tab log's Resize(146, 48), which suggests it was a real overlay. Re-check Esc in the overlay during 0.1 manual testing |
| 7 | Oldest herdr with everything used | 0.7.0 (docs and source reading, not run on 0.7.0). Ruling: keep `min_herdr_version = "0.9.0"` (focus-event fix landed in 0.9.1) | manifest unchanged |
| 8 | Pane closes when its process exits | Left open: after q the screen cleared and the pane stayed | The pane must close itself (Task 25 as planned); README wording |

## herdr version evidence (probe 7)

Repo moved to https://github.com/herdrdev/herdr. v0.6.10 has no `src/cli/plugin.rs` (404). v0.7.0 notes (https://github.com/herdrdev/herdr/releases/tag/v0.7.0): "Added local plugin v1 support ... managed plugin panes ..." and "plugin pane placement, plugin invocation context/env injection". v0.7.0 `src/cli/plugin.rs` parses placements overlay, split, tab, zoomed and has open/focus/close; `src/cli/pane.rs` has `pane get <pane_id>`; docs list `HERDR_PLUGIN_CONTEXT_JSON` and "any available `HERDR_WORKSPACE_ID`, `HERDR_TAB_ID`, and `HERDR_PANE_ID`". Argument-less `pane get` resolving the calling pane arrived in 0.8.2 (#2297, #2298); manual pane navigation emits focus events again from 0.9.1.

## Terminal probe evidence (herdr 0.9.3)

| Log | Mouse events | Down / Drag / Up | FocusGained / FocusLost | Esc |
|---|---|---|---|---|
| probe-tab.log | 264 | 11 / 129 / 11 | 5 / 5 | 0 (not pressed) |
| probe-overlay.log | 368 | 0 / 0 / 0 | 5 / 2 | 4 |

Tyler's observations (probe-observations.md): the overlay opened looking like a second tab; Esc registered as a keypress; after q the pane stayed open (screen cleared, nothing closed).

## API probes (2026-10-06 run 2)

### 0. Index lag

- t+0s [0 query:""]: 13 items, new id present: false
- t+0s [0b no query]: 13 items, new id present: false
- t+0s [0b no query, orderBy POSITION]: 13 items, new id present: false
- t+6s [0 query:""]: 13 items, new id present: false
- t+6s [0b no query]: 13 items, new id present: false
- t+6s [0b no query, orderBy POSITION]: 13 items, new id present: false
- t+12s [0 query:""]: 13 items, new id present: false
- t+12s [0b no query]: 13 items, new id present: false
- t+12s [0b no query, orderBy POSITION]: 13 items, new id present: false
- t+18s [0 query:""]: 13 items, new id present: false
- t+18s [0b no query]: 13 items, new id present: false
- t+18s [0b no query, orderBy POSITION]: 13 items, new id present: false
- t+24s [0 query:""]: 13 items, new id present: false
- t+24s [0b no query]: 13 items, new id present: false
- t+24s [0b no query, orderBy POSITION]: 13 items, new id present: false
- t+30s [0 query:""]: 13 items, new id present: false
- t+30s [0b no query]: 13 items, new id present: false
- t+30s [0b no query, orderBy POSITION]: 13 items, new id present: false
- t+36s [0 query:""]: 13 items, new id present: false
- t+36s [0b no query]: 13 items, new id present: false
- t+36s [0b no query, orderBy POSITION]: 13 items, new id present: false
- t+42s [0 query:""]: 13 items, new id present: false
- t+42s [0b no query]: 13 items, new id present: false
- t+42s [0b no query, orderBy POSITION]: 13 items, new id present: false
- t+48s [0 query:""]: 13 items, new id present: false
- t+48s [0b no query]: 13 items, new id present: false
- t+48s [0b no query, orderBy POSITION]: 13 items, new id present: false
- t+55s [0 query:""]: 13 items, new id present: false
- t+55s [0b no query]: 13 items, new id present: false
- t+55s [0b no query, orderBy POSITION]: 13 items, new id present: false
- t+61s [0 query:""]: 13 items, new id present: false
- t+61s [0b no query]: 13 items, new id present: false
- t+61s [0b no query, orderBy POSITION]: 13 items, new id present: false
- t+67s [0 query:""]: 13 items, new id present: false
- t+67s [0b no query]: 13 items, new id present: false
- t+67s [0b no query, orderBy POSITION]: 13 items, new id present: false
- t+73s [0 query:""]: 13 items, new id present: false
- t+73s [0b no query]: 13 items, new id present: false
- t+73s [0b no query, orderBy POSITION]: 13 items, new id present: false

- [0 query:""] NOT visible within 60s
- [0b no query] NOT visible within 60s
- [0b no query, orderBy POSITION] NOT visible within 60s

Probe item deleted.

All items: 13

### 1. Incremental filter

Pivot 2026-10-06T16:12:06Z; items updated at or after it: 7

- `updated:>=2026-10-06T16:12:06Z` → 0 items (datetime exact: false)
- `updated:>=2026-10-06` → 13 items (datetime exact: false)

### 2. Filter syntax

- `label:bug` → server 1 / expected 1 ✓
- `-label:bug` → server 12 / expected 12 ✓
- `label:bug,docs` → server 2 / expected 2 ✓
- `status:Todo` → server 8 / expected 8 ✓
- `status:"In Progress"` → server 4 / expected 4 ✓
- `assignee:@me` → server 2 / expected 2 ✓
- `no:assignee` → server 11 / expected 11 ✓
- `is:issue` → server 12 / expected 10 ✗
- `is:draft` → server 2 / expected 2 ✓

### 3. Views

- "View 1" ("TABLE_LAYOUT"): filter Null, groupBy [], columnBy [], sortBy []
- "Board" ("BOARD_LAYOUT"): filter Null, groupBy [], columnBy [{"name":"Status"}], sortBy []
- "Bugs" ("TABLE_LAYOUT"): filter String("label:bug"), groupBy [], columnBy [], sortBy []
- "Probe" ("TABLE_LAYOUT"): filter String("label:bug -status:Done"), groupBy [], columnBy [], sortBy []

Filter write returned String("label:bug -status:Done") (round trip exact: true)

Group-by and sort are not in UpdateProjectV2ViewInput in schema 0.1.0-dev; confirm by checking the schema copy for `groupBy` inside that input.

## Linear (phase 0 for the Linear backend)

Schema: `crates/queries/graphql/linear/schema.graphql`, fetched 2026-10-16 from linear/linear master (53325 lines). Probes ran against the testbed workspace only (`tests/linear-testbed.toml`); nothing here comes from any other workspace.

| # | Question | Answer | Consequence |
|---|---|---|---|
| L1 | How soon after an edit does a poll or a read show it? | 0.2 s (updatedAt-filtered list with `includeArchived`), 0.4 s (single issue by id), 0.5 s (custom view issues): every path under 0.5 s on a small team, so Linear is effectively read-after-write consistent | Linear's recent window (0.5) can be short; polls can be trusted right after an edit. Plan 2 keeps a small window (a few seconds) as a margin, not a multi-minute one |
| L2 | What does the full issue page cost, and what is the page size cap? | `X-Complexity` 18 for `first: 100` and for `first: 250`, with 19 issues returned each time; `first: 1000` is rejected with `INVALID_INPUT` (the cap lies between 250 and 1000). The reported complexity appears to follow the items returned rather than `first`; measured on a small team (19 issues), so it says little about a big board. Complexity remaining fell by about 18 per request from 3,000,000 | `SyncProfile.full_page` for Linear stays 100. Plan 2 re-measures cost on a larger board if needed, and never sends `first` above 250 |
| L3 | Which strings do view settings use? | `layout`: `board`, `list`. `issueGrouping`: `workflowState`, `priority`, `assignee`, `none`. `issueSubGrouping`: `none`, `workflowState`. `viewOrdering`: `priority`, `manual`. `viewOrderingDirection`: `asc` or null (null when the ordering is not manual). `showCompletedIssues`: `all`, `none`. Settings are present for every view, workspace, team and project scoped | Plan 2 maps `layout` board/list to board/list; `issueGrouping` workflowState/priority/assignee/none to column or group keys; `issueSubGrouping` the same set; `viewOrdering` priority/manual to the sort. Unknown strings fall back to the list layout with no grouping |
| L4 | Can custom view tabs ship, and are project-scoped views reachable? | `customViews` lists workspace and team views (6 here) but not "Probe project view", which lives inside the project. It is reachable through `Project.facets[].targetCustomView` (id, name, settings) and `customView(id).issues` returned its 17 issues | Custom view tabs ship for workspace and team views from `customViews`. Project-scoped views are reachable only through `Project.facets`; plan 2 reads them there when a project board is open |
| L5 | Does a view's "hide completed" setting reach the issue list? | No. "Probe hide completed" has `showCompletedIssues: none`, yet `customView.issues` returned a completed issue (1 of 11, the same count as TBB's completed issues) | The client hides completed issues itself for views whose `showCompletedIssues` is `none` |
| L6 | How do errors look? | Bad key: HTTP 401, `AUTHENTICATION_ERROR`. Unknown issue (random UUID or `ZZZ-99999`): HTTP 200 with `INPUT_ERROR` "Entity not found: Issue". A refused request (no permission) was not produced; the revoked-key case was not run. `RATELIMITED` is from Linear's documentation (HTTP 400), not measured | `BackendError` mapping: 401 or `AUTHENTICATION_ERROR` becomes `Unauthorized`; HTTP 200 with `INPUT_ERROR` (entity not found) becomes `Api`; `RATELIMITED` becomes `RateLimited` (retry seconds from the rate limit headers); `FORBIDDEN` (docs, unconfirmed) becomes `MissingPermission`; transport failures `Network`; undecodable bodies `Decode` |
| L7 | Do polls see archivals and trash? | Yes. With `includeArchived: true`, a poll lists an archived issue (`archivedAt` set, `trashed` null) and a trashed issue (`trashed: true`, `archivedAt` set), both immediately and 10 s later | Polls see archivals and trash, so the periodic full refresh is only a safety net. The poll must pass `includeArchived: true` and treat `archivedAt` set or `trashed: true` as removal |
| L8 | Where do a project board's teams come from? | `Project.teams` (TBA, TBB) matched the team keys found on the project's issues (TBA, TBB) | A project board's teams come from `Project.teams` (one query), no scan of issues needed |
| L9 | Can `KEY-123` be looked up directly, and what limits search? | `issue(id: "TBA-1")` works with the identifier. `searchIssues` answered HTTP 200 with one result; no `X-RateLimit-Endpoint-*` header came back, so the documented 30 requests a minute limit is unconfirmed | Reference jumps look up `KEY-123` directly with `issue(id:)`. For 0.5, search is limited to the documented 30 a minute until a header confirms it, with one request per keystroke pause |
| L10 | Can `graphql_client` 0.16 build an operation with an `IssueFilter` variable from Linear's schema? | Yes, once the module declares the extra custom-scalar aliases the filter types reach: `TimelessDateOrDuration`, `Duration` and `JSONObject` (plus `JSON`) as `String`. Without them the first error is ``error[E0425]: cannot find type `Duration` in module `super` ``. No measurable build cost: `project-boards-queries` with `--features live` built in 6.9 s without the probe and 6.0 s with it (single timed builds, warm dependencies) | Plan 2 can send built-in tab filters as `IssueFilter` variables; it must add those scalar aliases to the `linear` module when it does. No need for raw requests with a validated twin |

### Seed observations

- Linear's API cannot create cycles (`cycleCreate` is deprecated: "Cycle creation is not supported"). Enabling cycles with `cycleEnabledStartDate` set to today made cycle 1 current immediately. Consequence: tests and the seed rely on enabling cycles; the client never creates one.
- This Free-plan workspace refuses multi-select label groups: `issueLabelCreate` with `groupType: multiSelect` returns `INPUT_ERROR` "multi-select issue label groups disabled"; single-select groups work. Consequence: the client must handle workspaces with and without multi-select groups; the testbed exercises single-select only.
- A new workspace starts with parentless labels "Bug", "Feature" and "Improvement"; `issueLabelCreate` with a null `color` returns `INPUT_ERROR` "invalid color". Consequence: the client always sends a colour when creating labels.
- `customViewCreate` works on the Free plan (two shared views were created through the API).

### Probe report (2026-10-19)

### L2: query cost of the issue page
| first | status | X-Complexity | complexity remaining | nodes | error codes |
|---|---|---|---|---|---|
| 100 | 200 | 18 | 2999956 | 19 | none |
| 250 | 200 | 18 | 2999938 | 19 | none |
| 1000 | 200 | 18 | 2999920 | 0 | INVALID_INPUT |
### L3/L4/L5: custom views
ProbeViews: HTTP 200, 6 views, error codes: none
| view | team | project-scoped | layout | issueGrouping | issueSubGrouping | viewOrdering | viewOrderingDirection | showCompletedIssues |
|---|---|---|---|---|---|---|---|---|
| Probe team board by status | TBB | no | board | workflowState | none | priority | null | none |
| Probe hide completed | TBB | no | list | none | none | priority | null | none |
| Probe board by priority | TBA | no | board | priority | none | manual | asc | all |
| Probe list by assignee | TBA | no | list | assignee | workflowState | priority | null | all |
| Testbed open | workspace | no | list | workflowState | none | priority | null | all |
| Testbed A bugs | TBA | no | list | workflowState | none | priority | null | all |
L4: "Probe project view" is NOT in the customViews list.
L4: it IS reachable through Project.facets (team: workspace; layout board, issueGrouping workflowState, issueSubGrouping none, viewOrdering priority, viewOrderingDirection null, showCompletedIssues all); ProbeViewIssues on it returns 17 issues.
L5: "Probe hide completed" returns 11 issues through ProbeViewIssues (first 100), of which 1 are completed; TBB has 11 issues in its first page, of which 1 are completed.
### L6: errors
- bad key, TestbedViewer: HTTP 401; codes: AUTHENTICATION_ERROR; first message: Authentication required, not authenticated
- random UUID, ProbeIssue: HTTP 200; codes: INPUT_ERROR; first message: Entity not found: Issue
- unknown identifier ZZZ-99999, ProbeIssue: HTTP 200; codes: INPUT_ERROR; first message: Entity not found: Issue
- revoked key: not run (PB_LINEAR_REVOKED_KEY is not set)
### L1: read-after-write delay
Set TBA-1 priority 0 -> 1: HTTP 200; codes: none; first message: -; updatedAt returned: true
| path | seen with the new priority after |
|---|---|
| a: updatedAt-filtered list (includeArchived) | 0.2 s |
| b: single issue by id | 0.4 s |
| c: "Probe board by priority" view issues | 0.5 s |
Restored TBA-1 priority to 0; confirmed by a read: 0
### L7: archive and trash in the polled list
issueArchive: HTTP 200, success: true, codes: none
issueDelete: HTTP 200, success: true, codes: none
| when | issue | listed | archivedAt set | trashed |
|---|---|---|---|---|
| immediately | archived one | yes | true | null |
| immediately | trashed one | yes | true | true |
| after 10 s | archived one | yes | true | null |
| after 10 s | trashed one | yes | true | true |
### L8: a project's teams
- project teams: TBA, TBB
- team keys on its issues (first 100, 17 issues): TBA, TBB
- they match: true
### L9: identifier lookup and search
- ProbeIssue("TBA-1"): works (HTTP 200, codes: none)
- ProbeSearch(<seeded title>): HTTP 200, 1 results, codes: none
- no header with `endpoint` in its name
