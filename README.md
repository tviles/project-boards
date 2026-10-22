# project-boards

A [herdr](https://herdr.dev) plugin for GitHub Projects and Linear: see your project board as a table or a kanban board, switch between its saved views, filter and search it, and read any item, without leaving the terminal.

Edit a GitHub board without leaving herdr: move cards, reorder them, set any field, assign people, label, set milestones, retitle, edit bodies, comment, close and reopen, add new issues, drafts or existing items, convert drafts, archive and remove. Edits show at once and are sent in the background; a failed one is undone and listed in the error log. Linear boards are editable too, with the same keys (see [Linear](#linear)); saving views and bulk edits arrive in 0.6.

## Install

    herdr plugin install tviles/project-boards

Requires herdr 0.9.0 or newer, macOS or Linux, and a GitHub token with the `project` scope (and `repo`, or `public_repo`, to edit issues):

    gh auth refresh -s project -s repo

For Linear boards, add a Linear API key instead or as well (see [Linear](#linear)).

Releases install a prebuilt binary. Installing from `main` between releases builds from source and needs a Rust toolchain, 1.88 or newer.

Bind a key in `~/.config/herdr/config.toml`, then `herdr server reload-config`:

    [[keys.command]]
    key = "prefix+b"
    type = "plugin_action"
    command = "tviles.project-boards.open"

Other actions: `open-picker`, `open-overlay`, `open-split`, `open-zoomed`, `doctor`.

## Use

The board opens for the repository of the focused pane (the board you last used there, else its linked board, else a picker). Opening it again focuses the existing pane. The picker lists your GitHub boards, then Linear projects and teams when a Linear key is configured; a board that no longer exists is forgotten, and the picker opens with "board not found".

From a shell, the plugin's binary (`bin/project-boards` in the plugin's directory) opens a given board: `project-boards open --project OWNER/NUMBER` for a GitHub board, or `--project linear:team:KEY` (a team key such as `ENG`), `--project linear:project:SLUG` (the project's slug, as in its URL) or `--project https://linear.app/<workspace>/project/<slug>` for a Linear board.

| Key | Action |
|---|---|
| `h` `j` `k` `l`, arrows | move |
| `Home` / `End` | first / last |
| `Tab` / `Shift+Tab` | next / previous view |
| `L` | table / board (remembered per view) |
| `<` / `>` | move the item to the previous / next column (stops at the first and last; never clears the field) |
| `K` / `J` | move the item up / down (unsorted views) |
| `e` | edit a field (the project's fields, plus Title, Assignees, Labels and Milestone) |
| `a` / `t` | assignees / labels (type to filter, Space toggles, Enter applies) |
| `c` | comment (opens a form) |
| `X` | close (issues: with a reason) or reopen |
| `A` | archive or unarchive |
| `n` | new issue, new draft, or add an existing issue or pull request |
| `C` | convert a draft to an issue |
| `Space` | everything you can do with the item |
| `E` | error log of failed edits; `R` retries one, `d` dismisses one |
| `Enter` | item detail (collapse a group in the table) |
| `z` | collapse or expand a group |
| `/` | quick search |
| `f` | add a filter on top of the view's own filter (GitHub filter syntax; this session only) |
| `x` | clear the filter added with `f` |
| `o` | open in the browser |
| `B` | switch board |
| `r` | refresh |
| `?` | help |
| `Esc` | back or cancel; never quits |
| `q` | quit |
| `Ctrl+C` | quit now (unsent edits are dropped); fixed, not rebindable |

The bottom line lists the edit keys the selected item allows (`a`, `t`, `e`, `< >`, `X`, `c`, `J K`, `A`, `C`, in that order), and always keeps `space actions` and `? help`; when the pane is narrow the board-wide hints (`/`, `f`, `L`, `q`) go first, then the edit hints from the right. The same edit keys work in the item detail. An item you edit stays in place until you leave the view or press `r`, even when the view's filter no longer matches it (it is then dimmed and marked ⊘). The header shows `saving N…` while edits are sending, and `writes paused Ns` while GitHub asks the plugin to slow down; quitting with edits still sending asks first (`y` quits, `n` or `Esc` stays). `Ctrl+C` is the emergency exit: it quits at once without asking, and edits not yet sent are dropped. If the board is read-only for you the header says `read-only`; if the token lacks the `project` scope it says so, and the edit keys explain why.

`<` and `>` follow the columns the board shows, stopping at the first and the last; "No Status" is never a destination, so clearing a field is only done with `e`. Reordering needs a view with no sort and stays inside its lane or group.

`e` opens the menu of the item's project fields (type to filter): pick one, then choose a value. Text and number fields take typed input (empty clears); date fields take `2026-10-03`, `today`, `tomorrow`, `+3d`, `-2d`, `+1w` or a weekday such as `fri` (the next one after today, in your local time; empty clears); single-select and iteration pickers end with a "Clear" row; in a multi-select picker `Space` toggles and `Enter` applies. `Esc` goes back one level (value, then field menu, then the board). `Space` lists every operation the item allows with its key, and always offers "Open in the browser" when the item has a page.

An edit GitHub confirmed is kept on screen for 5 minutes, because GitHub's own lists lag behind writes; another person's change to the same field shows once that window ends. Failed edits go to the error log (`E`, the newest 50), which `Esc` closes. The help overlay (`?`) scrolls with `j`/`k` when the pane is short.

`a` and `t` open a multi-select of the item's repository's assignable users or labels, with yours and the item's current values checked: type to filter, `Space` toggles, `Enter` sends only what changed, `Esc` goes back (to the field menu when you came from `e`, otherwise it closes). Each list holds the first 100 candidates and says "showing the first 100 — type to narrow" when there are more. Drafts have no labels (convert them with `C` first); a draft's assignees come from the pane's repository. The milestone is chosen from the repository's open milestones (`e` then Milestone, or `Space`), with "No milestone" to clear it.

Titles, bodies, comments and new items are typed into a form: a `Title` line and/or a `Body`, then buttons. `Tab` and `Shift+Tab` move between fields and buttons; `Enter` in the title moves to the body (or saves a title), `Enter` in the body starts a new line, `Enter` on a button runs it, and `Alt+Enter` submits from anywhere. Every other key types, so `q` and `j` are letters here. `Esc` closes the form, asking "Discard what you typed? (y/n)" first if you typed anything. An empty title creates nothing, and an unchanged title or body or an empty comment sends nothing: the form says so and keeps your text. `Open in editor` takes the text to `$VISUAL` or `$EDITOR` (`vi` if neither is set; a new item as its title, a blank line, then the body), and what you save comes back into the form to submit. An editor that opens a window and returns at once needs its wait flag (`VISUAL='code -w'`). `Space` offers Edit title (also in `e`), Edit body, Milestone, Remove from project and Show archived items.

`X` closes an open item: an issue asks for a reason (Completed or Not planned), a pull request asks `y/n`; on a closed item it reopens at once. `A` archives or unarchives (an archived item dims and is marked ⊘ until you leave the view or press `r`). `Space` then Show archived items adds `is:archived` to the session filter, which `x` clears. Remove from project is only in the `Space` menu and always asks first; the issue stays in its repository, but a draft is deleted.

`n` asks what to add. On a board the new item lands in the selected column and the selected card's lane; in a table it takes no values. New draft opens the form. New issue first asks for the repository, then opens the form: the pane's repository, then the project's linked repositories, then any `owner/name` you type and press `Enter` on, then repositories you can push to ("showing the first 500 repositories; type owner/name for another"). Add an existing issue or pull request takes a search (`Enter` searches, then `Enter` adds the highlighted hit) limited to open items (unless you type `is:closed`) in the pane's and linked repositories (with neither, type `repo:owner/name`); `is:open`, `is:closed`, `is:issue` and `is:pr` are understood, any other word is plain text, and `repo:owner/name` searches that repository instead. Pasting an issue or pull request URL (`https://github.com/owner/name/issues/12`) or `owner/name#12` adds that item directly; one already on the board is selected instead. `Esc` goes back one level (search or repository, then the kind menu, then the board). `C` asks for a repository and turns a draft into an issue there.

If GitHub never answers a request that creates something, the error log says it "may have been created; check GitHub before retrying", and `R` refuses it: look on github.com first, then add it again if it is missing.

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
    gh_user = "my-login"               # gh account for `gh auth token --user`, used when GH_TOKEN and GITHUB_TOKEN are not set; default: active account
    linear_api_key_command = ["security", "find-generic-password", "-s", "project-boards-linear", "-w"]  # prints the Linear API key; used when LINEAR_API_KEY is not set

    [keys]
    next_view = "]"                    # action = key

`NO_COLOR=1` turns colour off. Labels use GitHub's colours: exact on terminals that set `COLORTERM=truecolor`, otherwise the nearest of 256; with `NO_COLOR=1` they show as plain text.

## Tokens

Editing needs a classic token with the `project` scope for every board edit (and `repo` or `public_repo` for issue edits). Fine-grained tokens report no scopes, so nothing is blocked up front; a refused write shows GitHub's message and the scope to add. The `doctor` action reports `can edit boards and issues` when both scopes are present, `can edit boards; editing issues needs ...` with only `project`, and a warning or failure when `project` is missing.

The token comes from `GH_TOKEN`, `GITHUB_TOKEN`, or `gh auth token`, in that order. If you have several `gh` accounts, `gh_user` in `config.toml` picks which one's token the plugin uses, without changing your active account. Boards owned by a user account need a classic token or the `gh` login (`gh auth login`, then `gh auth refresh -s project`): GitHub does not let fine-grained tokens read them. Fine-grained tokens work for organisation boards when they have the organisation's Projects permission.

## Linear

Linear boards need a personal API key: in Linear, open Settings → Security & access, and create a personal API key. Give it to the plugin in one of two ways:

- Store it in the macOS Keychain (the command prompts for the key):

      security add-generic-password -s project-boards-linear -a "$USER" -w

  and point `config.toml` at it:

      linear_api_key_command = ["security", "find-generic-password", "-s", "project-boards-linear", "-w"]

  The command runs directly, without a shell, and its trimmed output is the key. Any program that prints the key works: `op read`, `pass show` or `secret-tool lookup`, for example. It is given 20 seconds, so a dismissed password prompt cannot hang the pane.
- Or set `LINEAR_API_KEY`, which is used first when it is set.

The key is never logged, cached or written to a file. One key reaches one workspace; `doctor` prints which one, and the picker shows a dim line naming the fix when no key is found.

A Linear board is a project (its issues, across its teams) or a team (all its issues), opened from the picker or with `open --project` (see [Use](#use)). Its tabs are Board, All issues and My issues, then the project's views (on a project board), then the shared views of its teams and of the workspace. The detail shows the description, threaded comments, parent and sub-issues, project, cycle, priority, estimate, due date and links; a reference such as `ENG-123` jumps to that issue. Other people's changes show within one poll interval.

Linear boards are editable with the same keys as GitHub boards. What differs: an issue has one assignee; states, cycles and team label groups belong to a team, so a move into a column the issue's team lacks is refused with a reason; remove works only on project boards (it clears the issue's project; on a team board, archive instead); there are no drafts or milestones; estimates are whole numbers; and on a project board, `n` → Add existing takes an issue key such as `ENG-123` or a linear.app URL. Linear rewrites Markdown list markers in descriptions when it stores them (`- ` becomes `* `).

`f` takes the same syntax as on GitHub (`priority:high`, `cycle:@current`, `assignee:@me`, `-label:x`, `no:assignee`, free text), but the plugin evaluates it over the view's loaded issues instead of sending it to the server. A field it does not know, such as a typo like `stauts:Todo`, flashes "unknown filter field" and matches nothing, rather than being ignored. `/` and "Show archived items" work as on GitHub.

## Troubleshooting

- `herdr plugin log list --plugin tviles.project-boards` shows action output.
- The pane logs to `project-boards.log` in its state directory (`~/.local/state/herdr/plugins/tviles.project-boards/`); set `RUST_LOG=debug` for more.
- In overlay placement, see `docs/phase0-findings.md` finding 6 for how `Esc` behaves.
- While `$EDITOR` (opened from a form's `Open in editor`) is open the board waits for it. Quit the editor rather than suspending it with `Ctrl+Z`: the board is suspended with it, and a herdr pane has no shell to bring them back with `fg`.
- An editor that opens a window and returns at once (`code`, `subl`) needs its waiting flag: `VISUAL='code -w'`.

## Privacy

The plugin talks only to `api.github.com`, and to `api.linear.app` when a Linear key is configured. It caches board data (never the token or the API key) in its state directory. No telemetry.
