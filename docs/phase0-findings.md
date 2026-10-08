# Phase 0 findings (2026-10-08)

| # | Question | Answer | Consequence |
|---|---|---|---|
| 0 | Index lag after a write | A new draft stayed invisible for more than 73 s in all three item-list forms (`query: ""`, no `query`, `orderBy` POSITION) | Changes made by others appear a minute or more late. Input to the 0.2 plan: mutation responses must update the confirmed layer directly, and confirmed edits must outrank stale poll results for several minutes. 0.1 is read-only: no code change |
| 1 | `updated:>=` in `items(query:)` | Date only: `updated:>=<datetime>` returns 0; `updated:>=<date>` works | INCREMENTAL_MODE = Date |
| 2 | Web filter syntax parity | Every check matches except `is:issue` (server 12, client 10: it also counts draft issues) | Documented; evaluated client-side in a later milestone |
| 3 | View filter round trip; group-by/sort writable | Filter round trip exact. `UpdateProjectV2ViewInput` has no group field (awk check printed `0`); group-by/sort read-back was empty but inconclusive, because the manual view settings had not been applied | Local overrides as designed. Re-check read-back when fixtures are recorded in Task 10, after the settings are applied |
| 4 | Mouse drag reaches a tab pane | Yes (tab log: 11 Down, 129 Drag, 11 Up). Not exercised in the overlay (no clicks there) | Drag in 0.4 as designed |
| 5 | Focus events reach plugin panes | Yes (FocusGained/FocusLost in both logs) | FOCUS_EVENTS_EXPECTED = yes |
| 6 | Esc in overlay | Reaches the plugin (4 Esc key events in the overlay log). The overlay looked like a second tab to Tyler | Documented in README |
| 7 | Oldest herdr with everything used | 0.7.0 (docs and source reading, not run on 0.7.0). Ruling: keep `min_herdr_version = "0.9.0"` (focus-event fix landed in 0.9.1) | manifest unchanged |
| 8 | Pane closes when its process exits | Left open: after q the screen cleared and the pane stayed | The pane must close itself (Task 25 as planned); README wording |

## herdr version evidence (probe 7)

Repo moved to https://github.com/herdrdev/herdr. v0.6.10 has no `src/cli/plugin.rs` (404). v0.7.0 notes (https://github.com/herdrdev/herdr/releases/tag/v0.7.0): "Added local plugin v1 support ... managed plugin panes ..." and "plugin pane placement, plugin invocation context/env injection". v0.7.0 `src/cli/plugin.rs` parses placements overlay, split, tab, zoomed and has open/focus/close; `src/cli/pane.rs` has `pane get <pane_id>`; docs list `HERDR_PLUGIN_CONTEXT_JSON` and "any available `HERDR_WORKSPACE_ID`, `HERDR_TAB_ID`, and `HERDR_PANE_ID`". Argument-less `pane get` resolving the calling pane arrived in 0.8.2 (#2297, #2298); manual pane navigation emits focus events again from 0.9.1.

## Terminal probe evidence (herdr 0.9.3)

| Log | Mouse events | Down / Drag / Up | FocusGained / FocusLost | Esc |
|---|---|---|---|---|
| probe-tab.log | 264 | 11 / 129 / 11 | 5 / 5 | 0 (not pressed) |
| probe-overlay.log | 368 | 0 / 0 / 0 | 5 / 2 | 4 |

Tyler's observations (probe-observations.md): the overlay opened looking like a second tab; Esc registered as a keypress; after q the pane stayed open (screen cleared, nothing closed).

## API probes (2026-10-06 run 2)

## 0. Index lag

