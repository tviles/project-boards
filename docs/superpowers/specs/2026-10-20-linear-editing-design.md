# project-boards: editing Linear boards (0.5)

**Status:** Shipped in 0.5.0
**Date:** 2026-10-20
**Builds on:** `2026-10-15-linear-adapter-design.md` (the Linear spec) and `2026-09-30-project-boards-design.md` (the main spec). The Linear spec's sections 3–8 and 10 still hold; this document answers its section 11 and says only what changes. The main spec still governs GitHub boards, whose behaviour 0.5 leaves unchanged.
**Facts:** `docs/phase0-findings.md`, section "Linear" (L1–L10, the seed observations, the plan 2 recording). Linear's API facts below were read from the pinned `crates/queries/graphql/linear/schema.graphql`. Anything still unmeasured is a probe in section 12.
**How 0.4 was built:** `docs/superpowers/plans/2026-10-19-linear-client-and-viewer.md` (the `Backend` enum, `Backends` per provider, `EditContext`'s provider read-only reason, `ListQuery`, polled removals, the fixed Linear field ids).

## 1. Purpose

Make Linear boards editable from herdr, with the same keys, popovers and optimistic edits that GitHub boards have had since 0.2 and 0.3. A herdr user whose team plans in Linear should be able to work their board through a day without opening linear.app.

### Success criteria

From herdr alone, on a Linear project or team board, a user can:

1. Move a card between workflow states, including closing it as completed or canceled and reopening it, and reorder it in a manually ordered view.
2. Set and clear priority, estimate, due date, cycle, the value of a single-select label group, and (on a team board) the project.
3. Assign or unassign the issue, change its labels, edit its title and description, and comment on it.
4. Create an issue in a column, which lands with that column's and swimlane's values, and add an existing issue to a project board by its key or URL.
5. Archive and unarchive an issue, and remove an issue from a project board.
6. Retry a failed or rate-limited create or comment without ever making a second issue or posting a comment twice.

And a GitHub user notices no difference.

## 2. Scope

### In scope

- Every operation in section 5 on Linear boards, through the existing keys (`<` `>`, `J` `K`, `e`, `a`, `t`, `c`, `X`, `n`, `A`, `Space`, the title and body forms, the error log and `R`).
- Seams this needs: capabilities per provider, the recent window and in-flight limit per provider, and wording that names the board's provider.
- Four 0.4 leftovers (section 11).
- Three new probes against the testbed (section 12).

### Out of scope for 0.5

- Bulk edits, saving and creating views, local group-by and sort, mouse drag: still 0.6.
- Changing an issue's team. A move or a value that would need it is refused (section 6).
- Deleting or trashing issues. Archive is the way to put one away.
- Marking an issue as a duplicate (it needs a duplicate-of relation). Moves into a state of kind `duplicate` are refused.
- Editing parent issues, sub-issues and relations (read-only, as in the main spec).
- Replying to, editing or deleting comments.
- Searching Linear for an existing issue to add. Adding works by pasted key or URL only, because `searchIssues` is limited to 30 requests a minute (L9, docs).
- Creating labels, cycles or projects; project milestones; triage actions beyond moving a card into or out of the Triage column (D10).
- Checking an estimate against the team's estimation scale before sending it (Linear refuses invalid values; the refusal shows as for any failed edit).
- OAuth and several workspaces, as in the Linear spec.

## 3. Decisions

### Decided by Tyler (2026-10-20)

| # | Question | Decision |
|---|---|---|
| T1 | A move into a merged column whose name the issue's team has no state for | **Refused with a reason**, for example `TBB has no "In Review" state`. Nothing is sent; the card stays. No fallback to another state of the same kind. |
| T2 | Which team owns a new issue on a project board | **The column's team** when the column belongs to one team; **otherwise the new-issue form asks**, with a team line prefilled with the team last chosen in this session. |
| T3 | Which 0.4 leftovers go into 0.5 | The Board tab's empty Canceled and Duplicate columns; `is:` typos that do not flash; the `@` prefix on Linear names; the `Polled` naming and the `model::caps` → `backend` layering. The picker's 100 cap and the other leftovers stay deferred (section 11). |
| T4 | Which new probes run | A duplicate create id, `sortOrder` behaviour, and a refused (FORBIDDEN) write. Each is run only after Tyler says yes. The revoked-key case stays unmeasured. |
| T5 | Adding an existing issue that is already in another project (an issue has at most one project, so adding moves it) | **Confirm first:** `TBA-12 is in "<other project>"; move it here? (y/n)`. Nothing changes another project without a yes. |
| T6 | Comments and retries | **Comments carry a client UUID too**, like creates, so a comment whose answer was lost can be resent with `R` without posting twice. Probe L11 also tries a duplicate comment id. |

### Settled in design (Claude's calls; flag any you disagree with)

| # | Area | Decision |
|---|---|---|
| D1 | `Op` and `ContentId` | No new content-id type. A Linear issue's `ItemId` and `content_id` are both its UUID (0.4), and `Item::content_id()` already gives `ContentId::Issue(uuid)` for Linear issues. Linear never builds `ContentId::PullRequest` or `ContentId::Draft`; capabilities (section 4) make sure no op that needs one is offered. The `Op` variants stay; a few gain the data a Linear send needs (section 5). |
| D2 | Capabilities | A capability set per provider lives in `model` (section 4). GitHub's set is exactly today's matrix. `check` asks the board's provider set before anything else. |
| D3 | Team-owned values | T1 applies to every value a team owns, not only states: workflow states, cycles and labels of team-level label groups. A value is only ever set on an issue of the team that owns it (section 6). |
| D4 | Close and reopen | Status changes. Close as completed goes to the team's first `completed` state, close as canceled to its first `canceled` state, reopen to its first `unstarted` state (then `backlog`), "first" meaning lowest position (section 7). |
| D5 | Reordering | Writes `sortOrder`, as the midpoint of the two neighbours it lands between. Only manually ordered views reorder, as on GitHub, where sorted views refuse reorder (section 8). `prioritySortOrder` is never written. |
| D6 | Creating | One `issueCreate` call carrying a client UUID and every preset. A retry sends the same UUID (section 9). |
| D7 | Recent window and in-flight limit | Both move into `SyncProfile`. GitHub keeps 5 minutes and 4. Linear gets 30 seconds and 4 (section 10). |
| D8 | Permissions | A resolved Linear board is editable (`viewer_can_update: true`); a refusal fails only that edit, with Linear's message. No blanket block, because Linear permissions are per team (section 10). |
| D9 | Add existing | On project boards only, by pasted key (`TBA-12`) or `linear.app` issue URL, looked up with `issue(id:)` (L9). The column's and swimlane's presets apply, as on GitHub, resolved for the found issue's team (section 9). |
| D10 | Triage | The Triage column is a move target like any other: Linear's app allows moving an issue into triage. Only `duplicate` states are refused (section 6). |
| D11 | Test cleanup | Live tests and probes put their throwaway issues in the trash (`issueDelete`), not the archive, so the Free plan's 250-issue cap is not filled by repeated runs. Only the test tooling deletes; the app never does. |

### Still holding from the Linear spec

Section 3 of the Linear spec holds unchanged: board kinds, merged columns by name, the `Backend` enum with no `dyn Provider`, fixed field ids, `BoardKey`, credentials, filters evaluated on the client, `SyncProfile` per provider, `BackendError`, and testing against the testbed only. The boundary rule holds: generated Linear types never leave `src/linear/`, `crates/queries` and `ops`.

## 4. Capabilities per provider

`model::caps::check(kind, item, ctx)` stays the single source of truth. What changes is what decides a provider's answer: instead of `read_only_reason(provider)` refusing everything on Linear, each provider has a capability set in `model`, and `backend` reads it rather than `caps` importing `backend` (this is also leftover T3's layering fix).

