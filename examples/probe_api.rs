//! Phase 0 probes 1-3 against the testbed. Run: cargo run --example probe_api
//! Prints a Markdown report for docs/phase0-findings.md.

use project_boards::github::token::resolve_token_from_system;
use project_boards::github::transport::{GraphqlRequest, HttpTransport, Transport};
use serde_json::{Value, json};

type Check = Box<dyn Fn(&Value) -> bool>;

const ITEMS: &str = r#"query Probe($id: ID!, $q: String!, $after: String) {
  node(id: $id) { ... on ProjectV2 { items(first: 100, after: $after, query: $q) {
    totalCount pageInfo { hasNextPage endCursor }
    nodes { id updatedAt
      content { __typename ... on Issue { number labels(first: 10) { nodes { name } } assignees(first: 5) { nodes { login } } }
                            ... on PullRequest { number labels(first: 10) { nodes { name } } assignees(first: 5) { nodes { login } } } }
      status: fieldValueByName(name: "Status") { ... on ProjectV2ItemFieldSingleSelectValue { name } } } } } } }"#;

async fn items(t: &HttpTransport, id: &str, q: &str) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let req = GraphqlRequest::raw("Probe", ITEMS, json!({"id": id, "q": q, "after": after}));
        let data = t.execute(req).await.map_err(|e| e.to_string())?.data;
        let conn = &data["node"]["items"];
        out.extend(conn["nodes"].as_array().cloned().unwrap_or_default());
        if conn["pageInfo"]["hasNextPage"].as_bool() != Some(true) {
            break;
        }
        after = conn["pageInfo"]["endCursor"].as_str().map(String::from);
    }
    Ok(out)
}

