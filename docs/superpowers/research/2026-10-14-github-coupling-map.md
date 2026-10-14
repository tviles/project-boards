# How GitHub-specific project-boards is (coupling map, 2026-10-14)

A map of where the codebase assumes GitHub Projects v2, written to plan a second backend (Linear; see `2026-10-14-linear-api.md`). Taken from reading the code at v0.3.1 (main `b5f8d27`, version 0.3.2-dev). Line numbers are approximate.

## 1. Layering and seams

- **The boundary rule mostly holds.** `graphql_client` types stay in `crates/queries` (the `operation!` macro over the named operations in `board.graphql`) and `src/github/`. Responses are decoded by hand from `serde_json::Value` in `src/github/convert.rs`.
- **Leaks:**
  - `crate::github::GithubError` is used across `SyncEvent::{OpDone, Failed}` (`src/sync/mod.rs`), `ops::Confirmed::{Created.refused, NotAdded.error}`, `ops::send::send`, `ui/controller.rs`, `ui/app.rs` (`setup_message`), `ui/app/edit.rs` (`error_message`, `record_refused_scopes`) and `ui/runtime.rs`.
  - `RateInfo` from `github::transport` is carried in `SyncEvent::Rate`.
- **Only three non-github modules touch the concrete `Github` client:**
  - `sync/syncer.rs` (`Syncer { gh: Arc<Github>, … }`);
  - `ops/send.rs` (`send(gh: &Github, project, op)`);
  - `ui/runtime.rs` and `commands/doctor.rs`, which construct it.
- **The `Github` facade is already shaped like a provider:** its methods return model types only.
  - Reads: `resolve_project`, `fetch_project`, `viewer`, `fetch_items_page`, `fetch_view_ids_page`, `items_page_cost`, `hydrate_items`, `fetch_item_detail`.
  - Board discovery: `list_repo_projects`, `list_viewer_projects`.
  - Lookups: `repo_candidates`, `pushable_repos`, `lookup_repo`, `search_content`, `content_by_number`.
  - About 20 mutations in `mutations.rs`.
- **Call-site categories:**
  - sync jobs through `Syncer`: `resolve`, `refresh_project`, `full_load`, `incremental`, `view_ids` (with hydrate), `detail`, `repo_projects`, `projects`, `candidates`, `repos`, `lookup_repo`, `search`, `mutate`;
  - edits through `ops::send::send`, called only from `Syncer::mutate`;
  - `commands/doctor.rs::run_doctor`;
  - token resolution through `github/token.rs::resolve_token_from_system(gh_user)`;
  - tests and tooling: `tests/common`, `tests/live*.rs`, `tests/github_fixtures.rs`, `examples/probe_api.rs`.
- **Lower seam: `trait Transport`** (`github/transport.rs`): `execute(GraphqlRequest) -> BoxFuture<Result<GraphqlResponse, GithubError>>`.
  - Implementations: `HttpTransport` (`GITHUB_ENDPOINT`, Bearer), `FixtureTransport`, `RecordingTransport`.
  - It is GraphQL-shaped, so it is reusable for Linear (also GraphQL), but `GraphqlResponse` is GitHub-biased: `scopes` comes from `X-OAuth-Scopes`, and its `errors` are the tolerated `NOT_FOUND`/`FORBIDDEN` ones.
- **Upper seam: `trait Store`** (`store/mod.rs`). Generic over ops and snapshots; `MemoryStore` has no GitHub types outside tests.

## 2. Domain model: generic or GitHub-specific

- **Ids** (`model/ids.rs`):
  - `ProjectId`, `ItemId`, `FieldId`, `ViewId`, `OptionId`, `IterationId` are opaque string newtypes, so generic.
  - `BoardRef { owner, number: u32 }` is GitHub-shaped: `key()` gives `owner__number`, `Display` gives `owner/number`, and `FromStr` requires a numeric number. It is the identity for the cache, `state.json`, `panes.json`, the CLI (`open --project OWNER/NUMBER`) and `PB_BOARD`.
  - `RepoSlug { owner, name }` is GitHub-only. It is threaded through `SyncJob::{RepoProjects, Candidates, LookupRepo, Search.scope}` and `PaneOptions.repo`.