The set answers, per provider:

| Capability | GitHub | Linear |
|---|---|---|
| Drafts (new draft, draft edits, convert) | yes | no |
| Milestones | yes | no |
| Close reasons | completed, not planned | completed, canceled (each a state, section 7) |
| Assignees | several | at most one |
| Remove from the board | every board | project boards only (it clears the issue's project) |
| Add existing | by search or URL | project boards only, by key or URL |
| Scope checks (`project`, `repo`) | yes | no |

The matrix for a Linear issue that is not archived:

| Operation | Linear issue |
|---|---|
| Move, set field, archive | yes |
| Reorder | yes, in manually ordered views (the existing sorted-view refusal) |
| Title, body, comment, labels | yes |
| Assignees | yes, one at most |
| Close | when its state kind is triage, backlog, unstarted or started (`ContentState::Open`, as 0.4 maps them) |
| Reopen | when its state kind is completed, canceled or duplicate (`Closed`) |
| Remove | project boards only |
| Milestone, convert | no, with a reason (section 13) |

Archived Linear issues allow only unarchive (and remove, on a project board) and read, as on GitHub. Placeholders (`local:` ids) refuse edits with the `Creating` reason until they are created, as on GitHub. A Linear item cached before 0.5 without the ids an edit needs gets `NeedsRefresh`, as on GitHub.

The `n` menu on a Linear board offers "New issue", and on a project board also "Add existing issue (key or URL)". It never offers a draft.

`EditContext` on a Linear board: `viewer_can_update` is true once the board resolves; `scopes` is `None` and `refused` stays empty, so the scope checks pass. The 0.4 reason "Editing Linear boards arrives in 0.5" is removed.

## 5. Operations on Linear

Every issue edit except comments, archiving and creation is one `issueUpdate(id, input)`. Every mutation is a named operation in `crates/queries/graphql/linear/linear.graphql`. Issue mutations select the same issue fields as `BoardIssues`, so the answer becomes `Confirmed::Item` and enters the confirmed board, as GitHub's field edits do.

| Op | Linear mutation | Notes |
|---|---|---|
| `SetField` / `ClearField` on Status | `issueUpdate { stateId }` | The issue's team's state (section 6). No clear: every issue has a state, and moves already stop before the "No Status" column. |
| on Priority | `{ priority: 1–4 }`; clear `{ priority: 0 }` | Integers on write (Linear spec section 9). |
| on Estimate | `{ estimate: n }`; clear `{ estimate: null }` | Whole numbers only; a fraction is refused before sending (section 13). |
| on Due date | `{ dueDate: "YYYY-MM-DD" }`; clear `null` | A date with no time zone, never converted through UTC. |
| on Cycle | `{ cycleId }`; clear `null` | Only the issue's team's cycles (section 6). |
| on a label-group field (`linear:group:<id>`) | `{ addedLabelIds: [new], removedLabelIds: [old] }`; clear `{ removedLabelIds: [old] }` | The old label is removed explicitly; the design never relies on Linear swapping a group's label by itself. The op carries the label it replaces, because `send` does not read the board. |
| on Project (team boards) | `{ projectId }`; clear `null` | |
| `Reorder` | `{ sortOrder }` | The op carries the new value (section 8). |
| `Assignees` | `{ assigneeId }` or `{ assigneeId: null }` | The one assignee left in `all`. |
| `Labels` | `{ addedLabelIds, removedLabelIds }` | |
| `Title`, `Body` | `{ title }`, `{ description }` | The description is Markdown, as Linear stores it. |
| `Comment` | `commentCreate { id, issueId, body }` | Top-level comments only. `id` is a client UUID made with the op (T6); a resend carries the same one. |
| `Close`, `Reopen` | `{ stateId }` | Built as a status change (section 7). |
| `Archive`, `Unarchive` | `issueArchive`, `issueUnarchive` | |
| `Remove` | `{ projectId: null }` | Project boards only; it asks for confirmation as on GitHub. The issue leaves the board. |
| `Create` (new issue) | `issueCreate { id, teamId, title, description, …presets }` | Section 9. |
| `Create` (existing) | `issueUpdate { projectId, …presets }` | The issue found by `issue(id: <key or id>)`; presets resolved for its team (section 9). |

`ops::send::send` gains a `Backend::Linear` arm that calls the Linear client's own methods; `Op::apply` stays provider-neutral (it changes the board snapshot the same way whichever provider receives the op).

## 6. Team-owned values and merged columns

A Linear board can span several teams, and some values belong to one team: workflow states, cycles, and the labels of a team-level label group. Linear refuses such a value on another team's issue. The rule, from T1 and D3:

- A value owned by a team is only set on that team's issues.
- **Moving into a merged column** (a column whose options come from several teams, merged by name): the move sends the issue's own team's state of that name. If the team has none, the move is refused: `TBB has no "In Review" state`.
- **Cycle columns and the cycle picker:** the picker offers only the issue's team's cycles. A move into another team's cycle column is refused: `TBB issues can't be in TBA's cycles`.
- **Label-group fields:** a field of a team-level group is settable only on that team's issues; others are refused: `"Type" belongs to team TBA`. Workspace-level groups work on every issue.
- **States of kind `duplicate`** are never a move target: `Mark duplicates in Linear`.

To apply the rule, the board must know which team owns each state, cycle and label group, and each issue's team id (0.4 keeps the team key in `ContentRef.container`). These are additive model fields with serde defaults, so GitHub caches stay valid and need no version change. A Linear board painted from a 0.4 cache lacks them; team-owned edits on it get `NeedsRefresh` until the board's first resolve replaces the cache.

## 7. Close and reopen

On Linear, `X` is a status change, not a separate concept:

- The close menu offers **Close as completed** and **Close as canceled**, each naming the state it will move to, for example `Close as completed (Done)`.
- Close as completed goes to the issue's team's `completed` state with the lowest position; close as canceled to its `canceled` state with the lowest position. If the team has none of that kind, that entry is refused with a reason.
- Reopen goes to the team's `unstarted` state with the lowest position; with none, its `backlog` state with the lowest position; with neither, it is refused.
- The op sent is a status change (`SetField` on `linear:status`), so the card moves to its new column at once, exactly as if it had been dragged there.
- Closing asks for confirmation, as on GitHub (main spec section 6).

`CloseReason::NotPlanned` reads "canceled" on Linear boards. GitHub's wording is unchanged.

## 8. Reordering

Linear keeps one manual order per issue, `sortOrder` (a float), which every manually ordered view uses. 0.4 already shows manual views in `sortOrder` order (plan 2 Decision 13).

- `J` and `K` move the issue past its neighbour in the list as shown (inside its column and swimlane on a board).
- The new `sortOrder` is the midpoint of the `sortOrder`s of the two issues it now sits between. At the top it is the first issue's value minus 1; at the bottom, the last one's plus 1.
- The op carries the new value, so `apply` updates the issue's `sort_order` and the card moves at once; the confirmed issue from `issueUpdate` replaces it.
- If the two neighbours have the same `sortOrder`, or either has none, the reorder is refused with a reason that suggests `r`. Probe L12 says whether this can happen in practice.
- Views sorted by a field (priority, dates) refuse reorder, as GitHub's sorted views do. So `prioritySortOrder` and `subIssueSortOrder` are never written.

## 9. Creating and adding issues

### New issues

- **The team** (T2): on a team board, the team. On a project board in the board layout, the column's team when every option behind the column belongs to one team. Otherwise (a merged column, a column field no team owns such as Priority, or the table layout) the new-issue form has a **Team** line, prefilled with the team chosen last in this session (else the board's first team); `Tab` reaches it and `Enter` opens a picker of the board's teams. The form's heading names the team: `New issue in TBA`.
- **Presets:** the column's and swimlane's values go into the same `issueCreate` (state, priority, labels, cycle, assignee, estimate, due date), resolved for the chosen team by section 6. On a project board, `projectId` is always the board's project. The team picker offers only teams for which every preset resolves; if none does, the create is refused with the reason. One call means no "refused presets" report: the issue is created with all its values or not at all.
- **The client UUID** (D6): a UUID v4 is generated when the op is made and stored in the op. The queue's rate-limit resend and the error log's `R` send the same op, so the same id. If Linear answers that an issue with that id already exists (probe L11 records how), the client loads `issue(id:)` and treats the create as succeeded. Linear boards never show GitHub's "may have been created" text, and never need `Confirmed::NotAdded`.
- **The placeholder** behaves as on GitHub: a `local:` item shown at once, swapped for the created issue when it is confirmed.

