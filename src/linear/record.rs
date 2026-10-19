//! Scrubs recorded Linear responses so no real person or workspace reaches a fixture.
//! Decision 15: people (user-like objects) and the workspace are replaced; teams, states,
//! labels and the like are seeded inventions and stay.

use crate::transport::Transport;
use crate::transport::record::RecordingTransport;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Invented full names, handed out by order of first appearance (wrapping after eight).
pub const INVENTED_PEOPLE: [&str; 8] = [
    "Avery Quill",
    "Blair Fenwick",
    "Casey Thornbury",
    "Devon Marsh",
    "Emery Locke",
    "Finley Ashgrove",
    "Gray Holloway",
    "Harper Wynn",
];

const WORKSPACE_NAME: &str = "Testbed Workspace";
const WORKSPACE_KEY: &str = "testbed-workspace";

pub struct Scrubber {
    url_key: String,
    /// First-appearance index per person, keyed by `id` (or the real name without one).
    people: HashMap<String, usize>,
    /// Every real value replaced so far, for `check`.
    replaced: Vec<String>,
}

impl Scrubber {
    pub fn new(url_key: &str) -> Self {
        Self {
            url_key: url_key.to_string(),
            people: HashMap::new(),
            replaced: Vec::new(),
        }
    }

    pub fn scrub(&mut self, v: &mut Value) {
        match v {
            Value::Object(map) => {
                if ["displayName", "email", "avatarUrl"]
                    .iter()
                    .any(|k| map.contains_key(*k))
                {
                    self.scrub_person(map);
                }
                if let Some(Value::Object(org)) = map.get_mut("organization")
                    && let Some(name) = org.get_mut("name")
                {
                    self.replace(name, WORKSPACE_NAME);
                }
                for child in map.values_mut() {
                    self.scrub(child);
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|i| self.scrub(i)),
            Value::String(s) if !self.url_key.is_empty() && s.contains(&self.url_key) => {
                *s = s.replace(&self.url_key, WORKSPACE_KEY);
            }
            _ => {}
        }
    }

    fn scrub_person(&mut self, map: &mut serde_json::Map<String, Value>) {
        let key = map
            .get("id")
            .or_else(|| map.get("name"))
            .and_then(Value::as_str)
            .map(String::from);
        let next = self.people.len();
        let i = match key {
            Some(k) => *self.people.entry(k).or_insert(next),
            None => next,
        };
        let fake_name = INVENTED_PEOPLE[i % INVENTED_PEOPLE.len()];
        let fake_email = format!("person{i}@example.com");
        for (field, fake) in [
            ("name", Some(fake_name)),
            ("displayName", Some(fake_name)),
            ("email", Some(fake_email.as_str())),
            ("avatarUrl", None),
        ] {
            if let Some(value) = map.get_mut(field) {
                match fake {
                    Some(fake) => self.replace(value, fake),
                    None => self.remember_and_clear(value),
                }
            }
        }
    }

    fn replace(&mut self, value: &mut Value, fake: &str) {
        if let Value::String(real) = value {
            if real != fake && !real.is_empty() {
                self.replaced.push(real.clone());
            }
            *value = Value::String(fake.to_string());
        }
    }

    fn remember_and_clear(&mut self, value: &mut Value) {
        if let Value::String(real) = value
            && !real.is_empty()
        {
            self.replaced.push(real.clone());
        }
        *value = Value::Null;
    }

    /// Fails when the text still holds a value that was replaced. The message names none.
    pub fn check(&self, written: &str) -> Result<(), String> {
        let leaked = self.replaced.iter().any(|v| written.contains(v.as_str()))
            || (!self.url_key.is_empty() && written.contains(&self.url_key));
        if leaked {
            Err("a recorded response still holds a value the scrubber replaced; nothing was written".into())
        } else {
            Ok(())
        }
    }
}

/// A recorder that scrubs every response and refuses to write one that still leaks.
pub fn scrubbing_recorder(
    inner: Arc<dyn Transport>,
    dir: PathBuf,
    url_key: &str,
) -> RecordingTransport {
    let scrubber = Mutex::new(Scrubber::new(url_key));
    RecordingTransport::with_filter(
        inner,
        dir,
        Box::new(move |body| {
            let mut scrubber = scrubber.lock().unwrap();
            scrubber.scrub(body);
            let text = serde_json::to_string_pretty(body).map_err(|e| e.to_string())?;
            scrubber.check(&text)
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn recorded() -> Value {
        json!({"data": {
        "organization": {"name": "Real Corp Inc", "urlKey": "project-boards-linear-testbed"},
        "issues": {"nodes": [
            {"identifier": "TBA-1",
             "url": "https://linear.app/project-boards-linear-testbed/issue/TBA-1",
             "state": {"name": "Todo"}, "team": {"name": "Alpha"},
             "assignee": {"id": "u1", "name": "Real Person", "displayName": "real",
                          "email": "real@corp.test", "avatarUrl": "https://img.test/real.png"}},
            {"identifier": "TBA-2",
             "assignee": {"id": "u1", "name": "Real Person", "displayName": "real",
                          "email": "real@corp.test", "avatarUrl": "https://img.test/real.png"},
             "comments": {"nodes": [{"user": {"id": "u2", "name": "Other Human",
                          "displayName": "other", "email": "other@corp.test",
                          "avatarUrl": null}}]}}
        ]}}})
    }

    #[test]
    fn people_and_the_workspace_are_replaced_consistently() {
        let mut v = recorded();
        Scrubber::new("project-boards-linear-testbed").scrub(&mut v);
        let issues = &v["data"]["issues"]["nodes"];
        for a in [&issues[0]["assignee"], &issues[1]["assignee"]] {
            assert_eq!(a["name"], INVENTED_PEOPLE[0]);
            assert_eq!(a["displayName"], INVENTED_PEOPLE[0]);
            assert_eq!(a["email"], "person0@example.com");
            assert!(a["avatarUrl"].is_null());
            assert_eq!(a["id"], "u1");
        }
        let c = &issues[1]["comments"]["nodes"][0]["user"];
        assert_eq!(c["name"], INVENTED_PEOPLE[1]);
        assert_eq!(c["email"], "person1@example.com");
        assert_eq!(issues[0]["state"]["name"], "Todo");
        assert_eq!(issues[0]["team"]["name"], "Alpha");
        assert_eq!(v["data"]["organization"]["name"], "Testbed Workspace");
        assert_eq!(
            issues[0]["url"],
            "https://linear.app/testbed-workspace/issue/TBA-1"
        );
        assert_eq!(v["data"]["organization"]["urlKey"], "testbed-workspace");
    }

    #[test]
    fn a_person_without_an_id_is_keyed_by_their_real_name() {
        let mut v = json!([
            {"name": "Real Person", "email": "a@corp.test"},
            {"name": "Real Person", "email": "a@corp.test"},
            {"name": "Someone Else", "email": "b@corp.test"}
        ]);
        Scrubber::new("example-key").scrub(&mut v);
        assert_eq!(v[0]["name"], INVENTED_PEOPLE[0]);
        assert_eq!(v[1]["name"], INVENTED_PEOPLE[0]);
        assert_eq!(v[2]["name"], INVENTED_PEOPLE[1]);
    }

    #[test]
    fn check_passes_a_clean_file_and_fails_without_naming_the_value() {
        let mut s = Scrubber::new("project-boards-linear-testbed");
        s.scrub(&mut recorded());
        assert!(s.check(r#"{"name": "Invented"}"#).is_ok());
        let err = s
            .check(r#"{"body": "ask Real Person about it"}"#)
            .unwrap_err();
        assert!(!err.contains("Real Person"), "{err}");
        let err = s
            .check("see linear.app/project-boards-linear-testbed")
            .unwrap_err();
        assert!(!err.contains("project-boards-linear-testbed"), "{err}");
    }
}
