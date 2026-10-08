//! Everything that talks to GitHub. Generated GraphQL types never leave this module.

pub mod client;
pub mod convert;
pub mod error;
pub mod fixture;
pub mod queries;
pub mod token;
pub mod transport;

pub use client::Github;
pub use error::{GithubError, GraphqlErrorEntry};