- **`Item`** (`model/item.rs`): `{ id, content, archived, updated_at, values: BTreeMap<FieldId, FieldValue>, content_fields: ContentFields }`.
  - `ItemContent` is `Issue{reference,title,state} | PullRequest{…,is_draft} | Draft{title} | Redacted | Unknown{type_name}`. This is GitHub's Projects item taxonomy.
  - `ContentRef { repo: "owner/name", number, url }` is GitHub-shaped; `repo` drives the card's short name and `candidate_repo`.
  - `ContentState { Open, Closed, Merged, Unknown }` and `StateReason { Completed, NotPlanned, Duplicate, Reopened }` are GitHub vocabulary.
  - `ContentFields` holds GitHub-derived built-ins: created/updated/closed, `issue_type`, `parent`, `sub_issues`, `linked_prs`, `content_id` (the node id used for edits), `assignee_ids` (login → node id), `label_ids` (name → node id), `milestone`.
  - `Item::field_value` synthesises the built-in kinds from it. `Item::ref_label()` gives `#12` / `new` / `Draft`.
- **Fields** (`model/field.rs`):
  - `FieldKind`: `Text, Number, Date, SingleSelect{options}, MultiSelect{options}, Iteration{iterations,completed}, Title, Assignees, Labels, Milestone, Repository, LinkedPullRequests, Reviewers, ParentIssue, SubIssuesProgress, IssueType, Tracks, TrackedBy, Created, Updated, Closed, Unsupported{type_name}`.
  - `FieldKind::from_data_type` maps GitHub's `dataType` strings, and `OptionColor::from_api` maps GitHub's 8 colour names.
  - `find_field`/`id_suffix` handle GitHub's `PVTSSF_`/`PVTF_` quirk.
  - Generic-ish: Text, Number, Date, SingleSelect, MultiSelect, Iteration (≈ cycles), Title, Assignees, Labels, Created/Updated/Closed, Parent/SubIssues, `can_be_column`.
  - GitHub-only: Repository, LinkedPullRequests, Reviewers, Milestone (≈ Linear project milestone), IssueType, Tracks/TrackedBy.
- **`FieldValue`**: `Text, Number, Date, SingleSelect{option_id,name}, MultiSelect, Iteration{iteration_id,title,start_date}, Labels(Vec<Label{name, color hex}>), Users(Vec<String> logins), Milestone, Repository, PullRequests, Reviewers, Parent, SubIssues, IssueType`. Users are identified by login.
- **`model/refs.rs`**: `UserRef{id,login}`, `LabelRef{id,name,color}`, `MilestoneRef`, `RepoRef{id,slug}`, `CloseReason{Completed,NotPlanned}`, `ContentId{Issue|PullRequest|Draft}`, `RepoCandidates`, `PushableRepos`, `ContentHit`, and `parse_content_url`, which hard-codes github.com URLs and `owner/name#N`.
- **Project and View** (`model/project.rs`, `model/view.rs`):
  - `Project { id, board, owner_kind, title, url, viewer_can_update, fields, views, linked_repos, issue_repos }`; `owner_kind`, `linked_repos` and `issue_repos` are GitHub-only.
  - `View { id, number, name, layout, filter: String, visible_fields, group_by, vertical_group_by, sort_by }` is generic in shape, but `filter` is a GitHub filter string and `Layout::from_api` maps `BOARD_LAYOUT`/`ROADMAP_LAYOUT`.
- **Mostly generic:** `ItemDetail`/`Comment` (`model/detail.rs`; `LinkedRef` is GitHub-flavoured) and `BoardSnapshot` (`store/snapshot.rs`: `{ version, project, items, order, views: BTreeMap<ViewId, ViewList>, fetched_at }`).

## 3. Ops (`src/ops/mod.rs`, `ops/send.rs`)

`NewItem` is `Draft | Issue{repo: RepoRef,…} | Existing{hit: ContentHit}`. `Confirmed` is `Applied | Item(Item) | Created{temp,item,refused: Vec<(FieldId, GithubError)>} | NotAdded{issue,error}`.

| Op | Confirmed | Sent as | Notes |
|---|---|---|---|
| `SetField` / `ClearField` | `Item` | `updateProjectV2ItemFieldValue` per value type / clear | project-field concept; a move is SetField on the column field |
| `Reorder{item, after}` | `Applied` | `updateProjectV2ItemPosition` | per-project rank; Linear `sortOrder` differs |
| `Assignees{add, remove, all}` | `Applied` | add/remove assignees; drafts set the full list | Linear has a single assignee |
| `Labels{add, remove}` | `Applied` | add/remove labels | repo-scoped label ids |
| `Milestone` | `Applied` | per content type | GitHub-only |
| `Title`, `Body` | `Applied` | Issue/PR/Draft variants (6 ops) | the 3-way `ContentId` split is GitHub-only |
| `Comment` | `Applied` | `addComment` | generic |
| `Close{reason}` / `Reopen` | `Applied` | close/reopen issue or PR | Linear: transition to a completed/canceled state |
| `Convert{item, repo}` | `Item` | `convertProjectV2DraftIssueItemToIssue` | GitHub-only |
| `Archive` / `Unarchive` | `Applied` | project-item archive | the model differs in Linear |
| `Remove` | `Applied` | `deleteProjectV2Item` | "remove from project, keep issue" is GitHub-only |
| `Create{temp, new, presets}` | `Created` / `NotAdded` | draft add; or createIssue then addProjectV2ItemById; or add existing | two-step create and `NotAdded` retry exist because of GitHub |

