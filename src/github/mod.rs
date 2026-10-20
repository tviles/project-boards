//! Everything that talks to GitHub. Generated GraphQL types never leave this module.

pub mod client;
pub mod convert;
pub mod error;
pub mod lookups;
pub mod mutations;
pub mod queries;
pub mod token;

pub use crate::error::{BackendError, GraphqlErrorEntry};
pub use crate::transport;
pub use crate::transport::{fixture, record};
pub use client::Github;
