//! Typed operations. graphql_client checks each one against graphql/schema.graphql at
//! compile time and generates its `Variables` type. Responses are decoded in convert.rs,
//! not with the generated types, so unknown union members degrade instead of failing.
#![allow(dead_code, non_camel_case_types, clippy::all)]

use graphql_client::GraphQLQuery;

type URI = String;
type DateTime = String;
type Date = String;

macro_rules! operation {
    ($($name:ident),* $(,)?) => {$(
        #[derive(GraphQLQuery)]
        #[graphql(
            schema_path = "graphql/schema.graphql",
            query_path = "graphql/board.graphql",
            variables_derives = "Debug, Clone"
        )]
        pub struct $name;
    )*};
}

operation!(
    ResolveProject,
    ProjectSchema,
    ItemsPage,
    ItemsPageCost,
    ViewItemIds,
    HydrateItems,
    ItemDetail,
    RepoProjects,
    ViewerProjects,
    Viewer,
    SetTextValue,
    SetNumberValue,
    SetDateValue,
    SetSingleSelectValue,
    SetMultiSelectValue,
    SetIterationValue,
    ClearFieldValue,
    UpdateItemPosition,
    AddDraftIssue,
    DeleteItem,
    AddAssignees,
    RemoveAssignees,
    AddLabels,
    RemoveLabels,
    UpdateIssueTitle,
    UpdateIssueBody,
    SetIssueMilestone,
    UpdatePullRequestTitle,
    UpdatePullRequestBody,
    SetPullRequestMilestone,
    UpdateDraftTitle,
    UpdateDraftBody,
    SetDraftAssignees,
    AddComment,
    CloseIssue,
    ReopenIssue,
    ClosePullRequest,
    ReopenPullRequest,
    AddItemById,
    CreateIssue,
    ConvertDraft,
    ArchiveItem,
    UnarchiveItem,
    RepoCandidates,
    PushableRepos,
    RepoLookup,
    SearchContent,
    ContentByNumber,
);

/// The live suite's cleanup only: release builds do not contain this operation.
#[cfg(feature = "live")]
#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "graphql/schema.graphql",
    query_path = "graphql/cleanup.graphql",
    variables_derives = "Debug, Clone"
)]
pub struct DeleteIssue;

/// Linear operations, validated against graphql/linear/schema.graphql. Phase 0 only for now.
#[cfg(feature = "live")]
pub mod linear {
    use graphql_client::GraphQLQuery;

    type DateTime = String;
    type TimelessDate = String;
    type DateTimeOrDuration = String;

    macro_rules! linear_operation {
        ($($name:ident),* $(,)?) => {$(
            #[derive(GraphQLQuery)]
            #[graphql(
                schema_path = "graphql/linear/schema.graphql",
                query_path = "graphql/linear/testbed.graphql",
                variables_derives = "Debug, Clone"
            )]
            pub struct $name;
        )*};
    }

    linear_operation!(
        TestbedViewer,
        SeedTeams,
        SeedRenameTeam,
        SeedCreateTeam,
        SeedTeamSettings,
        SeedTeamDetails,
        SeedTeamCycleState,
        SeedEnableTeamCycles,
        SeedCycles,
        SeedMoveLabel,
        SeedRenameState,
        SeedLabels,
        SeedCreateLabelGroup,
        SeedCreateChildLabel,
        SeedCreateLooseLabel,
        SeedProjects,
        SeedCreateProject,
        SeedIssues,
        SeedCreateIssue,
        SeedArchiveIssue,
        SeedMarkDuplicate,
        SeedViews,
        SeedCreateTeamBugsView,
        SeedCreateOpenView,
    );
}