fn labels(item: &Value) -> Vec<String> {
    item["content"]["labels"]["nodes"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|l| l["name"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}
fn status(item: &Value) -> String {
    item["status"]["name"].as_str().unwrap_or("").to_string()
}

/// Item ids from one page of `items(first: 100 ...)` with the given argument text (no `query` unless given).
async fn first_page_ids(
    t: &HttpTransport,
    project_id: &str,
    args: &str,
) -> Result<Vec<String>, String> {
    let q = format!(
        "query L($id: ID!) {{ node(id: $id) {{ ... on ProjectV2 {{ items({args}) {{ totalCount nodes {{ id }} }} }} }} }}"
    );
    let data = t
        .execute(GraphqlRequest::raw("L", &q, json!({"id": project_id})))
        .await
        .map_err(|e| e.to_string())?
        .data;
    Ok(data["node"]["items"]["nodes"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|n| n["id"].as_str().map(String::from))
        .collect())
}

/// Probes 0 and 0b: how long after a write does each form of `items` show the new item?
/// 0: `query: ""`. 0b: no `query` argument at all, and no `query` with orderBy POSITION.
async fn probe_index_lag(t: &HttpTransport, project_id: &str) {
    println!("## 0. Index lag\n");
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let add = r#"mutation A($p: ID!, $t: String!) { addProjectV2DraftIssue(input: {projectId: $p, title: $t}) { projectItem { id } } }"#;
    let added = match t
        .execute(GraphqlRequest::raw(
            "A",
            add,
            json!({"p": project_id, "t": format!("probe lag {unix}")}),
        ))
        .await
    {
        Ok(r) => r.data,
        Err(e) => {
            println!("- add draft error: {e}\n");
            return;
        }
    };
    let new_id = added["addProjectV2DraftIssue"]["projectItem"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let forms = [
        ("0 query:\"\"", "first: 100, query: \"\""),
        ("0b no query", "first: 100"),
        (
            "0b no query, orderBy POSITION",
            "first: 100, orderBy: {field: POSITION, direction: ASC}",
        ),
    ];
    let mut first_seen: Vec<Option<u64>> = vec![None; forms.len()];
    let start = std::time::Instant::now();
    for attempt in 0..=12u64 {
        let at = start.elapsed().as_secs();
        for (n, (label, args)) in forms.iter().enumerate() {
            match first_page_ids(t, project_id, args).await {
                Ok(ids) => {
                    let seen = ids.contains(&new_id);
                    println!(
                        "- t+{at}s [{label}]: {} items, new id present: {seen}",
                        ids.len()
                    );
                    if seen && first_seen[n].is_none() {
                        first_seen[n] = Some(at);
                    }
                }
                Err(e) => println!("- t+{at}s [{label}]: error: {e}"),
            }
        }
        if first_seen.iter().all(Option::is_some) || attempt == 12 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
    println!();
    for (n, (label, _)) in forms.iter().enumerate() {
        match first_seen[n] {
            Some(0) => println!("- [{label}] visible immediately"),
            Some(s) => println!("- [{label}] first visible after about {s}s"),
            None => println!(
                "- [{label}] NOT visible after {}s",
                start.elapsed().as_secs()
            ),
        }
    }
    let del = r#"mutation D($p: ID!, $i: ID!) { deleteProjectV2Item(input: {projectId: $p, itemId: $i}) { deletedItemId } }"#;
    match t
        .execute(GraphqlRequest::raw(
            "D",
            del,
            json!({"p": project_id, "i": new_id}),
        ))
        .await
    {
        Ok(_) => println!("\nProbe item deleted.\n"),
        Err(e) => println!("\nProbe item delete error (delete {new_id} by hand): {e}\n"),
    }
}

#[tokio::main]
async fn main() {
    let testbed: toml::Table = std::fs::read_to_string("tests/testbed.toml")
        .unwrap()
        .parse()
        .unwrap();
    let project_id = testbed["project_id"].as_str().unwrap().to_string();
    let token = resolve_token_from_system(None).expect("token");
    let t = HttpTransport::new(token.value);
    println!("# Phase 0 API probes\n");
    probe_index_lag(&t, &project_id).await;
    let all = items(&t, &project_id, "").await.expect("all items");
    println!("All items: {}\n", all.len());

    // Probe 1: updated:>= with a datetime and with a date.
    let mut stamps: Vec<String> = all
        .iter()
        .filter_map(|i| i["updatedAt"].as_str().map(String::from))
        .collect();
    stamps.sort();
    let pivot = stamps[stamps.len() / 2].clone();
    let expected = stamps.iter().filter(|s| **s >= pivot).count();
    println!(
        "## 1. Incremental filter\n\nPivot {pivot}; items updated at or after it: {expected}\n"
    );
    for q in [
        format!("updated:>={pivot}"),
        format!("updated:>={}", &pivot[..10]),
    ] {
        match items(&t, &project_id, &q).await {
            Ok(found) => println!(
                "- `{q}` → {} items (datetime exact: {})",
                found.len(),
                found.len() == expected
            ),
            Err(e) => println!("- `{q}` → error: {e}"),
        }
    }

    // Probe 2: filter syntax parity, compared with a client-side evaluation of the same items.
    println!("\n## 2. Filter syntax\n");
    let checks: Vec<(&str, Check)> = vec![
        ("label:bug", Box::new(|i| labels(i).contains(&"bug".into()))),
        (
            "-label:bug",
            Box::new(|i| !labels(i).contains(&"bug".into())),
        ),
        (
            "label:bug,docs",
            Box::new(|i| labels(i).iter().any(|l| l == "bug" || l == "docs")),
        ),
        ("status:Todo", Box::new(|i| status(i) == "Todo")),
        (
            "status:\"In Progress\"",
            Box::new(|i| status(i) == "In Progress"),
        ),
        (
            "assignee:@me",
            Box::new(|i| {
                i["content"]["assignees"]["nodes"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|u| u["login"] == "tviles"))
            }),
        ),
        (
            "no:assignee",
            Box::new(|i| {
                i["content"]["assignees"]["nodes"]
                    .as_array()
                    .is_none_or(|a| a.is_empty())
            }),
        ),
        (
            "is:issue",
            Box::new(|i| i["content"]["__typename"] == "Issue"),
        ),
        (
            "is:draft",
            Box::new(|i| i["content"]["__typename"] == "DraftIssue"),
        ),
    ];
    for (q, pred) in checks {
        let expected = all.iter().filter(|i| pred(i)).count();
        match items(&t, &project_id, q).await {
            Ok(found) => println!(
                "- `{q}` → server {} / expected {} {}",
                found.len(),
                expected,
                if found.len() == expected {
                    "✓"
                } else {
                    "✗"
                }
            ),
            Err(e) => println!("- `{q}` → error: {e}"),
        }
    }

    // Probe 3: view filter round trip on the "Probe" view, and group-by/sort read-back.
    println!("\n## 3. Views\n");
    let views_q = r#"query V($id: ID!) { node(id: $id) { ... on ProjectV2 { views(first: 20) { nodes { id name layout filter
        groupByFields(first: 3) { nodes { ... on ProjectV2FieldCommon { name } } }
        verticalGroupByFields(first: 3) { nodes { ... on ProjectV2FieldCommon { name } } }
        sortByFields(first: 3) { nodes { direction field { ... on ProjectV2FieldCommon { name } } } } } } } } }"#;
    let views = t
        .execute(GraphqlRequest::raw("V", views_q, json!({"id": project_id})))
        .await
        .expect("views")
        .data;
    for v in views["node"]["views"]["nodes"].as_array().unwrap() {
        println!(
            "- {} ({}): filter {:?}, groupBy {}, columnBy {}, sortBy {}",
            v["name"],
            v["layout"],
            v["filter"],
            v["groupByFields"]["nodes"],
            v["verticalGroupByFields"]["nodes"],
            v["sortByFields"]["nodes"]
        );
    }
    let probe_view = views["node"]["views"]["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "Probe")
        .expect("Probe view")["id"]
        .clone();
    let filter = "label:bug -status:Done";
    let update = r#"mutation U($v: ID!, $f: String!) { updateProjectV2View(input: {viewId: $v, filter: $f}) { projectV2View { filter } } }"#;
    match t
        .execute(GraphqlRequest::raw(
            "U",
            update,
            json!({"v": probe_view, "f": filter}),
        ))
        .await
    {
        Ok(r) => println!(
            "\nFilter write returned {:?} (round trip exact: {})",
            r.data["updateProjectV2View"]["projectV2View"]["filter"],
            r.data["updateProjectV2View"]["projectV2View"]["filter"] == filter
        ),
        Err(e) => println!("\nFilter write error: {e}"),
    }
    println!(
        "\nGroup-by and sort are not in UpdateProjectV2ViewInput; confirm by checking the schema copy for `groupBy` inside that input."
    );
}