### Existing issues (project boards)

- "Add existing issue" asks for a key (`TBA-12`) or a `https://linear.app/…/issue/…` URL, looks it up with `issue(id:)` (L9), reading its team and current project, and sends `issueUpdate { projectId: <board's project>, …presets }`.
- **Presets** (D9): pressing `n` in a column or swimlane applies its values, as on GitHub, resolved for the found issue's own team by section 6. A preset that team cannot take is left out, and the toast names it (`added TBA-12; TBA has no "In Review" state`), as GitHub reports refused presets. The add itself still happens.
- **Already in another project** (T5): the add asks `TBA-12 is in "<other project>"; move it here? (y/n)` before sending anything. `y` sends; any other key cancels.
- An issue already on the board says so and sends nothing. An unknown key says `No issue TBA-12`. An issue whose team is not one of the project's teams is sent anyway, and Linear's answer decides.

## 10. Sync, limits and errors

### The recent window and in-flight limit (D7)

`SyncProfile` gains `recent_window: Duration` and `max_in_flight: usize`. The store and the op queue are built with the board's provider profile instead of reading `RECENT_WINDOW` and `MAX_IN_FLIGHT`.

| | GitHub (unchanged) | Linear |
|---|---|---|
| Recent window | 5 minutes | 30 seconds |
| Edits in flight | 4 | 4 |

