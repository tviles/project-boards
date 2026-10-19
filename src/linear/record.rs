//! Scrubs recorded Linear responses so no real person or workspace reaches a fixture.
//! Decision 15: people (user-like objects) and the workspace are replaced; teams, states,
//! labels and the like are seeded inventions and stay.

use crate::transport::Transport;
use crate::transport::record::RecordingTransport;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Invented full names, handed out by order of first appearance; past eight a numeric
/// suffix keeps them unique.
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
    /// First-appearance index per person, keyed by every real `id`, `displayName`, `name`
    /// and `email` seen, so objects that carry only some of them unify.
    people: HashMap<String, usize>,
    next_person: usize,
    /// Every real value replaced so far, for `check`.
    replaced: Vec<String>,
}

impl Scrubber {
    pub fn new(url_key: &str) -> Self {
        Self {
            url_key: url_key.to_string(),
            people: HashMap::new(),
            next_person: 0,
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
        let keys: Vec<String> = ["id", "displayName", "name", "email"]
            .iter()
            .filter_map(|k| map.get(*k).and_then(Value::as_str))
            .filter(|k| !k.is_empty())
            .map(String::from)
            .collect();
        // Any key already seen identifies the person; every key is then an alias of them.
        let i = keys
            .iter()
            .find_map(|k| self.people.get(k).copied())
            .unwrap_or_else(|| {
                self.next_person += 1;
                self.next_person - 1
            });
        for k in keys {
            self.people.insert(k, i);
        }
        let base = INVENTED_PEOPLE[i % INVENTED_PEOPLE.len()];
        let round = i / INVENTED_PEOPLE.len();
        let fake_name = if round == 0 {
            base.to_string()
        } else {
            format!("{base} {}", round + 1)
        };
        let fake_email = format!("person{i}@example.com");
        for (field, fake) in [
            ("name", Some(fake_name.as_str())),
            ("displayName", Some(fake_name.as_str())),
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

    #[test]
    fn an_id_less_object_unifies_with_the_same_person_in_either_order() {
        let mut s = Scrubber::new("example-key");
        let mut a = json!([{"id": "u1", "displayName": "Real One"}, {"displayName": "Real One"},
                           {"displayName": "Real Two"}]);
        s.scrub(&mut a);
        assert_eq!(a[0]["displayName"], INVENTED_PEOPLE[0]);
        assert_eq!(a[1]["displayName"], INVENTED_PEOPLE[0]);
        assert_eq!(a[2]["displayName"], INVENTED_PEOPLE[1]);
        let mut s = Scrubber::new("example-key");
        let mut b = json!({"displayName": "Real One"});
        s.scrub(&mut b);
        let mut c = json!({"id": "u1", "displayName": "Real One"});
        s.scrub(&mut c);
        assert_eq!(b["displayName"], c["displayName"]);
    }

    #[test]
    fn ten_people_get_ten_distinct_names() {
        let mut s = Scrubber::new("example-key");
        let mut v = Value::Array(
            (0..10)
                .map(|i| json!({"id": format!("u{i}"), "displayName": format!("real{i}")}))
                .collect(),
        );
        s.scrub(&mut v);
        let names: std::collections::HashSet<_> = v
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["displayName"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names.len(), 10);
        assert_eq!(v[8]["displayName"], format!("{} 2", INVENTED_PEOPLE[0]));
    }
}
