//! Everything that talks to GitHub. Generated GraphQL types never leave this module.

pub mod error;
pub mod fixture;
pub mod token;
pub mod transport;

pub use error::{GithubError, GraphqlErrorEntry};