The window has to outlast a poll that was already on its way when an edit was confirmed. L1 measured Linear at under half a second from write to read, so the window only covers that race; 30 seconds covers a multi-page poll with room to spare, and lets another person's later change to the same issue show within one poll interval. Four in flight stays: each edit is one request, well inside 2,500 requests an hour.

### Errors on edits

- `FORBIDDEN` (probe L13) becomes `MissingPermission { provider: Linear, needed: [] }`. The edit fails and is undone, and the toast shows Linear's message. Later edits are still tried: Linear permissions are per team, so one refusal says nothing about another team's issues.
- `AUTHENTICATION_ERROR` mid-session becomes `Unauthorized`; each edit fails with the Linear remedy text.
- `RATELIMITED` (HTTP 400) becomes `RateLimited`, with the wait from `x-ratelimit-*-reset` (milliseconds, plan 2 recording). The queue pauses writes with the header countdown, as for GitHub's secondary limit. A create resent after the pause carries the same UUID.
- `INPUT_ERROR` (an invalid estimate, a state of another team that slipped through) fails the edit with Linear's message.
- Log lines name the error variant and the operation only, as today.

## 11. 0.4 leftovers

Folded in (T3):

- **The Board tab's empty Canceled and Duplicate columns:** on a Linear board, a built-in tab whose filter excludes state kinds hides those kinds' columns while they are empty. A column holding items still shows, as the existing rule says.
- **`is:` typos:** on Linear boards (where `f` is evaluated on the client), an `is:` value other than `open`, `closed`, `issue` or `archived` matches nothing and flashes `unknown filter value: is:closd`, the same treatment unknown keys get (plan 2 Decision 5). GitHub boards send `f` to GitHub and are unchanged.
- **The `@` prefix:** tables on Linear boards show display names without `@`. GitHub logins keep it.
- **Naming and layering:** `linear::convert::Polled` gets a name that no longer clashes with `StoreUpdate::Polled`, and `model::caps` stops importing `backend` (section 4).

