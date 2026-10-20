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

const CLI_FORMS: &str =
    "expected OWNER/NUMBER, linear:team:KEY, linear:project:SLUG or a Linear project URL";

impl BoardKey {
    /// What `--project` accepts: any board key, or a Linear project URL
    /// (`https://linear.app/<workspace>/project/<slug>-<slugId>[/...]`), which names the
    /// project by the slug id after the last `-`. A full slug in `linear:project:<slug>` is
    /// cut to its slug id the same way; a UUID is kept whole.
    pub fn parse_cli(text: &str) -> Result<BoardKey, String> {
        let Some(rest) = text
            .strip_prefix("https://")
            .or_else(|| text.strip_prefix("http://"))
        else {
            return match text.parse()? {
                Self::LinearProject { id } if !is_uuid(&id) && id.contains('-') => id
                    .rsplit_once('-')
                    .and_then(|(_, slug_id)| linear_id(slug_id))
                    .map(|id| Self::LinearProject { id })
                    .ok_or_else(|| CLI_FORMS.to_string()),
                key => Ok(key),
            };
        };
        let rest = rest.split(['?', '#']).next().unwrap_or(rest);
        let mut parts = rest.split('/');
        let (host, _workspace, kind, slug) =
            (parts.next(), parts.next(), parts.next(), parts.next());
        match (host.map(str::to_ascii_lowercase).as_deref(), kind, slug) {
            (Some("linear.app"), Some("project"), Some(slug)) => slug
                .rsplit_once('-')
                .and_then(|(_, id)| linear_id(id))
                .map(|id| Self::LinearProject { id })
                .ok_or_else(|| CLI_FORMS.to_string()),
            _ => Err(CLI_FORMS.to_string()),
        }
    }
}

/// Whether `text` is a UUID in 8-4-4-4-12 hexadecimal form.
pub fn is_uuid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, n)| g.len() == n && g.chars().all(|c| c.is_ascii_hexdigit()))
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

    #[test]
    fn a_linear_project_url_becomes_its_slug_id() {
        for url in [
            "https://linear.app/acme/project/launch-plan-1a2b3c4d5e6f/overview",
            "https://linear.app/acme/project/launch-plan-1a2b3c4d5e6f",
            "http://linear.app/acme/project/launch-plan-1a2b3c4d5e6f/",
            "https://linear.app/acme/project/launch-plan-1a2b3c4d5e6f?utm=x#top",
            "https://linear.app/acme/project/launch-plan-1a2b3c4d5e6f/overview?x",
            "https://linear.app/acme/project/launch-plan-1a2b3c4d5e6f#frag",
            "https://LINEAR.APP/acme/project/launch-plan-1a2b3c4d5e6f",
        ] {
            assert_eq!(
                BoardKey::parse_cli(url),
                Ok(BoardKey::LinearProject {
                    id: "1a2b3c4d5e6f".into()
                }),
                "{url}"
            );
        }
    }

    #[test]
    fn team_keys_and_slugs_parse() {
        assert_eq!(
            BoardKey::parse_cli("linear:team:ENG"),
            Ok(BoardKey::LinearTeam { id: "ENG".into() })
        );
        // A full slug names the project by its slug id, the part after the last `-`, the
        // only part Linear resolves; a slug id or a UUID is kept as given.
        for (given, id) in [
            ("launch-plan-1a2b3c4d5e6f", "1a2b3c4d5e6f"),
            ("1a2b3c4d5e6f", "1a2b3c4d5e6f"),
            (
                "7d0c2f6e-1b7a-4c1e-9a55-0d3c1e2f4a6b",
                "7d0c2f6e-1b7a-4c1e-9a55-0d3c1e2f4a6b",
            ),
        ] {
            assert_eq!(
                BoardKey::parse_cli(&format!("linear:project:{given}")),
                Ok(BoardKey::LinearProject { id: id.into() }),
                "{given}"
            );
        }
        assert!(BoardKey::parse_cli("linear:project:launch-").is_err());
        assert_eq!(
            BoardKey::parse_cli("acme/3"),
            Ok(BoardKey::github("acme", 3))
        );
    }

    #[test]
    fn a_url_from_another_host_is_rejected() {
        let want =
            "expected OWNER/NUMBER, linear:team:KEY, linear:project:SLUG or a Linear project URL";
        for url in [
            "https://github.com/orgs/acme/projects/3",
            "https://example.com/acme/project/x-1a2b",
            "https://linear.app/acme/team/ENG/active",
            "https://linear.app/acme/project/",
            "https://linear.app/acme/project/nodash",
        ] {
            assert_eq!(BoardKey::parse_cli(url), Err(want.to_string()), "{url}");
        }
    }

    #[test]
    fn uuids_are_recognised() {
        assert!(is_uuid("7d0c2f6e-1b7a-4c1e-9a55-0d3c1e2f4a6b"));
        assert!(is_uuid("7D0C2F6E-1B7A-4C1E-9A55-0D3C1E2F4A6B"));
        for bad in [
            "",
            "ENG",
            "launch-plan-1a2b3c4d5e6f",
            "7d0c2f6e-1b7a-4c1e-9a55-0d3c1e2f4a6",
            "7d0c2f6e-1b7a-4c1e-9a55-0d3c1e2f4a6bb",
            "7d0c2f6e-1b7a-4c1e-9a55-0d3c1e2f4a6g",
        ] {
            assert!(!is_uuid(bad), "{bad}");
        }
    }
}
