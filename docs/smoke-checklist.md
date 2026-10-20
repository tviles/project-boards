# Smoke checklist (run before every release)

Setup: `cargo build --release && herdr plugin link .`, keybinding for `tviles.project-boards.open`, a pane `cd`'d into a clone of `tviles/project-boards-testbed`.

- [ ] Key opens a tab with the testbed board; the header shows `loading` on the first run and `stale` on the second, then neither.
- [ ] Pressing the key again focuses the same tab; no second tab opens.
- [ ] `open-picker` lists the testbed board first; `Esc` on a fresh picker does nothing and `Ctrl+C` quits.
- [ ] `Esc` on a `B` picker cancels it and returns to the board.
- [ ] `open-overlay`, `open-split` and `open-zoomed` each open the board; quitting returns focus.
- [ ] `Tab` reaches all four views; "Bugs" shows only bug-labelled items after a moment.
- [ ] "Board" shows Status columns with Priority lanes and a "No Priority" lane under Todo.
- [ ] `L` switches layout; `/emoji` finds the 🚀 item; `f` + `label:docs` + Enter shows one item.
- [ ] `Enter` on "Crash when board has no Status field" shows the body, two comments, `[1]` link; `Tab` + `Enter` opens example.com in the browser.
- [ ] Shrinking the pane below 40 columns shows "Widen the pane".
- [ ] Editing an item's Status on github.com shows up within about 2 minutes while focused (GitHub's search index can lag over a minute).
- [ ] `GH_TOKEN=bad` shows the setup screen with `gh auth login`; `r` retries.
- [ ] `doctor` action shows a notification; the plugin log has one line per check.

## Editing (0.2)

- [ ] On "Board", `>` moves a card to the next column at once; the header shows `saving 1…` briefly; github.com shows the move.
- [ ] `>` twice quickly from Todo moves the card to Done and GitHub ends there; a third `>` says "Already in the last column" and never clears Status; `<` on Todo says "Already in the first column".
- [ ] `K`/`J` reorder within a lane; on a sorted view they explain why not.
- [ ] `e` on an item edits Status, Priority, Notes (text), Size (number), Due (`fri`, then `+3d`, then empty to clear), Sprint (current one first) and Areas (Space toggles, Enter applies); `Esc` from a value goes back to the field menu, a second `Esc` closes it.
- [ ] "Bugs" view: move a bug to Done with `e`; it stays, dimmed with ⊘; `Tab` and back makes it go.
- [ ] A board view filtered with `f` and `-status:Done`: `>` from In Progress stops (Done is hidden); setting Status to Done with `e` shows a "Done (filtered)" column.
- [ ] `Space` lists the allowed operations with their keys and always ends with "Open in the browser"; `Enter` and the listed keys run them.
- [ ] The edit keys (`<` `>` `e` `Space`) work the same in the item detail, and `Esc` returns to the detail.
- [ ] After moving a card, wait 2 minutes (inside the 5-minute window) with the pane focused: the poll does not flip it back to its old column.
- [ ] With the network off (Wi-Fi off), an edit is undone with an error ending in `E error log`; `E` lists it; back online, `R` retries it; `d` dismisses an entry; `Esc` closes the log.
- [ ] `x` clears a filter added with `f`; `X` does nothing.
- [ ] `q` right after an edit asks "1 edit is still sending; quit anyway?"; `n` stays; `Ctrl+C` quits at once.
- [ ] `?` scrolls with `j`/`k` in a short pane (about 12 rows) and the bottom line still ends with `? help`.
- [ ] A board you can only read shows `read-only` in the header; `>` says "This board is read-only for you".
- [ ] With a token lacking the `project` scope, the header shows `read-only: token lacks the project scope` (`read-only (no scope)` when narrow).
- [ ] `doctor` says "can edit boards and issues" with a token that has `project` and `repo`.

## Editing (0.3)

