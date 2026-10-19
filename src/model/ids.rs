use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

macro_rules! string_id {
    ($($name:ident),* $(,)?) => {$(
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    )*};
}

string_id!(ProjectId, ItemId, FieldId, ViewId, OptionId, IterationId);

/// A GitHub board as users name it: `owner/number`, e.g. `acme/3`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GithubBoard {
    pub owner: String,
    pub number: u32,
}

impl fmt::Display for GithubBoard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.number)
    }
}

/// Any board. GitHub's is written `owner/number` as it always was, so state written by
/// earlier versions still parses; Linear's is `linear:project:<id>` or `linear:team:<id>`,
/// by UUID because team keys and project names can be renamed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub enum BoardKey {
    Github(GithubBoard),
    LinearProject { id: String },
    LinearTeam { id: String },
}

impl BoardKey {
    pub fn github(owner: impl Into<String>, number: u32) -> Self {
        Self::Github(GithubBoard {
            owner: owner.into(),
            number,
        })
    }

    pub fn provider(&self) -> crate::model::Provider {
        match self {
            Self::Github(_) => crate::model::Provider::Github,
            Self::LinearProject { .. } | Self::LinearTeam { .. } => crate::model::Provider::Linear,
        }
    }

    pub fn as_github(&self) -> Option<&GithubBoard> {
        match self {
            Self::Github(b) => Some(b),
            _ => None,
        }
    }

    /// A file-system-safe key, e.g. `acme__3` or `linear__team__<id>`.
    pub fn key(&self) -> String {
        match self {
            Self::Github(b) => format!("{}__{}", b.owner, b.number),
            Self::LinearProject { id } => format!("linear__project__{id}"),
            Self::LinearTeam { id } => format!("linear__team__{id}"),
        }
    }
}

impl fmt::Display for BoardKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Github(b) => b.fmt(f),
            Self::LinearProject { id } => write!(f, "linear:project:{id}"),
            Self::LinearTeam { id } => write!(f, "linear:team:{id}"),
        }
    }
}

/// A Linear id as it may appear in a key: letters, digits and `-` (UUIDs, and team keys
/// until plan 2 resolves those to ids).
fn linear_id(id: &str) -> Option<String> {
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .then(|| id.to_string())
}

impl FromStr for BoardKey {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(rest) = s.strip_prefix("linear:") {
            let bad = || format!("expected linear:project:ID or linear:team:ID, got `{s}`");
            return match rest.split_once(':') {
                Some(("project", id)) => linear_id(id)
                    .map(|id| Self::LinearProject { id })
                    .ok_or_else(bad),
                Some(("team", id)) => linear_id(id)
                    .map(|id| Self::LinearTeam { id })
                    .ok_or_else(bad),
                _ => Err(bad()),
            };
        }
        let bad = || format!("expected OWNER/NUMBER, got `{s}`");
        let (owner, number) = s.split_once('/').ok_or_else(bad)?;
        let number: u32 = number.parse().map_err(|_| bad())?;
        if owner.is_empty() || owner.contains('/') || number == 0 {
            return Err(bad());
        }
        Ok(Self::github(owner, number))
    }
}

impl From<BoardKey> for String {
    fn from(b: BoardKey) -> String {
        b.to_string()
    }
}

impl TryFrom<String> for BoardKey {
    type Error = String;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

/// A repository, `owner/name`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RepoSlug {
    pub owner: String,
    pub name: String,
}

impl fmt::Display for RepoSlug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

impl FromStr for RepoSlug {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.split_once('/') {
            Some((owner, name)) if !owner.is_empty() && !name.is_empty() && !name.contains('/') => {
                Ok(Self {
                    owner: owner.to_string(),
                    name: name.to_string(),
                })
            }
            _ => Err(format!("expected OWNER/NAME, got `{s}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Provider;

    #[test]
    fn github_keys_keep_their_form() {
        let b: BoardKey = "acme/3".parse().unwrap();
        assert_eq!(b, BoardKey::github("acme", 3));
        assert_eq!(b.to_string(), "acme/3");
        assert_eq!(b.key(), "acme__3");
        assert_eq!(b.provider(), Provider::Github);
        assert_eq!(b.as_github().map(|g| g.number), Some(3));
    }

    #[test]
    fn linear_keys_round_trip() {
        let id = "7d0c2f6e-1b7a-4c1e-9a55-0d3c1e2f4a6b";
        for (text, key, provider) in [
            (
                format!("linear:project:{id}"),
                format!("linear__project__{id}"),
                Provider::Linear,
            ),
            (
                format!("linear:team:{id}"),
                format!("linear__team__{id}"),
                Provider::Linear,
            ),
        ] {
            let b: BoardKey = text.parse().unwrap();
            assert_eq!(b.to_string(), text);
            assert_eq!(b.key(), key);
            assert_eq!(b.provider(), provider);
            assert_eq!(b.as_github(), None);
        }
    }

    #[test]
    fn an_owner_called_linear_is_still_github() {
        assert_eq!(
            "linear/5".parse::<BoardKey>(),
            Ok(BoardKey::github("linear", 5))
        );
        assert_eq!(
            "linear-app/2".parse::<BoardKey>(),
            Ok(BoardKey::github("linear-app", 2))
        );
    }

    #[test]
    fn bad_keys_are_rejected() {
        for bad in [
            "acme",
            "acme/x",
            "/3",
            "a/b/3",
            "acme/0",
            "acme/-1",
            "linear:",
            "linear:project:",
            "linear:cycle:x",
            "linear:team:a/b",
            "linear:team:a:b",
            "linear:team: x",
        ] {
            assert!(bad.parse::<BoardKey>().is_err(), "{bad} should not parse");
        }
    }

    #[test]
    fn board_keys_serialize_as_their_string() {
        let b = BoardKey::github("acme", 3);
        assert_eq!(serde_json::to_string(&b).unwrap(), "\"acme/3\"");
        assert_eq!(serde_json::from_str::<BoardKey>("\"acme/3\"").unwrap(), b);
        let t: BoardKey = "linear:team:t1".parse().unwrap();
        assert_eq!(
            serde_json::from_str::<BoardKey>("\"linear:team:t1\"").unwrap(),
            t
        );
        assert!(serde_json::from_str::<BoardKey>("\"nope\"").is_err());
    }

    #[test]
    fn repo_slug_round_trips() {
        let r: RepoSlug = "tviles/project-boards".parse().unwrap();
        assert_eq!(r.to_string(), "tviles/project-boards");
        assert!("tviles".parse::<RepoSlug>().is_err());
        assert!("a/b/c".parse::<RepoSlug>().is_err());
    }
}
