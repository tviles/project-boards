//! Everything that talks to GitHub. Generated GraphQL types never leave this module.

pub mod client;
pub mod convert;
pub mod error;
pub mod fixture;
pub mod lookups;
pub mod mutations;
pub mod queries;
pub mod record;
pub mod token;
pub mod transport;

pub use crate::error::{BackendError, GraphqlErrorEntry};
pub use client::Github;