- [ ] In `$EDITOR` (`c`, then `Open in editor`), type `q`, `j`, `x`, `X`, `>` and arrow keys, then save and quit: back on the board nothing quit, moved, closed or changed filters (no keystroke typed in the editor reached the board), and the whole screen is redrawn.
- [ ] With `VISUAL='code -w'`, `c`, `Open in editor`, then `Ctrl+C` in the pane: the form says "The editor exited with an error; nothing was sent", keeps its text, and the board stays open.
- [ ] With the editor open from the `c` form, `kill <pane pid>` from another pane, then quit the editor: the board exits and no comment is sent.
- [ ] `a` on an issue lists testbed assignees with yours checked; toggling and Enter changes it on github.com; `Esc` closes it (or goes back to the field menu when opened from `e`).
- [ ] `t`, type `good`, Space, Enter adds "good first issue"; on a draft `t` says "Drafts have no labels — convert to issue first (`C`)".
- [ ] `e` → Milestone sets v2 and "No milestone" clears it.
- [ ] `Space` → Edit title opens the form with the title; `Enter` saves and retitles; saving it unchanged says "Title unchanged" in the form.
- [ ] `c` opens the "Comment on #n" form; typing `q`, `j`, `e` types letters; `Alt+Enter` adds a comment that shows in the detail; submitting it empty says "Empty comment; nothing was sent" in the form.
- [ ] `Open in editor` in a form (a comment, and a new draft with a title and body) opens `$EDITOR` on the form's text; save and quit: the edited text is back in the form and nothing was sent until `Alt+Enter`.
- [ ] `Esc` in a form you typed in asks "Discard what you typed? (y/n)"; `n` returns with the text intact, `y` closes and sends nothing; `Esc` in an untouched form closes at once.
- [ ] `X` on an issue offers completed / not planned; on the sample pull request it asks y/n; `X` again reopens.
- [ ] `A` archives (the item dims, ⊘); `Space` → Show archived items lists it; `A` unarchives.
- [ ] `Space` → Remove from project asks first; the issue stays in the repository.
- [ ] On "Board", `n` in "In Progress" → New draft opens the form; Title and Body then `Alt+Enter` create a draft in that column and lane, selected.
- [ ] `n` → New issue offers this pane's repository first, then opens the "New issue in owner/name" form; the issue appears on github.com in the project; `Esc` in the repository picker returns to the New item menu.
- [ ] `n` → Add existing finds an issue by text and by pasted URL; one already on the board is selected instead.
- [ ] `C` on a draft converts it to an issue in the chosen repository.

## Linear (0.4)

Setup: the testbed workspace's key in `LINEAR_API_KEY` (or `linear_api_key_command`), the GitHub token as above, and the same pane in the testbed clone. Never point this at a real workspace.

- [ ] `open-picker` lists the GitHub boards, then "Testbed project" under Linear projects, then TBA and TBB under Linear teams; picking TBA opens it with the team's name and `· Linear` in the header, `loading` at first, then neither.
- [ ] Quit and press the key again in the same pane: TBA reopens directly, without the picker. `project-boards open --project linear:team:TBA` from a shell focuses the same tab.
- [ ] The Board tab shows Status columns from the workflow states in Linear's order (backlog, then unstarted, started, completed); on the project board, states that TBA and TBB share by name are one column, and no canceled or duplicate issues show.
- [ ] `Tab` reaches Board, All issues, My issues, then the team view "Testbed A bugs" and the workspace view "Testbed open"; on the project board (`linear:project:…` from the picker) the project's own view comes right after the built-in tabs.
- [ ] `f` + `priority:high` shows only High issues; `f` + `cycle:@current` shows only the current cycle's issues; `x` clears it.
- [ ] `f` + `stauts:Todo` flashes "unknown filter field: stauts" and the view is empty.
- [ ] `Enter` on an issue with sub-issues shows its description, threaded comments (a reply indented under its comment), the parent (on a sub-issue), the sub-issues with their states, and its links; a `TBA-…` reference in the text jumps to that issue.
- [ ] Change an issue's status in Linear's web app: the board shows it within one poll interval (30 seconds while focused).
- [ ] `e`, `<`, `>`, `a`, `t`, `c`, `X`, `n` and `A` each say "Editing Linear boards arrives in 0.5"; `Space` offers only "Open in the browser" and "Show archived items"; `o` opens the issue on linear.app.
- [ ] `doctor` prints `linear key: from LINEAR_API_KEY` and `linear: workspace project-boards-linear-testbed; … requests left`.
- [ ] Without `LINEAR_API_KEY` (and no `linear_api_key_command`): the picker shows one dim line "Linear boards: set LINEAR_API_KEY or linear_api_key_command" and no Linear sections, `doctor` prints `linear: not configured (optional)`, and the testbed GitHub board works as in 0.3 (open, move a card, `f` help reads "add a filter on top of the view's own filter").
- [ ] With no credentials at all (`GH_TOKEN`, `GITHUB_TOKEN` and `LINEAR_API_KEY` unset, no `linear_api_key_command`, and `gh_user = "no-such-account"` in `config.toml` so `gh auth token` gives nothing): the setup screen lists the GitHub fix, "— or —", then the Linear fix.
