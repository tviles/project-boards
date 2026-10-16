//! Which service a board lives on.

use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Github,
    Linear,
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Provider::Github => "GitHub",
            Provider::Linear => "Linear",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_show_their_product_names_and_store_lowercase() {
        assert_eq!(Provider::Github.to_string(), "GitHub");
        assert_eq!(Provider::Linear.to_string(), "Linear");
        assert_eq!(
            serde_json::to_string(&Provider::Linear).unwrap(),
            "\"linear\""
        );
        assert_eq!(
            serde_json::from_str::<Provider>("\"github\"").unwrap(),
            Provider::Github
        );
    }
}
