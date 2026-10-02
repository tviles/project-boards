# project-boards: a GitHub Projects board for herdr

**Status:** Design approved in brainstorming and refined by a grilling session (2026-10-01); pending written-spec review
**Date:** 2026-09-30
**Plugin id:** `tviles.project-boards`
**Binary:** `project-boards`
**Repository:** `github.com/tviles/project-boards` (public, MIT licence)

## 1. Purpose

A public herdr plugin that lets anyone view and run a GitHub Projects (v2) board from inside herdr, without opening github.com. It shows the board as a table or as a kanban board, the same way GitHub does, and supports the day-to-day management GitHub exposes: moving items between columns, setting any project field, assigning people, labelling, commenting, closing, adding and archiving items, and saving views.

Research on 2026-09-30 found no existing herdr plugin that renders a Projects board for a human to work in. The closest, `DnzzL/herdr-docket`, reads a Projects board only as a task queue for agents.

### Success criteria

From herdr alone, a user can:

1. Open a board linked to the repository they are working in, or pick any board they can access.
2. See it as a table or a kanban board, switch between the project's saved views, and filter it.
3. Open an item and read its body, comments, sub-issues, parent, issue type and linked pull requests.
4. Move an item between columns, reorder it, and set or clear any project field.
5. Change assignees, labels, milestone, title and body; comment; close or reopen.
6. Add an existing issue or pull request, create a new issue, create, edit or convert a draft, archive or unarchive an item, and remove it from the project.
7. Save a view's name, layout, filter and visible fields back to GitHub, and create new views.

None of these may require the browser.

### Audience

Public from day one, listed on the herdr marketplace through the `herdr-plugin` topic once 1.0 ships (section 11). The plugin must assume nothing about a board: field names, Status options, iteration setup, and whether the owner is a user or an organisation are all discovered at runtime.

## 2. Scope

### In scope for v1

- **Viewer:** table and board layouts, the project's saved views, server-side view filters, local quick search, item detail with rendered Markdown.
- **Item operations:** everything under `ProjectV2Item` listed in section 6.
- **Issue and pull request operations:** assignees, labels, milestone, title, body, comments, close and reopen.
- **View operations:** switch, change layout, filter, pick visible fields, save to GitHub, create.

### Out of scope for v1 (each is its own later spec)

- **Project administration:** creating, editing or deleting fields and options, project settings, collaborators, repository and team links, workflows, status updates, templates.
- **Editing sub-issues, parent issues and issue types.** They are shown read-only in the item detail.
- **Agent integration:** starting a coding agent from an item, or moving cards automatically as agents work. v1 leaves a seam for this (a CLI entry point, plugin actions, and `store` behind an interface so a later daemon can own it) but builds none of it.
- **A popup "quick peek".** Popup placement does not fit the single-instance model (section 4).
- **Syntax highlighting** in code blocks. A later cargo feature.
- **Windows.**
- **Classic (v1) projects.** They are deprecated by GitHub and not supported.
- **Offline editing.** Edits made without a network connection fail and are undone; they are never queued.

## 3. Decisions

| Area | Decision |
|---|---|
| Stack | Rust, ratatui, tokio, reqwest, `graphql_client` with a checked-in copy of GitHub's GraphQL schema, `pulldown-cmark` for Markdown |
| API access | Direct GraphQL over HTTPS with a token. The plugin never spawns `gh api` per call, because process start-up would cost 100–300 ms per request. |
| Item loading | One shared item set per board. Each view is a list of item IDs returned by a server-side filtered query. |
| Board choice | Boards linked to the focused pane's repository first; a picker over every board the user can access; the last choice remembered per repository |
| Instances | One pane per board. Opening a board that is already open focuses its pane. |
| Views | The project's saved GitHub views are the tabs. An explicit save writes name, layout, filter and visible fields to GitHub. Group-by and sort are stored locally per view and applied on top, because the API cannot write them; GitHub-side changes clear the local override. |
| Edits | Optimistic. What the user sees is the confirmed snapshot with pending edits replayed on top. |
| Polling | Focus-aware: fast while the pane is focused, slow while it is not, with idle detection as a fallback |
| Placement | Its own herdr tab by default; overlay, split and zoomed are also available |
| Input | Keyboard first with vim-style navigation and no Ctrl chords by default; mouse click, wheel and drag when herdr passes mouse events through |
| Distribution | Release binaries for macOS (arm64, x64) and Linux (x64, arm64), pinned to the manifest version and checksum-verified, with `cargo build --release` as the fallback |
| Releases | Milestones 0.1 to 0.4, each released and usable, then 1.0 (section 11) |
| Telemetry | None |

