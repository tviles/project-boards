# Changelog

All notable changes to project-boards are listed here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.4.0] - 2026-10-19

A read-only viewer for Linear boards, next to GitHub Projects. Editing Linear boards arrives in 0.5; saving views and bulk edits move to 0.6.

### Added

- Linear boards, read-only. Give the plugin a Linear personal API key with `LINEAR_API_KEY`, or with `linear_api_key_command` in `config.toml` (for example a Keychain lookup). The command runs without a shell and is given 20 seconds, so a dismissed password prompt never hangs the pane. The key is never logged, cached or written to disk.
- Open a Linear board from the picker, which lists Linear projects and then Linear teams after your GitHub boards, or with `open --project linear:team:KEY`, `open --project linear:project:SLUG` or `open --project <Linear project URL>`. Like a GitHub board, it reopens directly next time from the same repository.
- Project boards (a project's issues, across its teams) and team boards (all of a team's issues), as a table or a kanban board. The columns are the workflow states, in Linear's order; states with the same name in different teams share a column.
- Tabs: Board (without canceled and duplicate issues, or issues completed more than 14 days ago), All issues and My issues, then the project's own views on a project board, then the shared views of the board's teams and of the workspace. A view keeps its layout, grouping, ordering, visible fields and "hide completed issues" setting where the plugin recognises them.
- `f` on a Linear board takes the same filter syntax as on GitHub boards (`status:"In Progress"`, `assignee:@me`, `label:bug`, `priority:high`, `cycle:@current`, `-label:x`, `no:assignee`, free text), evaluated over the view's loaded issues. A field it does not know, such as the typo `stauts:`, flashes "unknown filter field: stauts" and matches nothing, so a typo never looks like a working filter. `/` quick search and "Show archived items" work as on GitHub boards.
- The issue detail shows the description and comments as Markdown, with replies under the comment they answer; the parent, the sub-issues and their states, project, cycle, priority, estimate, due date, and links. A reference such as `ENG-123` to one of the board's teams jumps to that issue when it is on the board and opens it in the browser otherwise. `o` opens the issue on linear.app.
- Other people's changes appear within one poll interval, including issues that were archived or deleted.
- The header shows the board's name and a `Linear` badge. Edit keys on a Linear board say "Editing Linear boards arrives in 0.5".
- `doctor` checks the Linear key, prints the workspace it reaches and the requests left in its budget, and prints "linear: not configured (optional)" without a key. With only a Linear key, a missing GitHub token is a warning, not a failure.

### Changed

