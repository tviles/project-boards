//! Read-only lookups the editors need: a repository's assignable users, labels and
//! milestones; repositories the user can push to; issue and pull request search.

use crate::github::convert::{nodes, str_of, typename};
use crate::github::queries;
use crate::github::transport::GraphqlRequest;
use crate::github::{Github, GithubError};
use crate::model::*;
use graphql_client::GraphQLQuery;
use serde_json::Value;

fn writable(repo: &Value) -> bool {
    matches!(
        repo["viewerPermission"].as_str(),
        Some("ADMIN" | "MAINTAIN" | "WRITE")
    ) && repo["hasIssuesEnabled"].as_bool() == Some(true)
}

fn repo_ref(repo: &Value) -> Option<RepoRef> {
    Some(RepoRef {
        id: repo["id"].as_str()?.to_string(),
        slug: repo["nameWithOwner"].as_str()?.parse().ok()?,
    })
}

fn hit(node: &Value) -> Option<ContentHit> {
    let pull = match typename(node) {
        "Issue" => false,
        "PullRequest" => true,
        _ => return None,
    };
    Some(ContentHit {
        id: node["id"].as_str()?.to_string(),
        pull,
        repo: str_of(&node["repository"]["nameWithOwner"]),
        number: node["number"].as_u64()? as u32,
        title: str_of(&node["title"]),
        open: node["state"] == "OPEN",
    })
}

impl Github {
    /// The first 100 assignable users and labels and 50 open milestones of `repo`.
    pub async fn repo_candidates(&self, repo: &RepoSlug) -> Result<RepoCandidates, GithubError> {
        let vars = queries::repo_candidates::Variables {
            owner: repo.owner.clone(),
            name: repo.name.clone(),
        };
        let data = self
            .run(GraphqlRequest::from_body(
                queries::RepoCandidates::build_query(vars),
            ))
            .await?
            .data;
        let r = &data["repository"];
        let id = r["id"].as_str().ok_or_else(|| {
            GithubError::Decode(format!("repository {repo} not found or not visible"))
        })?;
        Ok(RepoCandidates {
            repo: RepoRef {
                id: id.to_string(),
                slug: repo.clone(),
            },
            users: nodes(r, "assignableUsers")
                .into_iter()
                .filter_map(|u| {
                    Some(UserRef {
                        id: u["id"].as_str()?.into(),
                        login: u["login"].as_str()?.into(),
                    })
                })
                .collect(),
            labels: nodes(r, "labels")
                .into_iter()
                .filter_map(|l| {
                    Some(LabelRef {
                        id: l["id"].as_str()?.into(),
                        name: str_of(&l["name"]),
                        color: str_of(&l["color"]),
                    })
                })
                .collect(),
            milestones: nodes(r, "milestones")
                .into_iter()
                .filter_map(|m| {
                    Some(MilestoneRef {
                        id: m["id"].as_str()?.into(),
                        title: str_of(&m["title"]),
                    })
                })
                .collect(),
        })
    }

    /// The viewer's 100 most recently pushed repositories they can write to, issues on.
    pub async fn pushable_repos(&self) -> Result<Vec<RepoRef>, GithubError> {
        let body = queries::PushableRepos::build_query(queries::pushable_repos::Variables);
        let data = self.run(GraphqlRequest::from_body(body)).await?.data;
        Ok(nodes(&data["viewer"], "repositories")
            .into_iter()
            .filter(|r| writable(r))
            .filter_map(repo_ref)
            .collect())
    }

    /// `repo` when it exists, the viewer can write to it and it has issues; else `None`.
    pub async fn lookup_repo(&self, repo: &RepoSlug) -> Result<Option<RepoRef>, GithubError> {
        let vars = queries::repo_lookup::Variables {
            owner: repo.owner.clone(),
            name: repo.name.clone(),
        };
        let data = self
            .run(GraphqlRequest::from_body(queries::RepoLookup::build_query(
                vars,
            )))
            .await?
            .data;
        let r = &data["repository"];
        Ok(if r.is_object() && writable(r) {
            repo_ref(r)
        } else {
            None
        })
    }

    /// Issues and pull requests matching GitHub search `query`, at most 20.
    pub async fn search_content(&self, query: &str) -> Result<Vec<ContentHit>, GithubError> {
        let vars = queries::search_content::Variables {
            query: query.into(),
        };
        let data = self
            .run(GraphqlRequest::from_body(
                queries::SearchContent::build_query(vars),
            ))
            .await?
            .data;
        Ok(nodes(&data, "search").into_iter().filter_map(hit).collect())
    }