`Op::apply` (optimistic replay: `move_after`, `place_by_rank`, placeholders) is mostly generic. `ops/queue.rs::OpQueue` runs one op at a time per item, `MAX_IN_FLIGHT = 4` (GitHub's secondary limit), with a `retry_after` pause.

## 4. Capability matrix (`src/model/caps.rs`)

- **Shape:** `OpKind` (15 kinds plus `Create`). `check(kind, item, ctx) -> Result<(), Denial>` and `check_new(needs_repo, ctx)` are the only decision points; `capabilities(item, ctx)` lists what is allowed.
- **`EditContext { viewer_can_update, scopes: Option<Vec<String>>, refused }`:** `has_project_scope` and `has_repo_scope` (which accepts `repo` or `public_repo`).
- **`Denial` variants:** ReadOnly, NoProjectScope, NoRepoScope, Archived, NotArchived, Creating, Private, Unsupported, DraftHasNo, DraftNotClosable, NotADraft, AlreadyClosed, AlreadyOpen, Merged, StateUnknown, NeedsRefresh. `Denial::message` embeds `gh auth refresh -s project|repo`.
- **How scopes reach it:** `GraphqlResponse.scopes` → `Github::scopes()` → `SyncEvent::Access{scopes}` → `App::set_scopes` / `refused_scopes` → `EditContext`. `record_refused_scopes` listens for `GithubError::InsufficientScopes`.
- **Generalising it:**
  - Keep `OpKind` and `check`.
  - Make `EditContext` provider-supplied: board writable, plus static capability flags (has drafts, convert, reorder scope, milestone, close reasons, multi-assignee).
  - Make denial messages provider-neutral (`MissingPermission(..)` plus a remedy text from the provider).
  - Linear has no scope header, so it behaves like "scopes unknown": nothing is blocked up front.

## 5. Sync and polling

- **Jobs:** `SyncJob` (`ui/controller.rs`), `SyncTask` and `SyncEvent` (`sync/mod.rs`).
- **Full load** (`Syncer::full_load`): `FULL_PAGE = 100` pages, capped by `max_items` (default 2000). `ItemsPage` upserts. `ItemsComplete` replaces only once every page has arrived. Then `Access` and `Rate`.
- **Incremental poll:** `incremental_query(INCREMENTAL_MODE, since)` → `"updated:>=<date>"` with `POLL_PAGE = 20`. `INCREMENTAL_MODE = IncrementalMode::Date` (see `docs/phase0-findings.md`). It falls back to a full load when the poll overflows. Linear has a native `updatedAt > ts` filter with datetime precision.
- **View ids** (`Syncer::view_ids`): the view's filter string goes to `items(query:)`; then `ViewIds{view, filter, list}`; then unknown ids are hydrated in batches of 100. Linear would use `customView.issues` (server-side filter) or its own `IssueFilter`.
- **Rate limits:**
  - Primary limit from the `x-ratelimit-*` headers: `RateInfo::is_low` (< 10%) makes `Scheduler::set_low_budget` poll 4× slower.
  - Secondary limit: a 403/429 with `retry-after`, or `RATE_LIMITED`, becomes `GithubError::RateLimited`. Reads respond with `Scheduler::pause_until`, writes with `OpQueue::retry_after`.
  - `items_page_cost` is a dry run used only by the live test.
  - Linear uses complexity-based limits with different headers.
- **`Scheduler`** (`sync/scheduler.rs`): provider-agnostic (`PollConfig`, focus and idle logic).
- **`RECENT_WINDOW`** (5 min, `store/mod.rs`): re-applies confirmed ops and created or converted items over poll data, because GitHub's item index lags writes by about 1 min. Linear is likely read-after-write consistent (unverified), so make the window a provider setting.
- **Cache** (`store/cache.rs`): `CACHE_VERSION = 3`, stored at `state_dir/cache/<BoardRef::key()>.json`. It embeds `Project` and `Item`, so it needs a provider discriminator.

## 6. Filtering

- **View filters:** `View.filter` is a raw GitHub filter string sent server-side.
  - The local `f` filter is appended to it (`App::filter_for`, `ui/app.rs` ~303) and kept in `extra_filter`.
  - Lists fetched with an extra filter are never cached.
  - "Show archived" injects `is:archived`.
- **`model/filter.rs`** reads a little GitHub syntax on the client: single-select and iteration qualifiers, `-field:`, `no:`, `has:`, `is:archived`.
  - `column_constraint` hides board columns the filter excludes (`ui/app.rs` ~429).
  - `still_matches` decides whether an edited sticky item has left the view (`ui/app/edit.rs` ~789).
- **Quick search `/`** (`ui/search.rs`) runs on the client and is provider-neutral.
- **GitHub search syntax:** `sync/mod.rs::search_plan`/`search_query` build `repo:… is:open is:issue` queries (`MAX_QUERY = 256`). The `ui/chrome.rs` hint mentions `repo:owner/name`.

## 7. Identity, config, state, herdr

- **Tokens** (`github/token.rs`): `GH_TOKEN`, then `GITHUB_TOKEN`, then `gh auth token [--user gh_user]` (`TokenSource`). `Token` redacts itself in `Debug`. `NoToken` makes `setup_message` say "run gh auth login". `Effect::ResolveToken` retries.
- **Config** (`src/config.rs`): `poll_interval_secs`, `background_poll_interval_secs`, `idle_after_secs`, `full_refresh_mins`, `max_items`, `placement`, `keys`, `gh_user` (checked by `is_github_login`).
- **State** (`src/state.rs`, `state.json`): `boards_by_repo` (owner/name → owner/number), `last_view` (board → view), `layout_overrides` ("owner/number/viewid" → `{layout, github_layout}`). Every key is a `BoardRef` string.
- **Pane registry** (`herdr/registry.rs`, `panes.json`): keyed by `BoardRef`. This is what enforces one pane per board.
- **Repo detection** (`herdr/repo.rs`):
  - `parse_github_remote` and `detect_repo` (git remote).
  - `open.rs` sets `PB_REPO`/`PB_BOARD`/`PB_PICKER`, and `remembered_board(repo)`.
  - `board_choice::first_step` tries explicit, then picker, then remembered, then the repo's linked boards, then the full list.
  - In the app, `pane_repo` is the default scope for search and for a new issue, and the repo assignees and labels come from.
  - Linear needs another default: a team from config, or one guessed from a branch name.
- **Manifest** (`herdr-plugin.toml`): actions `open`, `open-picker`, `open-overlay`, `open-split`, `open-zoomed`, `doctor --notify`. Provider-neutral apart from the description and `--project OWNER/NUMBER`. CLI in `src/cli.rs`.
- **Doctor** (`commands/doctor.rs`): `scope_check`, `repo_check`, `key_collisions`. All of its messages are GitHub-specific.

## 8. UI assumptions

- **Item references:**
  - `#number` appears in `ui/board.rs` (`card_reference`: glyph plus repo short name), `ui/table.rs` ~136, `ui/detail.rs` (parent, sub-issues, PRs), `ui/markdown.rs` (`#digits` autolinks) and `ui/app/edit.rs` (prompts).
  - Linear identifiers look like `ENG-123`, so a display-id string is needed.
- **Pills and colour:**
  - PR pills live in `ui/pills.rs` (`⑂ #N`, `pr_color`).
  - `OptionColor` is GitHub's 8 colours mapped to ANSI (`ui/theme.rs::option`). Label pills use hex (`ui/labels.rs`).
  - Linear gives hex for states and labels, so a hex option colour is wanted.
- **Links:** `Item::url()` → `Command::OpenUrl` (`is_web_link`) → `open`/`xdg-open`. Neutral.
- **Picker:** `ui/picker.rs` lists `ProjectSummary` by `owner/number` (repo-linked boards first). `B` switches board.
- **Popups** (`ui/app/popups.rs`): the new-item kind menu (Draft / Issue / Existing), the repo picker (pane repo, linked repos, pushable repos), and per-repo candidates for assignees, labels and milestones. Convert and Draft flows don't apply to Linear.
- **Wording:** "GitHub" and `gh auth refresh` appear throughout the UI text.

## 9. Tests and fixtures

- **Recorded fixtures:** `tests/fixtures/recorded/<scenario>/<OperationName>__N.json`, 14 scenarios, loaded by `tests/github_fixtures.rs` through `FixtureTransport::push_file` (keyed by operation name). Recording uses `RecordingTransport` with `PB_RECORD=1`.
- **Inline unit tests:** they use `FixtureTransport` and `github/convert.rs::item_json()`. `src/ui/fixtures.rs` is the shared UI board.
- **Live suites:** `tests/live.rs` and `tests/live_edits.rs` (`--features live`) run against `tests/testbed.toml` (`owner, number, project_id, repo`). Supporting pieces: `tests/common/mod.rs`, `scripts/seed-testbed.sh`, `scripts/update-schema.sh`. `tests/schema.rs` and `tests/manifest.rs` aren't live.
- **A Linear backend would need:** a sandbox Linear workspace with a seed script, a testbed config, Linear's `schema.graphql` and queries, its own recorded fixtures, and live tests.

## 10. Hotspots, ranked by refactor cost

1. **The Item and Op domain is GitHub-shaped:** Issue/PR/Draft/Redacted, `ContentId`, `ContentFields`, about 16 GitHub-shaped `Op` variants. It spans `model/item.rs`, `model/refs.rs`, `ops/`, `model/caps.rs`, and heavily `ui/app/edit.rs` (~4.9k lines) and `ui/app/popups.rs` (~3.4k lines).
2. **Editing is centred on repositories:** `RepoSlug`/`RepoRef`/`ContentRef.repo` drive candidate lookups, the new-issue repo picker, the add-existing search scope, `linked_repos`/`issue_repos` and `pane_repo`. Linear's equivalent is the team (states, labels, members).
3. **Board identity `BoardRef` (owner/number)** runs through the cache path, `state.json`, `panes.json`, the CLI, env vars and the picker. It needs a provider-qualified key.
4. **The filter language** is GitHub syntax, held in `View.filter`, extended by `f`, sent to the server, and read in part by `model/filter.rs` and `search_plan`.
5. **Error, permission and auth model:** `GithubError` leaks through `SyncEvent`, `Confirmed`, the controller and the app (`InsufficientScopes`, `NoToken` with gh remedies). Scopes feed `EditContext`. Doctor, `gh_user` and `setup_message` are GitHub-only.
6. **Sync semantics** (medium cost, mostly in `sync/`, `store/`, `ops/queue.rs`, `github/transport.rs`): `INCREMENTAL_MODE` and `updated:>=`, the page-size cost trick, `RECENT_WINDOW`, view-ids then hydrate, `MAX_IN_FLIGHT = 4`, the rate-limit headers.
7. **Display vocabulary** (cheap but spread wide): `#number`, repo short name, PR glyphs, `OptionColor`, `ContentState::Merged`.
8. **Fixtures and tests:** tied to operation names and `FixtureTransport`. Dozens of `ui/` tests build `GithubError` values.

## Natural seams for a `Provider` trait

Async methods that return model types and a provider-neutral error, taken from what the app calls today:

- **Session and auth:** `viewer()`, permissions/scopes, rate info, credential resolution (replacing `resolve_token_from_system`).
- **Boards:** `resolve_board(BoardRef) -> Project`, `refresh_project`, `list_boards(hint) -> Vec<ProjectSummary>`, optionally `boards_for_context(repo/team)`.
- **Items:** `fetch_items_page`, `fetch_view_ids_page` (or "view items"), `hydrate_items`, `fetch_item_detail`, optionally a cost estimate. Declared behaviour: incremental mode, poll page size, recent window.
- **Mutations:** `send(&Op) -> Confirmed`. This is the existing `ops::send::send` seam and the best single seam, because `Op` is a closed enum of intents.
- **Lookups:** generalise "repo candidates / pushable repos / search" into "assignable users, labels, states and cycles for a container", "containers you can create in", and "search items".
- **Capabilities:** a provider-supplied `EditContext` plus a static capability set consulted by `model::caps::check`, plus a provider for denial and remedy messages.
- **Identity:** a provider id, a board key qualified by provider (cache, state, pane registry), and a filter-dialect hook (`column_constraint`, `still_matches`).

**Cheapest first cut:**
1. Put the current `Github` behind a trait covering the `Syncer` and `ops::send` surface.
2. Make `Transport` provider-neutral, or add a Linear transport (it is GraphQL too).
3. Replace `GithubError` with a neutral error plus a remedy hook.
4. Qualify `BoardRef` with a provider.
