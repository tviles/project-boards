# Smoke checklist (run before every release)

Setup: `cargo build --release && herdr plugin link .`, keybinding for `tviles.project-boards.open`, a pane `cd`'d into a clone of `tviles/project-boards-testbed`.

- [ ] Key opens a tab with the testbed board; header shows `stale` (second run) or `loading`, then neither.
- [ ] Pressing the key again focuses the same tab; no second tab opens.
- [ ] `open-picker` lists the testbed board first; `Esc` on a fresh picker does nothing and `Ctrl+C` quits.
- [ ] `open-overlay`, `open-split` and `open-zoomed` each open the board; quitting returns focus.
- [ ] `Tab` reaches all four views; "Bugs" shows only bug-labelled items after a moment.
- [ ] "Board" shows Status columns with Priority lanes and a "No Priority" lane under Todo.
- [ ] `L` switches layout; `/emoji` finds the 🚀 item; `f` + `label:docs` + Enter shows one item.
- [ ] `Enter` on "Crash when board has no Status field" shows the body, two comments, `[1]` link; `Tab` + `Enter` opens example.com in the browser.
- [ ] Shrinking the pane below 40 columns shows "Widen the pane".
- [ ] Editing an item's Status on github.com shows up within 30 seconds while focused.
- [ ] `GH_TOKEN=bad` shows the setup screen with `gh auth login`; `r` retries.
- [ ] `doctor` action shows a notification; the plugin log has one line per check.