- t+0s [0 query:""]: 13 items, new id present: false
- t+0s [0b no query]: 13 items, new id present: false
- t+0s [0b no query, orderBy POSITION]: 13 items, new id present: false
- t+6s [0 query:""]: 13 items, new id present: false
- t+6s [0b no query]: 13 items, new id present: false
- t+6s [0b no query, orderBy POSITION]: 13 items, new id present: false
- t+12s [0 query:""]: 13 items, new id present: false
- t+12s [0b no query]: 13 items, new id present: false
- t+12s [0b no query, orderBy POSITION]: 13 items, new id present: false
- t+18s [0 query:""]: 13 items, new id present: false
- t+18s [0b no query]: 13 items, new id present: false
- t+18s [0b no query, orderBy POSITION]: 13 items, new id present: false
- t+24s [0 query:""]: 13 items, new id present: false
- t+24s [0b no query]: 13 items, new id present: false
- t+24s [0b no query, orderBy POSITION]: 13 items, new id present: false
- t+30s [0 query:""]: 13 items, new id present: false
- t+30s [0b no query]: 13 items, new id present: false
- t+30s [0b no query, orderBy POSITION]: 13 items, new id present: false
- t+36s [0 query:""]: 13 items, new id present: false
- t+36s [0b no query]: 13 items, new id present: false
- t+36s [0b no query, orderBy POSITION]: 13 items, new id present: false
- t+42s [0 query:""]: 13 items, new id present: false
- t+42s [0b no query]: 13 items, new id present: false
- t+42s [0b no query, orderBy POSITION]: 13 items, new id present: false
- t+48s [0 query:""]: 13 items, new id present: false
- t+48s [0b no query]: 13 items, new id present: false
- t+48s [0b no query, orderBy POSITION]: 13 items, new id present: false
- t+55s [0 query:""]: 13 items, new id present: false
- t+55s [0b no query]: 13 items, new id present: false
- t+55s [0b no query, orderBy POSITION]: 13 items, new id present: false
- t+61s [0 query:""]: 13 items, new id present: false
- t+61s [0b no query]: 13 items, new id present: false
- t+61s [0b no query, orderBy POSITION]: 13 items, new id present: false
- t+67s [0 query:""]: 13 items, new id present: false
- t+67s [0b no query]: 13 items, new id present: false
- t+67s [0b no query, orderBy POSITION]: 13 items, new id present: false
- t+73s [0 query:""]: 13 items, new id present: false
- t+73s [0b no query]: 13 items, new id present: false
- t+73s [0b no query, orderBy POSITION]: 13 items, new id present: false

- [0 query:""] NOT visible within 60s
- [0b no query] NOT visible within 60s
- [0b no query, orderBy POSITION] NOT visible within 60s

Probe item deleted.

All items: 13

## 1. Incremental filter

Pivot 2026-10-06T16:12:06Z; items updated at or after it: 7

- `updated:>=2026-10-06T16:12:06Z` → 0 items (datetime exact: false)
- `updated:>=2026-10-06` → 13 items (datetime exact: false)

## 2. Filter syntax

- `label:bug` → server 1 / expected 1 ✓
- `-label:bug` → server 12 / expected 12 ✓
- `label:bug,docs` → server 2 / expected 2 ✓
- `status:Todo` → server 8 / expected 8 ✓
- `status:"In Progress"` → server 4 / expected 4 ✓
- `assignee:@me` → server 2 / expected 2 ✓
- `no:assignee` → server 11 / expected 11 ✓
- `is:issue` → server 12 / expected 10 ✗
- `is:draft` → server 2 / expected 2 ✓

## 3. Views

- "View 1" ("TABLE_LAYOUT"): filter Null, groupBy [], columnBy [], sortBy []
- "Board" ("BOARD_LAYOUT"): filter Null, groupBy [], columnBy [{"name":"Status"}], sortBy []
- "Bugs" ("TABLE_LAYOUT"): filter String("label:bug"), groupBy [], columnBy [], sortBy []
- "Probe" ("TABLE_LAYOUT"): filter String("label:bug -status:Done"), groupBy [], columnBy [], sortBy []

Filter write returned String("label:bug -status:Done") (round trip exact: true)

Group-by and sort are not in UpdateProjectV2ViewInput in schema 0.1.0-dev; confirm by checking the schema copy for `groupBy` inside that input.
