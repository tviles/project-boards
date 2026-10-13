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
];

#[test]
fn schema_has_everything_we_use() {
    let schema =
        std::fs::read_to_string("crates/queries/graphql/schema.graphql").expect("schema present");
    for needle in REQUIRED {
        assert!(schema.contains(needle), "schema is missing `{needle}`");
    }
}
