//! The provider behind a board. One variant per provider; each arm calls that provider's own
//! client, and a new provider makes every `match` that has no arm for it fail to compile.

use crate::github::Github;
use crate::github::client::{FULL_PAGE, POLL_PAGE};
use crate::github::transport::RateInfo;
use crate::model::Provider;
use crate::sync::{INCREMENTAL_MODE, IncrementalMode};

/// How a provider is loaded and polled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncProfile {
    /// Items per page of a full load.
    pub full_page: u32,
    /// Items per page of an incremental poll.
    pub poll_page: u32,
    pub incremental: IncrementalMode,
}

pub const GITHUB_PROFILE: SyncProfile = SyncProfile {
    full_page: FULL_PAGE,
    poll_page: POLL_PAGE,
    incremental: INCREMENTAL_MODE,
};

pub enum Backend {
    Github(Github),
}

impl Backend {
    pub fn provider(&self) -> Provider {
        match self {
            Backend::Github(_) => Provider::Github,
        }
    }

    pub fn profile(&self) -> SyncProfile {
        match self {
            Backend::Github(_) => GITHUB_PROFILE,
        }
    }

    pub fn rate(&self) -> RateInfo {
        match self {
            Backend::Github(gh) => gh.rate(),
        }
    }

    /// Classic token scopes as the last response reported them; `None` when unknown.
    pub fn scopes(&self) -> Option<Vec<String>> {
        match self {
            Backend::Github(gh) => gh.scopes(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::fixture::FixtureTransport;
    use std::sync::Arc;

    #[test]
    fn github_keeps_its_page_sizes_and_incremental_mode() {
        let b = Backend::Github(Github::new(Arc::new(FixtureTransport::new())));
        assert_eq!(b.provider(), Provider::Github);
        assert_eq!(
            b.profile(),
            SyncProfile {
                full_page: 100,
                poll_page: 20,
                incremental: IncrementalMode::Date,
            }
        );
    }
}
