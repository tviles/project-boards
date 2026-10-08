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

/// A board as users name it: `owner/number`, e.g. `tviles/3`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BoardRef {
    pub owner: String,
    pub number: u32,
}

impl BoardRef {
    /// A filesystem-safe key, e.g. `tviles__3`.
    pub fn key(&self) -> String {
        format!("{}__{}", self.owner, self.number)
    }
}

impl fmt::Display for BoardRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.number)
    }
}

impl FromStr for BoardRef {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bad = || format!("expected OWNER/NUMBER, got `{s}`");
        let (owner, number) = s.split_once('/').ok_or_else(bad)?;
        let number: u32 = number.parse().map_err(|_| bad())?;
        if owner.is_empty() || owner.contains('/') || number == 0 {
            return Err(bad());
        }
        Ok(Self {
            owner: owner.to_string(),
            number,
        })
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

    #[test]
    fn board_ref_round_trips() {
        let b: BoardRef = "tviles/3".parse().unwrap();
        assert_eq!(
            b,
            BoardRef {
                owner: "tviles".into(),
                number: 3
            }
        );
        assert_eq!(b.to_string(), "tviles/3");
        assert_eq!(b.key(), "tviles__3");
    }

    #[test]
    fn board_ref_rejects_bad_input() {
        for bad in ["tviles", "tviles/x", "/3", "a/b/3", "tviles/0", "tviles/-1"] {
            assert!(bad.parse::<BoardRef>().is_err(), "{bad} should not parse");
        }
    }

    #[test]
    fn repo_slug_round_trips() {
        let r: RepoSlug = "tviles/project-boards".parse().unwrap();
        assert_eq!(r.to_string(), "tviles/project-boards");
        assert!("tviles".parse::<RepoSlug>().is_err());
        assert!("a/b/c".parse::<RepoSlug>().is_err());
    }
}
