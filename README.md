# project-boards

A [herdr](https://herdr.dev) plugin for GitHub Projects: see your project board as a table or a kanban board, switch between its saved views, filter and search it, and read any item, without leaving the terminal.

0.2 edits the board: move cards between columns, reorder them, and set or clear any project field. Edits show at once and are sent in the background; a failed one is undone and listed in the error log. Assignees, labels, comments and new items arrive in 0.3.

## Install

    herdr plugin install tviles/project-boards

Requires herdr 0.9.0 or newer, macOS or Linux, and a GitHub token with the `project` scope (and `repo`, or `public_repo`, to edit issues from 0.3 on):

    gh auth refresh -s project -s repo

Releases install a prebuilt binary. Installing from `main` between releases builds from source and needs a Rust toolchain, 1.88 or newer.

Bind a key in `~/.config/herdr/config.toml`, then `herdr server reload-config`:

    [[keys.command]]
    key = "prefix+b"
    type = "plugin_action"
    command = "tviles.project-boards.open"

Other actions: `open-picker`, `open-overlay`, `open-split`, `open-zoomed`, `doctor`.

## Use

The board opens for the repository of the focused pane (the board you last used there, else its linked board, else a picker). Opening it again focuses the existing pane.

| Key | Action |
|---|---|
| `h` `j` `k` `l`, arrows | move |
| `Home` / `End` | first / last |
| `Tab` / `Shift+Tab` | next / previous view |
| `L` | table / board (remembered per view) |
| `<` / `>` | move the item to the previous / next column (stops at the first and last; never clears the field) |
| `K` / `J` | move the item up / down (unsorted views) |
| `e` | edit a field |
| `Space` | everything you can do with the item |
| `E` | error log of failed edits; `R` retries one, `d` dismisses one |
| `Enter` | item detail (collapse a group in the table) |
| `z` | collapse or expand a group |
| `/` | quick search |
| `f` | add a filter on top of the view's GitHub filter (GitHub filter syntax; this session only) |
| `x` | clear the filter added with `f` (`X` is kept for close and reopen in 0.3) |
| `o` | open in the browser |
| `B` | switch board |
| `r` | refresh |
| `?` | help |
| `Esc` | back or cancel; never quits |
| `q` | quit |
| `Ctrl+C` | quit (fixed, not rebindable) |

The bottom line lists the edit keys the selected item allows, and always ends with `? help`. The same edit keys work in the item detail. An item you edit stays in place until you leave the view or press `r`, even when the view's filter no longer matches it (it is then dimmed and marked ⊘). The header shows `saving N…` while edits are sending, and `writes paused Ns` while GitHub asks the plugin to slow down; quitting with edits still sending asks first (`y` quits, `n` or `Esc` stays; `Ctrl+C` quits at once). If the board is read-only for you the header says `read-only`; if the token lacks the `project` scope it says so, and the edit keys explain why.

`<` and `>` follow the columns the board shows, stopping at the first and the last; "No Status" is never a destination, so clearing a field is only done with `e`. Reordering needs a view with no sort and stays inside its lane or group.

`e` opens the menu of the item's project fields (type to filter): pick one, then choose a value. Text and number fields take typed input (empty clears); date fields take `2026-10-03`, `today`, `tomorrow`, `+3d`, `-2d`, `+1w` or a weekday such as `fri` (the next one after today, in your local time; empty clears); single-select and iteration pickers end with a "Clear" row; in a multi-select picker `Space` toggles and `Enter` applies. `Esc` goes back one level (value, then field menu, then the board). `Space` lists every operation the item allows with its key, and always offers "Open in the browser" when the item has a page.

An edit GitHub confirmed is kept on screen for 5 minutes, because GitHub's own lists lag behind writes; another person's change to the same field shows once that window ends. Failed edits go to the error log (`E`, the newest 50), which `Esc` closes. The help overlay (`?`) scrolls with `j`/`k` when the pane is short.

In the detail: `j`/`k` scroll, `Home`/`End` jump to the top or bottom, `Tab` cycles links, `Enter` follows one, `m` shows the raw Markdown, `P` loads older comments.

Apart from the fixed `Ctrl+C` quit, no default key uses Ctrl, so herdr's prefix and your own Ctrl bindings keep working. On the first-run board picker, where there is no board to go back to, `Esc` does nothing and `Ctrl+C` quits; on a picker opened with `B`, `Esc` cancels. Run the `doctor` action to find herdr bindings that shadow board keys.

## Configure

`config.toml` in the directory `herdr plugin config-dir tviles.project-boards` prints. Every key is optional:

    poll_interval_secs = 30            # while the pane is focused
    background_poll_interval_secs = 300
    idle_after_secs = 300
    full_refresh_mins = 10
    max_items = 2000
    placement = "tab"                  # tab | overlay | split | zoomed
    gh_user = "my-login"               # gh account for `gh auth token --user`; default: active account

    [keys]
    next_view = "]"                    # action = key

`NO_COLOR=1` turns colour off. Labels use GitHub's colours: exact on terminals that set `COLORTERM=truecolor`, otherwise the nearest of 256; with `NO_COLOR=1` they show as plain text.

## Tokens

Editing needs a classic token with the `project` scope for every board edit (and, from 0.3, `repo` or `public_repo` for issue edits). Fine-grained tokens report no scopes, so nothing is blocked up front; a refused write shows GitHub's message and the scope to add. The `doctor` action reports `can edit boards and issues` when both scopes are present, `can edit boards; editing issues needs ...` with only `project`, and a warning or failure when `project` is missing.

The token comes from `GH_TOKEN`, `GITHUB_TOKEN`, or `gh auth token`, in that order. If you have several `gh` accounts, `gh_user` in `config.toml` picks which one's token the plugin uses, without changing your active account. Boards owned by a user account need a classic token or the `gh` login (`gh auth login`, then `gh auth refresh -s project`): GitHub does not let fine-grained tokens read them. Fine-grained tokens work for organisation boards when they have the organisation's Projects permission.

## Troubleshooting

- `herdr plugin log list --plugin tviles.project-boards` shows action output.
- The pane logs to `project-boards.log` in its state directory (`~/.local/state/herdr/plugins/tviles.project-boards/`); set `RUST_LOG=debug` for more.
- In overlay placement, see `docs/phase0-findings.md` finding 6 for how `Esc` behaves.

## Privacy

The plugin talks only to `api.github.com`. It caches board data (never the token) in its state directory. No telemetry.
