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

- [ ] In `$EDITOR` (opened with `c`), type `q`, `j`, `x`, `X`, `>` and arrow keys, then save and quit: back on the board nothing quit, moved, closed or changed filters (no keystroke typed in the editor reached the board), and the whole screen is redrawn.
- [ ] With `VISUAL='code -w'`, `c` then `Ctrl+C` in the pane cancels the comment ("The editor exited with an error; nothing was sent") and the board stays open.
- [ ] With the editor open from `c`, `kill <pane pid>` from another pane, then quit the editor: the board exits and no comment is sent.
