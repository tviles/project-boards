# Linear public API for a second project-boards backend (research, 2026-10-14)

Research notes for planning a Linear adapter. Legend: [V] verified from an official source (linear.app/developers pages or Linear's published `schema.graphql`), [S] secondary source, [U] unverified. Re-verify anything marked [U] or [S] before relying on it.

Schema analysed: https://raw.githubusercontent.com/linear/linear/master/packages/sdk/src/schema.graphql (~53k lines).

## 1. API shape

- Endpoint `https://api.linear.app/graphql`; GraphQL only, no REST. Introspection is enabled [V] (https://linear.app/developers/graphql).
- Schema for codegen: `packages/sdk/src/schema.graphql` in github.com/linear/linear, or introspection [V]. Fits this repo's `graphql_client` + checked-in schema pattern.
- Pagination: Relay cursors (`first/after`, `last/before`, `pageInfo { hasNextPage endCursor }`), `edges` or `nodes`; default page 50 [V] (https://linear.app/developers/pagination). Max `first` undocumented (250 is folklore) [U]; complexity is the practical cap.
- Ordering: `orderBy: PaginationOrderBy` = `createdAt` (default) | `updatedAt` only [V]. No arbitrary server sort (`IssueSortInput` is `[INTERNAL]`). Board/list order is client-side via `sortOrder`, `prioritySortOrder`, `subIssueSortOrder`.
- `includeArchived` Boolean on connections, default false [V].
- Filtering: structured `IssueFilter` input [V] (https://linear.app/developers/filtering): comparators `eq/neq/in/nin/lt/lte/gt/gte/null`, string `contains/startsWith/endsWith/eqIgnoreCase`; `and`/`or`; nested relation filters (`assignee`, `labels: { some/every }`, `state: { type: { eq: "started" } }`, `team`, `project`, `cycle`, `parent`, `children`); ISO-8601 durations (`"-P2W"`). No string `rawFilter` [V].
- Search queries: `searchIssues`, `issueSearch`, `semanticSearch` [V, schema].
- Rust crates [S, search snippets only]: `linear-api` 0.1.0 (2026-09-21, hand-written, API-key only, very new), `lineark-sdk` (generated), `linear-schema` + `linear-queries` (cynic), `linear_sdk` 0.0.1 (stale). Recommendation: own `graphql_client` operations against a pinned schema, as the GitHub backend does.

## 2. Auth

- Personal API key: header `Authorization: <API_KEY>` — no `Bearer` [V]. OAuth tokens use `Authorization: Bearer <token>`.
- OAuth2 [V] (https://linear.app/developers/oauth-2-0-authentication): scopes `read`, `write`, `issues:create`, `comments:create`, `timeSchedule:write`, `admin`; `actor=user` (default) or `actor=app`; PKCE supported (client_secret optional with PKCE); access tokens 24h with refresh tokens (migration 2026-04-01); `client_credentials` gives 30-day app-actor tokens. Loopback redirect for a local app unverified [U].
- No official Linear CLI [S]. Community `schpet/linear-cli` keeps keys in the OS keyring [S]. Don't reuse another tool's token.
- Simplest for a TUI: personal API key from `LINEAR_API_KEY` (plus optional keyring); OAuth+PKCE later.

## 3. Rate limits [V] (https://linear.app/developers/rate-limiting)

| | Requests/hour | Complexity points/hour |
|---|---|---|
| API key | 2,500 per user | 3,000,000 |
| OAuth app | 5,000 per user | 2,000,000 |
| Unauthenticated | 600 per IP | 100,000 |

- Max complexity per query: 10,000. Cost: property 0.1, object 1, connections multiply children by `first` (default 50), rounded up.
- Headers: `X-RateLimit-Requests-Limit/Remaining/Reset`, `X-Complexity`, `X-RateLimit-Complexity-Limit/Remaining/Reset`, `X-RateLimit-Endpoint-Requests-*`, `X-RateLimit-Endpoint-Name`.
- Trap [S]: ~260 points per issue with four unbounded nested connections, so ~38 issues per page (https://github.com/linearis-oss/linearis/issues/286). Always bound nested connections (`labels(first: 10)`).
- Webhooks need a public HTTPS non-localhost URL [V] (https://linear.app/developers/webhooks): unusable for a local TUI, so poll.
- 2,500 req/h ≈ 41/min: a poll every 15–30 s with one bounded query is fine.

## 4. Data model [V, schema]

- **Issue**:
  - Identity and text: `id` (UUID), `identifier` ("ENG-123"), `number`, `title`, `description` (markdown), `url`, `branchName`.
  - Planning: `priority` (Float: 0 none, 1 urgent, 2 high, 3 medium, 4 low) with `priorityLabel`, `estimate` (Float), `dueDate` (`TimelessDate`).
  - Organisation: `state` (required WorkflowState), `team` (required), single `assignee`, `delegate`, `creator`, `labels`/`labelIds`, `cycle`, `project`, `projectMilestone`, `parent`/`children`, `relations`/`inverseRelations`, `attachments` (GitHub PRs live here), `comments`, `reactions`, `subscribers`.
  - Ordering and lifecycle: `sortOrder`, `prioritySortOrder`, `subIssueSortOrder` (`boardOrder` deprecated), `archivedAt`, `trashed`, `snoozedUntilAt`, `startedAt`/`completedAt`/`canceledAt`, `updatedAt`.
- **WorkflowState**: `name`, `color`, `position`, `type` (String; in practice triage/backlog/unstarted/started/completed/canceled [U for exact set]). States are per team.
- **Team**: `key`, estimate settings (`issueEstimationType` …), `cyclesEnabled`, `triageEnabled`, `states`, `labels`, `members`.
- **IssueLabel**: `isGroup`, `parent`, `children`, `groupType` (`singleSelect` | `multiSelect`), `color`, optional `team` (null = workspace label). Single-select groups are mutually exclusive.
- **Comment**: threaded (`parent`/`children`), `resolvedAt`, markdown `body` (`bodyData` is internal ProseMirror JSON, never write it).
- **Relations**: blocks, duplicate, related, similar.
- **Custom fields**: none in the schema [V]. No changelog evidence of custom issue fields [S]. Label groups are the usual substitute.
- **Drafts**: issues have no draft state; `teamId` is always required. A `Draft` type exists for something else [U].

## 5. Views [V, schema]

- `CustomView`: `filterData` (JSON, IssueFilter shape), `projectFilterData`, `initiativeFilterData`, `name`, `slugId`, `shared`, nullable `team`, `owner`, `icon`, `color`, `userViewPreferences`/`organizationViewPreferences` (`ViewPreferences`), `viewPreferencesValues`.
- `ViewPreferencesValues` is readable:
  - Layout and grouping: `layout` (String: list/board), `issueGrouping`, `issueSubGrouping`, `issueGroupingLabelGroupId`, `issueSubGroupingLabelGroupId`, `issueNesting`.
  - Columns and groups: `columnOrderBoard`, `columnOrderList`, `hiddenColumns`, `hiddenRows`, `hiddenGroupsList`, `groupOrderingMode`.
  - Visibility toggles: `showEmptyGroups*`, `showSubIssues`, `showCompletedIssues`, `showArchivedItems`, `showSnoozedItems`, `showParents`, `closedIssuesOrderedByRecency`.
  - Field visibility booleans: `fieldAssignee`, `fieldStatus`, `fieldPriority`, `fieldLabels`, `fieldEstimate`, `fieldDueDate`, `fieldCycle`, `fieldProject`, …
  - Unknowns [U]: the String vocabularies (what an assignee grouping looks like) and the issue-ordering key names. Inspect real responses.
- `customView.issues(filter, first, after, includeArchived, includeSubTeams, orderBy)` applies the view's stored filter server-side, so filterData never needs translating; extra `filter` can be ANDed.
- Built-in views (My issues, team default, cycle and project views) are not verified as `CustomView` entities [U]; reproduce them as filters (`assignee: { isMe: { eq: true } }`, `team`, `project`).

## 6. Mutations [V, schema]

- **`issueCreate`**: `IssueCreateInput` requires `teamId`. Optional: `title`, `description`, `stateId`, `assigneeId`, `labelIds`, `priority`, `estimate`, `cycleId`, `projectId`, `projectMilestoneId`, `parentId`, `dueDate`, `sortOrder`, `subscriberIds`, `templateId`.
  - `id` is client-suppliable as a UUID v4, which makes retry-safe creates possible. What happens on a duplicate id is [U].
- **`issueUpdate`**: `stateId`, `assigneeId` (null clears), `labelIds` (replace) or `addedLabelIds`/`removedLabelIds` (delta), `priority`, `estimate`, `cycleId`, `projectId`, `parentId`, `dueDate`, `title`, `description`, `sortOrder`, `subIssueSortOrder`, `prioritySortOrder`, `trashed`, `snoozedUntilAt`, `teamId` (moves the issue).
- **Archive and delete**: `issueArchive` / `issueUnarchive`; `issueDelete` moves the issue to trash.
- **Comments and relations**: `commentCreate` (`issueId`, `body`, `parentId` for a reply, optional `id`); `issueRelationCreate`; also `issueAddLabel` and `issueSubscribe`.
- **Concurrency**: no version or ETag on inputs, so last write wins per field [U].
- **Close and reopen**: no boolean. Set `stateId` to a team state whose `type` is completed or canceled; reopen with an unstarted or started state.

## 7. Change detection

- Delta poll: `issues(filter: { updatedAt: { gt: "<ts>" } }, orderBy: updatedAt, includeArchived: true)` [V for sort-by-updated advice].
- No sync or delta API other than webhooks [U].
- Post-write read lag is undocumented [U]. Mutations return the updated object, so reconcile from that. Expect less lag than GitHub's ~1 min items-index lag, but measure it.

## 8. Identity and URLs

- `url` is shaped `https://linear.app/<workspace>/issue/ENG-123/<slug>`. `identifier` is `team.key` plus the number. `issue(id:)` accepts the identifier [S].
- Desktop deep link: prefix `linear://`, e.g. `linear://linear.app/<workspace>/issue/ENG-123/<slug>` [S]. Fall back to https.

## 9. Differences from GitHub Projects v2

| | GitHub Projects v2 | Linear |
|---|---|---|
| Assignees | many | one (`assignee`; `delegate` is for agents) |
| Status | single-select field per project | per-team WorkflowState with a semantic `type` |
| Fields | arbitrary custom fields | built-ins only (priority, estimate, due date, cycle, project, milestone) plus labels; single-select label groups act like select fields |
| Iterations | iteration field | cycles: per team, optional, auto-rolling |
| Board container | project (board) | team issues, project, or CustomView; the closest analogue to a "board" is a view |
| Priority | custom single-select | first-class, fixed 0–4 |
| Estimates | number field | team-configured scale (fibonacci, linear, exponential, t-shirt) |
| Items | issues, PRs, drafts | issues only; PRs are attachments |
| Manual order | per project (`updateProjectV2ItemPosition`) | global Float `sortOrder`, not per view; reorder = float between neighbours |
| Close | `closeIssue` with reason | move to a completed/canceled-type state |

## 10. Gotchas

- Archived issues are hidden unless `includeArchived: true`. Trashed issues are separate (`trashed`).
- The complexity cap (10,000 per query) is the main limit on page size. Bound every nested connection.
- `dueDate` is a timezone-less date. Do not convert it through UTC.
- `estimate` is a Float on read and an Int on input.
- Markdown: `description`/`body` are markdown, while `*Data` fields are internal ProseMirror. Round trips normalise whitespace. Mention syntax is undocumented [U].
- Rate-limit errors show in the headers and as a GraphQL `RATELIMITED`-style error (exact code [U]). Back off using the headers.
- API keys carry the user's full permissions. `actor=app` OAuth makes the actions appear as an app user.

## Implications for a provider abstraction

1. Capabilities per backend (multi-assignee, custom fields, drafts, status scope, cycles/iterations, estimate scale, manual order scope, PR items) drive what the UI offers, extending today's `model::caps`.
2. A normalised card/item with `assignees: Vec` (Linear: ≤1), status `{id, name, kind, color, order}`, labels with groups, optional priority/estimate/due, parent, url, updated_at, archived. GitHub-only custom fields stay provider-specific.
3. Fields belong to the board: GitHub exposes N project fields; Linear exposes a fixed set plus synthetic select fields from single-select label groups.
4. Views map to `{filter, group_by, sub_group_by, layout, order, visible_fields}`. Linear evaluates filters server-side (`customView.issues`), so there is no need to translate `filterData`.
5. Sync: `updatedAt` delta polling, a per-provider poll interval, header-driven backoff, one bounded query per poll.
6. Writes: client-generated UUIDs for creates, optimistic UI reconciled from mutation results, and close/reopen as "transition to a state of kind X".
7. Manual reorder writes a Float `sortOrder` between neighbours, globally, not per view.
8. Credentials per provider: the env var and keyring differ, and so does the header format (no `Bearer` for Linear keys).
9. Own `graphql_client` operations against a pinned `schema.graphql` rather than the brand-new `linear-api` crate.

## Unverified (check before relying on)

Max `first`; OAuth loopback redirect; rate-limit error code and retry-after; post-write consistency; concurrency semantics; duplicate-`id` create behaviour; `ViewPreferencesValues` string vocabularies and ordering keys; built-in view representation; `linear://` link format; markdown mention syntax; exact WorkflowState type names; crates.io status of the Rust crates.

## Sources

- https://linear.app/developers/graphql
- https://linear.app/developers/rate-limiting
- https://linear.app/developers/pagination
- https://linear.app/developers/filtering
- https://linear.app/developers/oauth-2-0-authentication
- https://linear.app/developers/webhooks
- https://github.com/linear/linear/blob/master/packages/sdk/src/schema.graphql
- https://docs.rs/linear-api/latest/linear_api/
- https://github.com/linearis-oss/linearis/issues/286
- https://github.com/schpet/linear-cli/blob/main/docs/authentication.md
- https://linear.app/docs/get-the-app
- https://github.com/linear/linear/issues/351
- https://linear.app/now/building-what-customers-need