Deferred again, each named so it is not lost: the picker's 100 cap on projects and teams; the unmeasured `dateUpdated` view ordering; a deleted Linear view reported as `INPUT_ERROR` getting generic text; same-name label groups across teams not being merged; the `labelGroup` grouping falling back to the board's only single-select group.

## 12. New probes

Each is a step in `examples/probe_linear.rs`, run against the testbed only and only after Tyler approves the run, behind the existing workspace guard. Results go into `docs/phase0-findings.md` under "Linear", continuing the numbering. They run before the slices that depend on them (section 15).

| # | Question | Depends on it | Fallback if the answer is bad |
|---|---|---|---|
| L11 | What does a second `issueCreate` with the same client `id` return (HTTP status, `extensions.code`, message), and does a second issue appear? The same for a second `commentCreate` with the same `id` (T6). | Safe create and comment retries (sections 5 and 9) | Before resending a create or comment that failed, look the id up (`issue(id:)`, `comment(id:)`) and treat a hit as done |
| L12 | After `issueUpdate { sortOrder }`, do `customView.issues` and the web app's manual order follow it? Does Linear keep the float exactly or renumber neighbours? Does it touch `prioritySortOrder`? | Reorder (section 8) | Midpoints as designed; equal neighbours refused |
| L13 | What does a refused write return? Produced with a second testbed key limited to read access (or one team), which Tyler creates as `PB_LINEAR_READONLY_KEY` | `MissingPermission` mapping (section 10) | Map `FORBIDDEN` from Linear's docs, unconfirmed, as 0.4 does |

L13 needs Tyler to create the restricted key in the testbed workspace first. Probes that write create their own throwaway issues and trash them afterwards (`issueDelete`), even on failure (D11).

## 13. Wording