- A board that no longer exists (deleted, or out of the token's reach) is forgotten for the repository, and the picker opens with "board not found: …", instead of the status line retrying "could not load the board" forever. This holds for a board shown from the cache too. A board is now remembered for a repository only once it has loaded.
- Without a Linear key, the picker shows one dim line naming the fix ("Linear boards: set LINEAR_API_KEY or linear_api_key_command"), or "Linear boards: linear_api_key_command gave no key" when the configured command gives none. GitHub boards work exactly as before.
- With no GitHub token and no Linear key, the setup screen lists both fixes.
- The `f` help line reads "add a filter on top of the view's own filter".
- Each board's cache is refetched once after upgrading (the cache format changed). Remembered boards, views and layouts are kept.

## [0.3.1] - 2026-10-14

### Fixed

- Items still being created say "new" on narrow board cards too, and quick search finds them by "new".

## [0.3.0] - 2026-10-14

Issue operations: the plugin now edits the issues and pull requests on a board, not only their project fields.

### Added

- `a` edits assignees and `t` edits labels: a checklist of the item's repository's assignable users or labels (type to filter, `Space` toggles, `Enter` sends only what changed). Each list holds the first 100 candidates and asks you to type to narrow when there are more.
- Milestones, from the repository's open milestones, with "No milestone" to clear. The `e` menu now also offers Title, Assignees, Labels and Milestone.
- Titles, bodies and comments are typed into a form (`c` comments; `Space` offers Edit title and Edit body). `Alt+Enter` submits; `Esc` asks before discarding what you typed; `Open in editor` takes the text to `$VISUAL` or `$EDITOR` and brings what you save back into the form.
- `X` closes an issue (with a reason: Completed or Not planned) or a pull request (after asking), and reopens a closed one.
- `A` archives or unarchives an item; `Space` then "Show archived items" lists archived items, and `x` hides them again. "Remove from project" is in the `Space` menu and always asks first.
- `n` adds an item: a new issue (in the pane's repository, the project's linked repositories, any `owner/name` you type, or a repository you can push to), a new draft, or an existing issue or pull request found by search or by pasting its URL or `owner/name#12`. On a board the new item lands in the selected column and lane.
- `C` converts a draft to an issue in a repository you choose.
- A new item or comment whose answer from GitHub was lost is marked in the error log as possibly created, and `R` will not resend it, so it cannot be posted twice.
- `r` also refreshes the cached assignee, label, milestone and repository lists, so labels or milestones added on GitHub show after a refresh.

### Changed

- Editing issues needs the `repo` or `public_repo` scope as well as `project`: `gh auth refresh -s project -s repo`.
- The bottom line lists the item's edit keys in a fixed order and always keeps `space actions` and `? help`; in a narrow pane the board-wide hints go first.
- Each board's cache is refetched once after upgrading (the cache format changed).

### Fixed

- An item detail whose item leaves the board (removed, or gone after a refresh) closes and says why, instead of staying blank.

## [0.2.0] - 2026-10-13

Item operations: the first release that edits a board.

### Added

- `<` and `>` move the item to the previous or next column, stopping at the first and the last; `K` and `J` move it up or down in views with no sort.
- `e` sets or clears any project field: single-select, iteration, text, number, date (`2026-10-03`, `today`, `+3d`, `fri` and similar) and multi-select.
- Edits show at once and are sent in the background, at most four at a time and in order for each item. A failed edit is undone and listed in the error log (`E`), where `R` retries it and `d` dismisses it.
- An edit GitHub confirmed stays on screen for 5 minutes, so GitHub's lagging lists cannot flip it back.
- An item you edit stays in place until you leave the view or press `r`, dimmed and marked ⊘ when the view's filter no longer matches it; on a board a temporary "(filtered)" column holds a card moved into a hidden column.
- `Space` lists everything you can do with the selected item, with its key, and always offers "Open in the browser". The bottom line lists the edit keys the item allows, and an edit key the item does not allow says why. The same keys work in the item detail.
- The header shows `saving N…` while edits are sending and `writes paused Ns` while GitHub asks the plugin to slow down; `read-only` when you cannot edit the board, and the missing scope when the token lacks `project`.
- Quitting with edits still sending asks first.
- `doctor` says whether the token can edit boards and issues.
- The help overlay scrolls when the pane is short.

### Changed

- `Ctrl+C` quits at once without asking; edits not yet sent are dropped.

### Fixed

- A panic in a background task no longer takes the terminal out of the board's screen while the board keeps running; it is logged instead.
- GitHub's secondary rate limit without a `retry-after` header waits 60 seconds instead of failing.

## [0.1.0] - 2026-10-12

The first release: a read-only viewer for GitHub Projects boards in herdr.

### Added

- Opens the board for the focused pane's repository: the board last used there, else the repository's linked board, else a picker over every board you can reach. Opening it again focuses the existing pane. Actions open it as a tab, an overlay, a split or zoomed.
- Table and kanban board layouts, with `L` to switch (remembered per view). Boards show a column per option, merge options that share a name, hide columns the view's filter excludes, and draw swimlanes; cards show the state dot, repository and number, assignee, title, the view's fields as pills, and labels in their GitHub colours.
- The project's saved views as tabs (`Tab`, `Shift+Tab`), with GitHub applying each view's filter.
- `f` adds a filter on top of the view's for this session, and `x` clears it; `/` searches the view's items by title, number, assignee and label.
- The item detail: body and comments as Markdown with numbered links, sub-issues, parent, issue type and linked pull requests; `#123` references jump to the item, `P` loads older comments, `m` shows the raw Markdown.
- The board paints from a cache at once and refreshes in the background: every 30 seconds while the pane is focused, every 5 minutes otherwise, with a full refresh every 10 minutes and on `r`. Polling slows when the GitHub rate-limit budget runs low.
- The `doctor` action checks the token and its scopes, GitHub, herdr, the configuration, the repository and keys that herdr would take first.
- `config.toml` sets poll intervals, `max_items`, the placement, `gh_user` and key bindings; no default key uses Ctrl apart from the fixed `Ctrl+C` quit. `NO_COLOR` is honoured.
- Releases install a checksum-verified prebuilt binary for macOS and Linux; installing from `main` builds from source.

[Unreleased]: https://github.com/tviles/project-boards/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/tviles/project-boards/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/tviles/project-boards/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/tviles/project-boards/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/tviles/project-boards/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/tviles/project-boards/releases/tag/v0.1.0
