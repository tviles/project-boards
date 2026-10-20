//! The provider behind a board. One variant per provider; each arm calls that provider's own
//! client, and a new provider makes every `match` that has no arm for it fail to compile.

use crate::error::BackendError;
use crate::github::Github;
use crate::github::client::{FULL_PAGE, POLL_PAGE};
use crate::github::transport::RateInfo;
use crate::linear::Linear;
use crate::model::Provider;
use crate::sync::{INCREMENTAL_MODE, IncrementalMode};
use std::sync::Arc;

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

/// Linear filters `updatedAt` to the instant, so polls ask only for what changed since.
pub const LINEAR_PROFILE: SyncProfile = SyncProfile {
    full_page: 100,
    poll_page: 50,
    incremental: IncrementalMode::DateTime,
};

pub fn profile_for(provider: Provider) -> SyncProfile {
    match provider {
        Provider::Github => GITHUB_PROFILE,
        Provider::Linear => LINEAR_PROFILE,
    }
}

/// Why a provider's boards cannot be edited, when they cannot.
pub const LINEAR_READ_ONLY: &str = "Editing Linear boards arrives in 0.5";

pub fn read_only_reason(provider: Provider) -> Option<&'static str> {
    match provider {
        Provider::Github => None,
        Provider::Linear => Some(LINEAR_READ_ONLY),
    }
}

pub enum Backend {
    Github(Github),
    Linear(Linear),
}

impl Backend {
    pub fn provider(&self) -> Provider {
        match self {
            Backend::Github(_) => Provider::Github,
            Backend::Linear(_) => Provider::Linear,
        }
    }

    pub fn profile(&self) -> SyncProfile {
        profile_for(self.provider())
    }

    pub fn rate(&self) -> RateInfo {
        match self {
            Backend::Github(gh) => gh.rate(),
            Backend::Linear(linear) => linear.rate(),
        }
    }

    /// Classic token scopes as the last response reported them; `None` when unknown, and
    /// always for Linear, whose keys have none.
    pub fn scopes(&self) -> Option<Vec<String>> {
        match self {
            Backend::Github(gh) => gh.scopes(),
            Backend::Linear(_) => None,
        }
    }
}

/// One backend per provider whose credentials resolved (Decision 17).
#[derive(Clone, Default)]
pub struct Backends {
    pub github: Option<Arc<Backend>>,
    pub linear: Option<Arc<Backend>>,
}

impl Backends {
    /// The provider's backend; `NoCredentials` when its credentials did not resolve.
    pub fn get(&self, provider: Provider) -> Result<Arc<Backend>, BackendError> {
        let backend = match provider {
            Provider::Github => &self.github,
            Provider::Linear => &self.linear,
        };
        backend
            .clone()
            .ok_or(BackendError::NoCredentials { provider })
    }

    /// The providers with a backend, GitHub first.
    pub fn providers(&self) -> Vec<Provider> {
        [Provider::Github, Provider::Linear]
            .into_iter()
            .filter(|p| self.get(*p).is_ok())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::fixture::FixtureTransport;

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

    #[test]
    fn linear_polls_by_datetime_fifty_at_a_time() {
        let b = Backend::Linear(Linear::new(Arc::new(FixtureTransport::new())));
        assert_eq!(b.provider(), Provider::Linear);
        assert_eq!(b.profile(), LINEAR_PROFILE);
        assert_eq!(
            LINEAR_PROFILE,
            SyncProfile {
                full_page: 100,
                poll_page: 50,
                incremental: IncrementalMode::DateTime,
            }
        );
        assert_eq!(profile_for(Provider::Linear), LINEAR_PROFILE);
        assert_eq!(profile_for(Provider::Github), GITHUB_PROFILE);
        assert_eq!(b.scopes(), None, "Linear keys have no scopes");
    }

    #[test]
    fn backends_report_missing_credentials_per_provider() {
        let github = Arc::new(Backend::Github(Github::new(Arc::new(
            FixtureTransport::new(),
        ))));
        let backends = Backends {
            github: Some(github),
            linear: None,
        };
        assert_eq!(backends.providers(), vec![Provider::Github]);
        assert_eq!(
            backends.get(Provider::Github).unwrap().provider(),
            Provider::Github
        );
        assert_eq!(
            backends.get(Provider::Linear).err(),
            Some(BackendError::NoCredentials {
                provider: Provider::Linear
            })
        );
        let none = Backends::default();
        assert!(none.providers().is_empty());
        assert_eq!(
            none.get(Provider::Github).err(),
            Some(BackendError::NoCredentials {
                provider: Provider::Github
            })
        );
    }
}