- Texts on the edit path that name GitHub as the server name the board's provider on Linear boards: "Linear returned an error", "Creating in Linear…", "Still being created in Linear; try again in a moment". On GitHub boards every text stays exactly as it is. `MAYBE_CREATED` and `MAYBE_POSTED` are reachable on GitHub boards only: Linear creates and comments carry client ids, so they are always safe to resend.
- New reasons on Linear boards (final wording is the builder's, kept to one line):
  - milestone: `Linear issues have no milestones`
  - convert: not offered (no drafts)
  - remove on a team board: `Every issue of a team is on its team board; archive it instead (A)`
  - estimate with a fraction: `Linear estimates are whole numbers`
  - the team-owned refusals in section 6, and the close and reopen refusals in section 7
- The bottom line and the `Space` menu list only what the item allows, as today, so Linear boards show their own set.
- The `a` popover on a Linear board is the same checklist with at most one person ticked: ticking someone unticks the other. Its heading says `Assignee`.

### Candidates for assignees and labels

- **Assignees:** the issue's team's members, loaded the first time they are needed and cached per team; `r` forgets them, as GitHub's per-repository candidates. A list cut by its page limit is marked partial, as on GitHub.
- **Labels:** the issue's team's labels and the workspace's labels, leaving out the labels of single-select groups (edited as fields, section 5) and the group labels themselves. Labels of multi-select groups are included where the workspace allows such groups.

## 14. Testing

Test-driven, with at least 80% line coverage at the release.

- **Units:** the capability matrix for every provider, item state and board kind; the team-ownership rule (merged column with and without the team's state, cycles, label groups, duplicate states); close and reopen targets; midpoint `sortOrder`, ends and ties; the recent window per provider (GitHub's existing window tests unchanged); new-issue team choice and preset resolution.
- **Send:** each Linear op's mutation and variables, through `FixtureTransport`; the create retry sends the same id; a duplicate-id answer (in the shape L11 records) becomes a success after the lookup.
- **Recorded fixtures:** each Linear mutation recorded from the testbed with `PB_RECORD=1` and scrubbed, replayed in plain `cargo test`. Hand-written fixtures, marked as such, cover `FORBIDDEN` if L13 cannot produce it, and `RATELIMITED`.
- **UI:** snapshot tests of the close menu, the assignee popover with one tick, the new-issue form with a team line, on Linear boards; GitHub snapshots unchanged.
- **Live:** `tests/live_linear.rs` exercises every operation in section 5 on throwaway issues in the testbed and trashes them (`issueDelete`) even when an assertion fails (D11). It uses `PB_LINEAR_TESTBED_KEY` or the Keychain item, never `LINEAR_API_KEY`, and checks the workspace URL key before any other request.
- **GitHub:** every existing test passes unchanged at every slice, apart from renames named in the plan.
- **No real data**, as in the Linear spec.

## 15. Delivery

Short branches off `main`, stacked where useful, each a PR into `main` that Tyler opens and merges with `--merge`. GitHub behaviour is unchanged at every merge and the full check passes on each PR. `main` stays `0.5.0-dev` until the release. A suggested order (the plan decides the final slicing):

| # | Slice | Contents |
|---|---|---|
| 1 | Seams | Capability sets per provider in `model` (and the layering fix); `SyncProfile` window and in-flight limit; provider wording; the `Polled` rename. Linear stays read-only at the end of this slice. |
| P | Probes | L11–L13, each after Tyler's yes; findings recorded. Before slices 3 and 5. |
| 2 | Linear writes | Mutations in `linear.graphql`; the `Backend::Linear` send arm; error mapping for edits; team ownership in the model. |
| 3 | Moves and fields | Moves with the team rule, close and reopen, field edits, reorder. |
| 4 | Content edits | Title, description, comment, assignee, labels with team candidates; archive, unarchive, remove. |
| 5 | Creating | New issues with team choice and presets; add existing on project boards. |
| 6 | Leftovers | Section 11. |
| 7 | Release 0.5.0 | README (Linear editing), CHANGELOG, the main spec's 0.5 milestone row and the Linear spec's section 11 pointing here, smoke checklist, coverage, tag after Tyler approves. |

## 16. Left to the builder

- The exact shape of the model additions for team ownership, and of the extra data `Reorder`, the label-group `SetField` and `Create` carry for Linear.
- How the UUID v4 is generated. No new dependency is needed (both supported platforms have `/dev/urandom`); adding a small crate is acceptable if the plan says why.
- The final wording of the new one-line reasons, within section 13's meaning.
- Page sizes for team members and labels, within L2's cap of 250.
- Whether archive and unarchive confirm with `Confirmed::Item` or `Confirmed::Applied`.
