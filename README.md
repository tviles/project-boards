# project-boards

A [herdr](https://herdr.dev) plugin for GitHub Projects: see your project board as a table or a kanban board, switch between its saved views, filter and search it, and read any item, without leaving the terminal.

0.1 is read-only. Editing (moving cards, fields, assignees, labels, comments) arrives in 0.2 and 0.3.

## Install

    herdr plugin install tviles/project-boards

Requires herdr 0.9.0 or newer, macOS or Linux, and a GitHub token with the `project` scope:

    gh auth refresh -s project        # add -s repo for private repositories

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
| `Enter` | item detail (collapse a group in the table) |
| `z` | collapse or expand a group |
| `/` | quick search |
| `f` | edit the view filter (GitHub filter syntax) |
| `o` | open in the browser |
| `B` | switch board |
| `r` | refresh |
| `?` | help |
| `Esc` | back or cancel; never quits |
| `q` | quit |
| `Ctrl+C` | quit (fixed, not rebindable) |

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

The token comes from `GH_TOKEN`, `GITHUB_TOKEN`, or `gh auth token`, in that order. If you have several `gh` accounts, `gh_user` in `config.toml` picks which one's token the plugin uses, without changing your active account. Boards owned by a user account need a classic token or the `gh` login (`gh auth login`, then `gh auth refresh -s project`): GitHub does not let fine-grained tokens read them. Fine-grained tokens work for organisation boards when they have the organisation's Projects permission.

## Troubleshooting

- `herdr plugin log list --plugin tviles.project-boards` shows action output.
- The pane logs to `project-boards.log` in its state directory (`~/.local/state/herdr/plugins/tviles.project-boards/`); set `RUST_LOG=debug` for more.
- In overlay placement, see `docs/phase0-findings.md` finding 6 for how `Esc` behaves.

## Privacy

The plugin talks only to `api.github.com`. It caches board data (never the token) in its state directory. No telemetry.
