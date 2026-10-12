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
);
