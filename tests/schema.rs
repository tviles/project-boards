//! Fails loudly if the checked-in schema lacks a type or field the plugin relies on.

const REQUIRED: &[&str] = &[
    "type ProjectV2 implements",
    "type ProjectV2View implements",
    "type ProjectV2Item implements",
    "union ProjectV2ItemFieldValue",
    "union ProjectV2FieldConfiguration",
    "union ProjectV2ItemContent",
    "type ProjectV2SingleSelectField implements",
    "type ProjectV2MultiSelectField implements",
    "type ProjectV2IterationField implements",
    "enum ProjectV2ViewLayout",
    "verticalGroupByFields(",
    "sortByFields(",
    "query: String = \"\"",
    "closedByPullRequestsReferences(",
    "subIssues(",
    "addProjectV2DraftIssue(",
    "deleteProjectV2Item(",
    "subIssuesSummary: SubIssuesSummary",
    "issueType: IssueType",
    "stateReason: IssueStateReason",
    "updateProjectV2ItemFieldValue(",
    "clearProjectV2ItemFieldValue(",
    "updateProjectV2ItemPosition(",
    "multiSelectOptionIds: [String!]",
    "addAssigneesToAssignable(",
    "removeAssigneesFromAssignable(",
    "addLabelsToLabelable(",
    "removeLabelsFromLabelable(",
    "updateIssue(",
    "updatePullRequest(",
    "updateProjectV2DraftIssue(",
    "addComment(",
    "closeIssue(",
    "reopenIssue(",
    "closePullRequest(",
    "reopenPullRequest(",
    "enum IssueClosedStateReason",
    "addProjectV2ItemById(",
    "createIssue(",
    "convertProjectV2DraftIssueItemToIssue(",
    "archiveProjectV2Item(",
    "unarchiveProjectV2Item(",
    "deleteIssue(",
    "assignableUsers(",
    "issueOrPullRequest(",
    "viewerPermission: RepositoryPermission",
];

#[test]
fn schema_has_everything_we_use() {
    let schema =
        std::fs::read_to_string("crates/queries/graphql/schema.graphql").expect("schema present");
    for needle in REQUIRED {
        assert!(schema.contains(needle), "schema is missing `{needle}`");
    }
}

/// What the Linear backend relies on in Linear's published schema (spec section 9).
const LINEAR_REQUIRED: &[&str] = &[
    "type Issue implements",
    "identifier: String!",
    "type CustomView implements",
    "type ViewPreferencesValues",
    "type WorkflowState implements",
    "enum LabelGroupType",
    "enum PaginationOrderBy",
    "type IssueConnection",
    "issueCreate(",
    "issueUpdate(",
    "customViews(",
    "rateLimitStatus: RateLimitPayload!",
    "facets: [Facet!]!",
    "targetCustomView: CustomView",
    "groupType: LabelGroupType",
    "issueGroupingLabelGroupId: String",
    "issueSubGroupingLabelGroupId: String",
    "includeArchived: Boolean",
    "trashed: Boolean",
];

#[test]
fn linear_schema_has_everything_we_use() {
    let schema = std::fs::read_to_string("crates/queries/graphql/linear/schema.graphql")
        .expect("Linear schema present");
    for needle in LINEAR_REQUIRED {
        assert!(
            schema.contains(needle),
            "Linear schema is missing `{needle}`"
        );
    }
}

#[test]
fn linear_operations_expose_names_and_fragments() {
    use project_boards_queries::linear::*;
    assert_eq!(resolve_project_board::OPERATION_NAME, "ResolveProjectBoard");
    assert_eq!(resolve_team_board::OPERATION_NAME, "ResolveTeamBoard");
    assert_eq!(team_by_key::OPERATION_NAME, "TeamByKey");
    assert_eq!(board_schema::OPERATION_NAME, "BoardSchema");
    assert_eq!(team_projects::OPERATION_NAME, "TeamProjects");
    assert_eq!(board_issues::OPERATION_NAME, "BoardIssues");
    assert_eq!(poll_issues::OPERATION_NAME, "PollIssues");
    assert_eq!(issue_ids::OPERATION_NAME, "IssueIds");
    assert_eq!(custom_view_issue_ids::OPERATION_NAME, "CustomViewIssueIds");
    assert_eq!(issues_by_id::OPERATION_NAME, "IssuesById");
    assert_eq!(issue_detail::OPERATION_NAME, "IssueDetail");
    assert_eq!(list_boards::OPERATION_NAME, "ListBoards");
    assert_eq!(linear_viewer::OPERATION_NAME, "LinearViewer");
    for q in [board_issues::QUERY, poll_issues::QUERY, issues_by_id::QUERY] {
        assert!(q.contains("fragment IssueFields"));
    }
    for q in [resolve_project_board::QUERY, board_schema::QUERY] {
        assert!(q.contains("fragment ViewFields"));
    }
}