    /// Issue or pull request `number` of `repo`, if there is one.
    pub async fn content_by_number(
        &self,
        repo: &RepoSlug,
        number: u32,
    ) -> Result<Option<ContentHit>, GithubError> {
        let vars = queries::content_by_number::Variables {
            owner: repo.owner.clone(),
            name: repo.name.clone(),
            number: i64::from(number),
        };
        let data = self
            .run(GraphqlRequest::from_body(
                queries::ContentByNumber::build_query(vars),
            ))
            .await?
            .data;
        Ok(hit(&data["repository"]["issueOrPullRequest"]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::fixture::FixtureTransport;
    use serde_json::json;
    use std::sync::Arc;

    fn repo() -> RepoSlug {
        "acme/app".parse().unwrap()
    }

    #[tokio::test]
    async fn candidates_decode_users_labels_and_milestones() {
        let t = Arc::new(FixtureTransport::new());
        t.push(
            "RepoCandidates",
            json!({"repository": {"id": "R_1",
            "assignableUsers": {"nodes": [{"id": "U_1", "login": "octo"}, null]},
            "labels": {"nodes": [{"id": "LA_1", "name": "good first issue", "color": "7057ff"}]},
            "milestones": {"nodes": [{"id": "MI_1", "title": "v1"}]}}}),
        );
        let c = Github::new(t.clone())
            .repo_candidates(&repo())
            .await
            .unwrap();
        assert_eq!(
            c.repo,
            RepoRef {
                id: "R_1".into(),
                slug: repo()
            }
        );
        assert_eq!(
            c.users,
            [UserRef {
                id: "U_1".into(),
                login: "octo".into()
            }]
        );
        assert_eq!(c.labels[0].name, "good first issue");
        assert_eq!(c.milestones[0].title, "v1");
        assert_eq!(
            (
                t.requests()[0].variables["owner"].as_str(),
                t.requests()[0].variables["name"].as_str()
            ),
            (Some("acme"), Some("app"))
        );
    }

    #[tokio::test]
    async fn only_repositories_with_write_access_and_issues_are_offered() {
        let t = Arc::new(FixtureTransport::new());
        t.push("PushableRepos", json!({"viewer": {"repositories": {"nodes": [
            {"id": "R_1", "nameWithOwner": "acme/app", "viewerPermission": "WRITE", "hasIssuesEnabled": true},
            {"id": "R_2", "nameWithOwner": "acme/docs", "viewerPermission": "READ", "hasIssuesEnabled": true},
            {"id": "R_3", "nameWithOwner": "acme/site", "viewerPermission": "ADMIN", "hasIssuesEnabled": false},
            {"id": "R_4", "nameWithOwner": "octo/dots", "viewerPermission": "ADMIN", "hasIssuesEnabled": true}
        ]}}}));
        let list = Github::new(t).pushable_repos().await.unwrap();
        let slugs: Vec<String> = list.iter().map(|r| r.slug.to_string()).collect();
        assert_eq!(slugs, ["acme/app", "octo/dots"]);
    }

    #[tokio::test]
    async fn a_repository_lookup_answers_none_when_missing_or_read_only() {
        let t = Arc::new(FixtureTransport::new());
        t.push("RepoLookup", json!({"repository": {"id": "R_1", "nameWithOwner": "acme/app", "viewerPermission": "MAINTAIN", "hasIssuesEnabled": true}}));
        t.push("RepoLookup", json!({"repository": null}));
        t.push("RepoLookup", json!({"repository": {"id": "R_2", "nameWithOwner": "acme/docs", "viewerPermission": "TRIAGE", "hasIssuesEnabled": true}}));
        let gh = Github::new(t);
        assert_eq!(
            gh.lookup_repo(&repo()).await.unwrap().map(|r| r.id),
            Some("R_1".into())
        );
        assert_eq!(gh.lookup_repo(&repo()).await.unwrap(), None);
        assert_eq!(gh.lookup_repo(&repo()).await.unwrap(), None);
    }

    #[tokio::test]
    async fn search_hits_and_numbers_decode_issues_and_pull_requests() {
        let t = Arc::new(FixtureTransport::new());
        t.push("SearchContent", json!({"search": {"nodes": [
            {"__typename": "Issue", "id": "I_1", "number": 12, "title": "Crash", "state": "OPEN", "repository": {"nameWithOwner": "acme/app"}},
            {"__typename": "PullRequest", "id": "PR_1", "number": 13, "title": "Fix", "state": "MERGED", "repository": {"nameWithOwner": "acme/app"}},
            {"__typename": "Discussion"}
        ]}}));
        t.push("ContentByNumber", json!({"repository": {"issueOrPullRequest": {"__typename": "Issue", "id": "I_1", "number": 12, "title": "Crash", "state": "CLOSED", "repository": {"nameWithOwner": "acme/app"}}}}));
        t.push(
            "ContentByNumber",
            json!({"repository": {"issueOrPullRequest": null}}),
        );
        let gh = Github::new(t.clone());
        let hits = gh.search_content("crash repo:acme/app").await.unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(
            (hits[0].pull, hits[0].open, hits[1].pull, hits[1].open),
            (false, true, true, false)
        );
        assert_eq!(t.requests()[0].variables["query"], "crash repo:acme/app");
        let one = gh.content_by_number(&repo(), 12).await.unwrap().unwrap();
        assert_eq!((one.number, one.open), (12, false));
        assert_eq!(t.requests()[1].variables["number"], 12);
        assert_eq!(gh.content_by_number(&repo(), 99).await.unwrap(), None);
    }
}