## 4. Architecture

One Rust binary plus a thin herdr manifest. The code is split into six modules. Each has one job and a small interface.

| Module | Responsibility | Depends on |
|---|---|---|
| `herdr` | Reads `HERDR_PLUGIN_CONTEXT_JSON` (focused pane's directory and worktree), `HERDR_PLUGIN_CONFIG_DIR` and `HERDR_PLUGIN_STATE_DIR`. Maps a directory to `owner/name` through its git remote. Opens and focuses plugin panes through the herdr CLI. | nothing internal |
| `github` | Token resolution, GraphQL transport, rate-limit tracking and backoff, and the typed queries and mutations generated by `graphql_client`. Exposes a `Transport` trait so tests can replace the network. | nothing internal |
| `model` | Plain domain types (`Project`, `Field`, `Item`, `ItemContent`, `FieldValue`, `View`, `Iteration`, `Option`) and the capability matrix. | nothing |
| `store` | The confirmed board snapshot, the ordered list of pending edits, the derived view of the two, per-view ID lists, loading and saving the cache, and merging poll results. Sits behind a `Store` trait. | `model` |
| `ops` | One `Op` per user action. Each op knows how to apply itself to a snapshot (used when replaying pending edits) and which GraphQL mutation to send. | `model`, `github` |
| `ui` | ratatui on a tokio event loop: table and board layouts, item detail, pickers and editors, filter bar, toasts, help. Reads only from `store` and emits only `Op`s. | `store`, `ops` |

**Boundary rule:** types generated from the GitHub schema never leave `github` and `ops`. `store` and `ui` only see `model` types. This keeps schema changes contained to one layer and lets the store and UI be tested without a network.

### Domain model

- `Field` is an enum with one variant per field type: `Text`, `Number`, `Date`, `SingleSelect { options }`, `MultiSelect { options }`, `Iteration { iterations, completed }`, the built-in fields (`Title`, `Assignees`, `Labels`, `Milestone`, `Repository`, `LinkedPullRequests`, `Reviewers`, `ParentIssue`, `SubIssuesProgress`), and `Unsupported { name, type_name }` for any type the plugin does not know.
- Built-in fields are edited through their own operations, not `updateProjectV2ItemFieldValue`: `Title`, `Assignees`, `Labels` and `Milestone` through the issue and pull request mutations in section 6. `Repository`, `LinkedPullRequests`, `Reviewers`, `ParentIssue` and `SubIssuesProgress` are read-only in v1.
- `ItemContent` is `Issue`, `PullRequest`, `Draft`, or `Redacted` (content the viewer cannot see).
- `FieldValue` mirrors `Field`, with `Unsupported` carried through for display only.

### Capability matrix

`model::capabilities(item, board) -> set of Op kinds` is the single source of truth for which operations an item allows. The UI offers only what it returns, and bulk edits use it to split eligible from ineligible items.

| | Issue | Pull request | Draft | Redacted |
|---|---|---|---|---|
| Project fields, move, reorder, archive | yes | yes | yes | move and archive only |
| Title, body | yes | yes | yes (draft mutation) | no |
| Assignees | yes | yes | yes | no |
| Labels, milestone, comment | yes | yes | no | no |
| Close and reopen | yes, with a reason | yes, no reason | no | no |
| Convert to issue | no | no | yes | no |

Archived items allow only unarchive, remove and read. When `viewerCanUpdate` is false, nothing is writable.

### Single instance per board

- The plugin records the pane ID of each open board in the state directory, keyed by board.
- `open` for a board that already has a live pane focuses it with `herdr plugin pane focus` instead of opening another. A stale record (the pane no longer exists) is discarded and a new pane opens.
- A different board opens normally.
- Every cache and state write is atomic: write a temporary file, then rename it over the old one.

### herdr manifest

- `id = "tviles.project-boards"`, `platforms = ["linux", "macos"]`, `min_herdr_version = "0.9.0"` (developed against 0.9.3). Phase 0 checks herdr's release notes and raises the floor if any placement or API used arrived later.
- `[[panes]]` with id `board`, placement `tab`, command `["bin/project-boards", "pane"]`.
- Actions: `open` (configured placement), `open-picker`, `open-overlay`, `open-split`, `open-zoomed`, `doctor`.
- Placements are `tab`, `overlay`, `split` and `zoomed`, the four that `herdr plugin pane open --placement` accepts. Popup is not used: it has no pane ID, so it cannot be focused or tracked, and it is session-modal, which is wrong for a board kept open all day.
- A build step that fetches the release binary (section 4, Distribution).

### Distribution

- The build script reads `version` from `herdr-plugin.toml`, downloads the binary for release `v<version>` and the platform, and verifies it against the release's SHA-256 checksums file. A checksum mismatch aborts the install.
- If no release with that version exists, the script runs `cargo build --release` instead. Unreleased commits on `main` carry a `-dev` version (for example `0.4.0-dev`), so installing from `main` between releases builds from source.
- CI fails if `main`'s manifest version is neither a published release nor a `-dev` version.
- GitHub Actions builds and publishes the four binaries and the checksums file for each release tag.

### Command line

The binary also works outside herdr, for debugging and tests:

- `project-boards pane` — the TUI (what the manifest runs).
- `project-boards open [--project OWNER/NUMBER] [--placement tab|overlay|split|zoomed]` — opens or focuses a board pane through the herdr CLI.
- `project-boards doctor` — checks token, scopes, herdr, network, repository detection and key collisions, and prints fixes.

Outside herdr, `--project` is required because there is no pane context to infer a repository from.

## 5. Data flow

### The store's two layers

- **Confirmed snapshot:** the board as the server last reported it: schema, views, the shared item set, and each view's ID list.
- **Pending edits:** an ordered list of `Op`s sent but not yet confirmed.
- **What the user sees** is always the confirmed snapshot with the pending edits replayed on top, in order.
- When an edit succeeds, its result is written into the confirmed snapshot and it leaves the pending list. When it fails, it leaves the list and the view is rebuilt. There are no inverse patches.
- A poll result updates the confirmed snapshot; pending edits still replay on top, so a poll never overwrites an edit in flight.

### Opening a board

1. `herdr` resolves the focused pane's directory to a repository.
2. The board is chosen: the one remembered for that repository, otherwise the repository's linked projects (open directly if there is one, pick if there are several), otherwise the picker over all accessible boards (the viewer's and each of their organisations'). If that board already has a live pane, it is focused instead (section 4).
3. The view opened is the last one used on this board, otherwise the project's first view.
4. `store` paints the cached snapshot from the state directory at once and marks the header "stale".
5. `github` fetches in parallel:
   - the schema: fields, options, iterations, owner type and `viewerCanUpdate`;
   - the views: name, layout, filter, group-by fields, vertical group-by fields, sort fields, visible fields;
   - the shared item set: all items with their field values, 100 per page in `POSITION` order, up to `max_items`;
   - the active view's ID list: `items(query: <view filter>)` selecting only item IDs.
6. Any ID in a view's list that is not in the shared set is loaded with `nodes(ids: [...])`, 100 per request, and added to the set.
7. The fresh snapshot replaces the cache and the UI redraws.

### Views

- Every view is an ordered list of item IDs. Switching to a view shows its cached list immediately and revalidates it with one ID-only query.
- An edit changes one record in the shared set, and every view sees it.
- A view's ID list is capped at `max_items`. The header shows "loaded N of M" only when the cap is reached.

### Layouts

- **Table:** one row per item, with columns from the view's visible fields. A group-by field splits rows into collapsible sections.
- **Board:** one column per option of the view's column field (single-select or iteration), in GitHub's option order, plus a "No \<field\>" column. A group-by field adds horizontal swimlanes.
- Sort follows the view, unless a local override applies (section 5, Local view overrides).

### Items that stop matching the view

An edit can make an item stop matching the view's filter, for example moving a card to Done in a view filtered to `status:Todo,In Progress`.

- The item stays where it is, dimmed and marked "⊘ no longer matches filter", until the user leaves the view or presses `r`.
- On a board, if the filter hides the destination column, a temporary column (for example "Done (filtered)") holds the moved card until the user leaves the view.
- New items that the filter would hide follow the same rule.

### Filtering

- **View filter:** GitHub filter syntax, for example `status:Todo assignee:@me -label:bug`, passed to `items(query:)` so results match the web. Changing it refetches the view's ID list.
- **Archived items:** hidden by default, as on the web. "Show archived" in the action menu adds `is:archived` to the view filter, which is where items are unarchived.
- **Quick search (`/`):** an instant, client-side match on title, number, assignee and label over the items in the current view. It never touches the network.

### Polling

- **Incremental poll:** the query is `updated:>=<last poll time>` with no view filter, so one small request refreshes the shared set for every view. It updates items already in the set and adds new ones.
- **Interval:** every 30 seconds while the pane is focused, every 5 minutes while it is not, and immediately when focus returns.
- **Focus detection:** the plugin requests terminal focus events (`CSI ?1004h`). If none arrive (herdr does not forward them), it falls back to idle detection: after 5 minutes with no input it drops to the 5-minute interval, and the next keypress triggers an immediate refresh.
- **Full refetch:** every 10 minutes while focused and on `r`. This catches deletions and archives, which an incremental poll cannot see. It also revalidates the active view's ID list.
- **Budget:** the remaining GraphQL budget is read from response headers. When it runs low, polling slows and the header says so.

### Local view overrides

- Group-by and sort changes made in herdr are stored per view in the state directory.
- Each override records the view's GitHub group-by and sort as they were when the override was made. If GitHub's values later change, the override is dropped and a toast says so, for example "View 'Sprint' grouping changed on GitHub — local grouping cleared".
- While an override is active, the header shows a `local: group, sort` badge.
- `U` resets the view, clearing its local overrides.

### Large boards

The shared set's first load is capped by `max_items` (default 2,000), in board position order so the active columns load first. Items outside the cap that a view references are loaded on demand (step 6 above), so every view is complete. GitHub allows boards of up to 50,000 items.

## 6. Operations

### Catalogue

Every operation is offered only when the capability matrix (section 4) allows it.

| Area | Operation | GraphQL mutation |
|---|---|---|
| Fields | Set a text, number, date, single-select, multi-select or iteration value; move a card between columns | `updateProjectV2ItemFieldValue` |
| Fields | Clear a value | `clearProjectV2ItemFieldValue` |
| Order | Reorder within a column or the table | `updateProjectV2ItemPosition` |
| Items | Add an existing issue or pull request, by search or pasted URL | `addProjectV2ItemById` |
| Items | Create a new issue in a chosen repository and add it | `createIssue`, then `addProjectV2ItemById` |
| Items | Create, edit, or convert a draft | `addProjectV2DraftIssue`, `updateProjectV2DraftIssue`, `convertProjectV2DraftIssueItemToIssue` |
| Items | Archive, unarchive | `archiveProjectV2Item`, `unarchiveProjectV2Item` |
| Items | Remove from the project | `deleteProjectV2Item` |
| Issues and PRs | Add or remove assignees | `addAssigneesToAssignable`, `removeAssigneesFromAssignable` |
| Issues and PRs | Add or remove labels | `addLabelsToLabelable`, `removeLabelsFromLabelable` |
| Issues and PRs | Set milestone, title, body | `updateIssue`, `updatePullRequest` |
| Issues and PRs | Comment | `addComment` |
| Issues and PRs | Close (with reason) or reopen | `closeIssue`, `reopenIssue`, `closePullRequest`, `reopenPullRequest` |
| Views | Save name, layout, filter, visible fields | `updateProjectV2View` |
| Views | Create a view | `createProjectV2View` |
| Bulk | Set a field, assign, label or archive several selected items | the mutations above, run concurrently |

### Adding and creating items

- **Repository picker** for a new issue: the focused pane's repository first, then the project's linked repositories, then any repository the user can push to, searched as they type.
- **Add existing** searches open issues and pull requests in the linked repositories and the pane's repository by default. Typing `repo:owner/name` widens the search.
- **Where a new item lands:** pressing `n` in a board column or swimlane pre-sets that column's and swimlane's field values (for example Status = In Progress). In table layout, the item lands with no field values, at the bottom.
- The new item is selected and briefly highlighted.

### Bulk edits

- Bulk edits apply to the eligible items and skip the rest, reporting the result, for example "labelled 7, skipped 2 drafts".
- The confirmation prompt shows the eligible and skipped counts before anything runs.

### Keys

No default uses a Ctrl chord, so herdr's prefix (`ctrl+b`) and any Ctrl bindings the user has set keep working. Every key can be rebound in config.

| Key | Action |
|---|---|
| `h` `j` `k` `l`, arrows | Move the selection |
| `Tab`, `Shift+Tab` | Next or previous view |
| `L` | Switch between table and board |
| `<`, `>` | Move the card to the previous or next column |
| `J`, `K` | Reorder down or up |
| `Enter` | Item detail |
| `Space` | Action menu for the selected item, listing every allowed operation with its key |
| `a` | Assignees |
| `t` | Labels |
| `e` | Edit a field |
| `c` | Comment |
| `x` | Close or reopen |
| `n` | New: issue, draft, or add an existing item |
| `C` | Convert a draft to an issue |
| `A` | Archive |
| `o` | Open in the browser |
| `f` | Edit the view filter |
| `/` | Quick search |
| `g` | Group by |
| `s` | Sort |
| `W` | Save the view to GitHub |
| `U` | Reset the view's local overrides |
| `v` | Visual (multi) select |
| `E` | Error log; `R` in it retries the selected edit |
| `r` | Refresh |
| `?` | Help |
| `Esc` | Back or cancel; never quits |
| `q` | Quit |

A key whose operation the selected item does not allow shows a one-line reason, for example "Drafts have no labels — convert to issue first (`C`)".

Mouse: click selects, the wheel scrolls, and dragging a card to another column moves it.

### Item detail

- On terminals at least 140 columns wide, the detail opens as a right-hand split inside the board pane. Narrower, it takes the whole pane. `Esc` goes back.
- Bodies and comments are rendered from Markdown with `pulldown-cmark`: headings, emphasis, lists, task lists (`☐`/`☑`), quotes, code blocks with a distinct background, tables when they fit, and links as underlined text with a numbered footnote URL. Images render as `[image: alt]` with their URL.
- `m` toggles between rendered and raw text.
- `Enter` on a link footnote opens it in the browser. A `#123` reference jumps to that item's detail if it is on the board, and opens the browser otherwise.
- The newest 20 comments load first; "load older" fetches more.
- Sub-issues, parent issue, issue type and linked pull requests are shown read-only.

### Editors

- Fuzzy pickers for single values, multi-select pickers for labels and assignees. Candidate labels, assignable users and milestones come from the item's own repository, are loaded when first needed, and are cached.
- Date input accepts ISO dates (`2026-10-03`), offsets (`+3d`) and weekday names (`fri`).
- The iteration picker shows the current and upcoming iterations first.
- Titles, bodies and comments open in `$EDITOR`. The TUI suspends while the editor runs and resumes afterwards.

### Confirmation

Removing an item from the project, closing an issue or pull request, saving a view (`W`), and any bulk operation on more than 5 items ask for confirmation first. The `W` prompt states: "Saves filter, layout, fields. Grouping and sort stay local — GitHub's API can't write them."

### Colour

- Single-select options use their GitHub colours (GRAY, BLUE, GREEN, YELLOW, ORANGE, RED, PINK, PURPLE) mapped to the terminal's 16 ANSI colours, so the user's terminal theme applies.
- Colour is never the only signal: option names are always shown.
- `NO_COLOR` switches to plain text.

## 7. Error handling and edge cases

### Authentication

- The token is taken from `GH_TOKEN`, then `GITHUB_TOKEN`, then `gh auth token`.
- With no token, the pane shows a setup screen with the exact fix. `project-boards doctor` runs the same checks.
- Missing scopes are detected from the `X-OAuth-Scopes` response header or an `INSUFFICIENT_SCOPES` error, and the fix is shown as a command: `gh auth refresh -s project` (plus `repo` for private repositories).
- GitHub does not let fine-grained tokens read boards owned by a user account; those need a classic token or the `gh` login. The not-found error, the doctor and the documentation say so. Fine-grained tokens work for organisation boards that grant them the organisation's Projects permission.

### Permissions

- When `viewerCanUpdate` is false, the board is read-only: write keys are disabled and the header says "read-only".
- Items whose content the viewer cannot see arrive redacted and render as a "private item" placeholder, never an error.

### Failed edits

- A failed edit leaves the pending list, the view is rebuilt from the confirmed snapshot and the remaining pending edits, and a toast shows GitHub's own error message.
- `E` opens an error log of failed edits; `R` retries the selected one.
- With no network, edits fail immediately and are undone. There is no offline queue.

### Ordering and concurrency

- Edits to the same item are sent one at a time, in the order they were made, so "move to Done, then back to Todo" cannot finish in the wrong order.
- If an earlier edit to an item fails, later edits to the same item are still sent: each was a separate intent. The toast names the one that failed.
- Edits to different items run concurrently, at most 4 at a time, to stay under GitHub's secondary rate limit on mutations.
- Conflicts with other people's edits resolve as last write wins, the same as on the web. The next poll shows the result.

### Rate limits

- The primary GraphQL budget is tracked from response headers, and polling slows as it falls.
- A secondary rate limit (`403` or `429` with `retry-after`) pauses writes, with a countdown in the header.

### Schema drift

- An unknown field type or unknown item content becomes an `Unsupported` value shown read-only. It never crashes the plugin.
- `graphql_client` is configured to map unknown enum and union variants to a fallback variant.

### Cache and state

- One cache file per board, holding the confirmed snapshot (schema, views, shared item set, view ID lists) and a format version. A version mismatch or a parse failure discards the cache and refetches.
- The cache holds only board data, never the token.
- All writes are atomic (section 4).

### Terminal size

- In a narrow pane the board shows as many columns as fit and scrolls sideways, with an indicator of which columns are visible.
- Below a minimum size the pane shows a "widen the pane" message instead of a broken layout.

### Key collisions

`doctor` reads herdr's `config.toml` when it can and warns when a plugin key is also bound in herdr, since herdr would take it first.

### Logging

- A log file in the state directory, with verbosity controlled by `RUST_LOG`.
- The token and any `Authorization` header are never written to the log.

### Network

The plugin talks only to `api.github.com`. The build step also downloads from the repository's GitHub releases. There is no telemetry.

## 8. Configuration and state

`config.toml` in `HERDR_PLUGIN_CONFIG_DIR`. Every key is optional; these are the defaults:

```toml
poll_interval_secs = 30           # while focused
background_poll_interval_secs = 300
idle_after_secs = 300             # idle fallback when focus events are unavailable
full_refresh_mins = 10
max_items = 2000
placement = "tab"                 # tab | overlay | split | zoomed

[keys]
# overrides, for example:
# move_next_column = ">"
```

`NO_COLOR` is honoured.

`HERDR_PLUGIN_STATE_DIR` holds data the plugin writes itself: board caches, the board remembered per repository, the last view per board, local group-by and sort overrides per view, the pane ID of each open board, and the log file.

## 9. Testing

Test-driven, with at least 80% line coverage measured by `cargo-llvm-cov`.

- **Pure units:** `model` conversions; the capability matrix for every item type, archived state and read-only boards; `store` replay of pending edits over the confirmed snapshot, including a failed edit followed by a later one on the same field; poll merges; view ID lists and sticky items; local override invalidation; per-item ordering; quick search; date parsing; keymap parsing.
- **GitHub layer:** a fake `Transport` replaying recorded GraphQL responses. `wiremock` covers HTTP behaviour: rate-limit headers, `retry-after`, scope errors, redacted items and unknown field types. Mutation builders are checked against the variables they should send.
- **UI:** ratatui `TestBackend` with `insta` snapshot tests of the table, board, detail and pickers at 60, 100 and 200 columns.
- **Schema drift:** a weekly scheduled CI job pulls GitHub's latest schema, reruns code generation and the test suite, and opens an issue if anything breaks.
- **herdr smoke test:** manifest validation, `herdr plugin link .`, and a manual checklist covering each placement and single-instance focusing.

### Fixtures and live tests

- A public test project and repository under `tviles` (`tviles/project-boards-testbed`), created and seeded by a script with every field type, all three item types, iterations, and several views (table, board, board with swimlanes).
- `PB_RECORD=1 cargo test --features live` runs against the testbed and writes fixtures to `tests/fixtures/`. Plain `cargo test` replays them offline.
- The live suite exercises every operation in section 6 end to end. It runs locally before each release, not in CI: the testbed is user-owned, and only a classic token (too broad to store as a CI secret) can reach it.
- Cases that cannot be produced on demand, such as redacted items and unknown field types, use hand-written fixtures marked as such.

## 10. Verify before building (phase 0)

These facts were not confirmed during design. Each gets a short probe against the testbed before milestone 0.1, so every milestone is designed on confirmed facts.

| # | Question | Depends on it | Fallback if false |
|---|---|---|---|
| 1 | Does `items(query:)` accept `updated:>=<timestamp>`? | Incremental poll | Full refetch only, with longer default intervals |
| 2 | Does `items(query:)` accept the same filter syntax as the web, including `@me` and negation (`-label:`)? | View filters | Document the unsupported qualifiers and apply them client-side |
| 3 | Does a view's `filter` survive a write followed by a read, and are group-by and sort really read-only through the API? | Saving views | If group-by and sort turn out to be writable, save them to GitHub too and drop local overrides |
| 4 | Does herdr pass mouse drags through to a plugin pane in a tab? | Drag to move | Keyboard moves only; click-to-select remains |
| 5 | Does herdr forward terminal focus events (`CSI I` / `CSI O`) to plugin panes? | Focus-aware polling | Idle detection only |
| 6 | Does herdr take `Esc` to close an overlay before the plugin sees it? | `Esc` as back in overlay placement | Document it; `Esc` still works as back in the other placements |
| 7 | What is the oldest herdr release with every placement and CLI command used? | `min_herdr_version` | Raise the floor |

Confirmed during design from GitHub's published schema (2026-09-30): the view mutations `createProjectV2View`, `updateProjectV2View` and `deleteProjectV2View` exist; `UpdateProjectV2ViewInput` takes `name`, `layout`, `filter` and `configuration.visibleFieldIds` only; `ProjectV2.items` takes a `query` argument; `ProjectV2MultiSelectField` exists. Confirmed from herdr 0.9.3: `herdr plugin pane` has `open`, `focus` and `close`, and `open --placement` accepts `overlay`, `split`, `tab` and `zoomed`.

## 11. Milestones

Each milestone is released and usable on its own. Each gets its own section of the implementation plan.

| Version | Contents |
|---|---|
| Phase 0 | The probes in section 10, the testbed and its seed script, repository skeleton, CI, and the release pipeline |
| 0.1 Viewer | Auth, `doctor`, board choice, single instance, table and board layouts, views as tabs, view filter and quick search, item detail with Markdown, cache, focus-aware polling. Read-only. |
| 0.2 Item operations | The store's confirmed and pending layers, move, reorder, set and clear fields, sticky items, error log, capability matrix |
| 0.3 Issue operations | Assignees, labels, milestone, title and body, comments, close and reopen, add existing, new issue, drafts, archive and unarchive, remove |
| 0.4 Views and bulk | Save and create views, local overrides, visual select and bulk edits, mouse drag |
| 1.0 | Live suite passing, documentation complete, `herdr-plugin` topic added so the marketplace lists it |

The `herdr-plugin` topic is withheld until 1.0 so half-built versions stay off the marketplace.
