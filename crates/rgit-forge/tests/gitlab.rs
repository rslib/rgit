//! GitLab HTTP stubs for the pull request (merge request) operations.

use std::sync::LazyLock;

use rgit_forge::{EnsureOutcome, Forge, GitlabClient, PullRequestState, RepoRef};
use wiremock::matchers::{body_json, body_partial_json, body_string, method, path_regex, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn random_username(prefix: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .subsec_nanos();
    format!("{prefix}-{nanos:x}")
}

static AUTHOR: LazyLock<String> = LazyLock::new(|| random_username("dev"));
static REVIEWER: LazyLock<String> = LazyLock::new(|| random_username("rev"));

fn repo() -> RepoRef {
    RepoRef {
        owner: "group".to_owned(),
        name: "project".to_owned(),
    }
}

fn mr_json(iid: u64, source: &str, target: &str) -> serde_json::Value {
    serde_json::json!({
        "iid": iid,
        "title": "Title",
        "state": "opened",
        "web_url": format!("https://gitlab.example/group/project/-/merge_requests/{iid}"),
        "draft": false,
        "author": {"username": AUTHOR.as_str()},
        "description": " Longer story.",
        "created_at": "2026-01-02T03:04:05Z",
        "updated_at": "2026-01-03T03:04:05Z",
        "sha": "abc123",
        "merge_status": "can_be_merged",
        "labels": ["ci", "forge"],
        "reviewers": [{"username": REVIEWER.as_str()}],
        "source_branch": source,
        "target_branch": target
    })
}

fn assert_mapped(pr: &rgit_forge::PullRequest) {
    assert_eq!(pr.author.as_deref(), Some(AUTHOR.as_str()));
    assert_eq!(pr.body.as_deref(), Some(" Longer story."));
    assert_eq!(pr.created_at.as_deref(), Some("2026-01-02T03:04:05Z"));
    assert_eq!(pr.updated_at.as_deref(), Some("2026-01-03T03:04:05Z"));
    assert_eq!(pr.head_sha.as_deref(), Some("abc123"));
    assert_eq!(pr.mergeable, Some(true));
    assert!(!pr.merged);
    assert_eq!(pr.labels, vec!["ci", "forge"]);
    assert_eq!(pr.reviewers, vec![REVIEWER.clone()]);
}

fn forge(server: &MockServer) -> Forge {
    Forge::Gitlab(
        GitlabClient::from_token("token".to_owned(), Some(&server.uri())).expect("client"),
    )
}

async fn stub_open_mrs(server: &MockServer, body: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path_regex(
            r"^/api/v4/projects/group(%2F|/)project/merge_requests$",
        ))
        .and(query_param("state", "opened"))
        .and(query_param("source_branch", "feature"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

#[tokio::test]
async fn ensure_pull_request_creates_when_no_open_mr_matches() {
    let server = MockServer::start().await;
    stub_open_mrs(&server, serde_json::json!([])).await;
    Mock::given(method("POST"))
        .and(path_regex(
            r"^/api/v4/projects/group(%2F|/)project/merge_requests$",
        ))
        .and(body_json(serde_json::json!({
            "title": "Add forge PRs",
            "source_branch": "feature",
            "target_branch": "main",
            "description": "Longer story.",
            "draft": false
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(mr_json(12, "feature", "main")))
        .mount(&server)
        .await;

    let ensured = forge(&server)
        .ensure_pull_request(
            &repo(),
            "feature",
            "main",
            "Add forge PRs",
            Some("Longer story."),
            false,
        )
        .await
        .expect("ensured");
    assert_eq!(ensured.outcome, EnsureOutcome::Created);
    assert_eq!(ensured.pull_request.number, 12);
    assert_eq!(ensured.pull_request.head_branch, "feature");
    assert_eq!(ensured.pull_request.base_branch, "main");
    assert_mapped(&ensured.pull_request);
}

#[tokio::test]
async fn ensure_pull_request_only_reports_when_the_base_matches() {
    let server = MockServer::start().await;
    stub_open_mrs(&server, serde_json::json!([mr_json(12, "feature", "main")])).await;

    let ensured = forge(&server)
        .ensure_pull_request(&repo(), "feature", "main", "Add forge PRs", None, false)
        .await
        .expect("ensured");
    assert_eq!(
        ensured.outcome,
        EnsureOutcome::Updated { retargeted: false }
    );
    assert_eq!(ensured.pull_request.number, 12);
}

#[tokio::test]
async fn ensure_pull_request_retargets_when_the_base_differs() {
    let server = MockServer::start().await;
    stub_open_mrs(
        &server,
        serde_json::json!([mr_json(12, "feature", "develop")]),
    )
    .await;
    Mock::given(method("PUT"))
        .and(path_regex(
            r"^/api/v4/projects/group(%2F|/)project/merge_requests/12$",
        ))
        .and(body_string("target_branch=main"))
        .respond_with(ResponseTemplate::new(200).set_body_json(mr_json(12, "feature", "main")))
        .mount(&server)
        .await;

    let ensured = forge(&server)
        .ensure_pull_request(&repo(), "feature", "main", "Add forge PRs", None, false)
        .await
        .expect("ensured");
    assert_eq!(ensured.outcome, EnsureOutcome::Updated { retargeted: true });
    assert_eq!(ensured.pull_request.number, 12);
    assert_eq!(ensured.pull_request.base_branch, "main");
}

#[tokio::test]
async fn pulls_one_merge_request_by_iid() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(
            r"^/api/v4/projects/group(%2F|/)project/merge_requests/12$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(mr_json(12, "feature", "main")))
        .mount(&server)
        .await;

    let pr = forge(&server)
        .pull_request(&repo(), 12)
        .await
        .expect("merge request");
    assert_eq!(pr.number, 12);
    assert_eq!(pr.head_branch, "feature");
    assert_mapped(&pr);
}

#[tokio::test]
async fn finds_the_merge_request_for_a_branch() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(
            r"^/api/v4/projects/group(%2F|/)project/merge_requests$",
        ))
        .and(query_param("state", "all"))
        .and(query_param("source_branch", "feature"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            mr_json(11, "feature-next", "main"),
            mr_json(12, "feature", "main")
        ])))
        .mount(&server)
        .await;

    let pr = forge(&server)
        .pull_request_for_branch(&repo(), "feature")
        .await
        .expect("search")
        .expect("merge request");
    assert_eq!(pr.number, 12);
}

#[tokio::test]
async fn lists_the_jobs_of_the_head_pipeline() {
    let server = MockServer::start().await;
    let mut mr = mr_json(12, "feature", "main");
    mr["head_pipeline"] = serde_json::json!({"id": 7, "status": "running"});
    Mock::given(method("GET"))
        .and(path_regex(
            r"^/api/v4/projects/group(%2F|/)project/merge_requests/12$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(mr))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path_regex(
            r"^/api/v4/projects/group(%2F|/)project/pipelines/7/jobs$",
        ))
        .and(query_param("per_page", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"name": "build", "status": "success", "web_url": "https://ci.example/build"},
            {"name": "spec", "status": "failed", "web_url": null}
        ])))
        .mount(&server)
        .await;

    let checks = forge(&server)
        .pull_request_checks(&repo(), 12)
        .await
        .expect("checks");
    assert_eq!(checks.rollup, rgit_forge::CheckRollup::Failing);
    let names: Vec<&str> = checks.checks.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["build", "spec"]);
    assert_eq!(checks.checks[0].status, "passing");
    assert_eq!(checks.checks[1].status, "failing");
}

#[tokio::test]
async fn a_merge_request_without_a_pipeline_has_no_checks() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(
            r"^/api/v4/projects/group(%2F|/)project/merge_requests/12$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(mr_json(12, "feature", "main")))
        .mount(&server)
        .await;

    let checks = forge(&server)
        .pull_request_checks(&repo(), 12)
        .await
        .expect("checks");
    assert_eq!(checks.rollup, rgit_forge::CheckRollup::None);
    assert!(checks.checks.is_empty());
}

#[tokio::test]
async fn list_requests_the_author_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(
            r"^/api/v4/projects/group(%2F|/)project/merge_requests$",
        ))
        .and(query_param("state", "all"))
        .and(query_param("author_username", AUTHOR.as_str()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!([mr_json(12, "feature", "main")]))
                .insert_header("X-Total", "1")
                .insert_header("X-Next-Page", ""),
        )
        .mount(&server)
        .await;

    let listing = forge(&server)
        .pull_requests(&repo(), 1, PullRequestState::All, None, Some(&AUTHOR))
        .await
        .expect("listing");
    assert_eq!(listing.items.len(), 1);
    assert_eq!(listing.items[0].number, 12);
    assert_eq!(listing.total, Some(1));
}

#[tokio::test]
async fn list_requests_the_state_and_source_branch_filters() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(
            r"^/api/v4/projects/group(%2F|/)project/merge_requests$",
        ))
        .and(query_param("state", "closed"))
        .and(query_param("source_branch", "feat-a"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;

    let listing = forge(&server)
        .pull_requests(&repo(), 1, PullRequestState::Closed, Some("feat-a"), None)
        .await
        .expect("listing");
    assert!(listing.items.is_empty());
    assert!(!listing.more);
}

#[tokio::test]
async fn merges_a_merge_request_with_squash() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path_regex(
            r"^/api/v4/projects/group(%2F|/)project/merge_requests/12$",
        ))
        .and(body_partial_json(serde_json::json!({
            "squash": true,
            "should_remove_source_branch": true
        })))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let mut merged = mr_json(12, "feature", "main");
    merged["state"] = serde_json::json!("merged");
    Mock::given(method("GET"))
        .and(path_regex(
            r"^/api/v4/projects/group(%2F|/)project/merge_requests/12$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(merged))
        .mount(&server)
        .await;

    let pr = forge(&server)
        .merge_pull_request(&repo(), 12, rgit_forge::MergeMethod::Squash, true)
        .await
        .expect("merged");
    assert_eq!(pr.number, 12);
    assert!(pr.merged);
    assert_eq!(pr.head_branch, "feature");
}

#[tokio::test]
async fn merge_with_rebase_rebases_onto_the_target_first() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(
            r"^/api/v4/projects/group(%2F|/)project/merge_requests/12/rebase$",
        ))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "rebase_in_progress": true
            })),
        )
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path_regex(
            r"^/api/v4/projects/group(%2F|/)project/merge_requests/12$",
        ))
        .and(body_partial_json(
            serde_json::json!({"should_remove_source_branch": false}),
        ))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let mut merged = mr_json(12, "feature", "main");
    merged["state"] = serde_json::json!("merged");
    Mock::given(method("GET"))
        .and(path_regex(
            r"^/api/v4/projects/group(%2F|/)project/merge_requests/12$",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(merged))
        .mount(&server)
        .await;

    let pr = forge(&server)
        .merge_pull_request(&repo(), 12, rgit_forge::MergeMethod::Rebase, false)
        .await
        .expect("merged");
    assert_eq!(pr.number, 12);
    assert!(pr.merged);
}

#[tokio::test]
async fn edits_the_title_and_description_of_a_merge_request() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path_regex(
            r"^/api/v4/projects/group(%2F|/)project/merge_requests/12$",
        ))
        .and(body_partial_json(serde_json::json!({
            "title": "New title",
            "description": "New story."
        })))
        .respond_with({
            let mut edited = mr_json(12, "feature", "main");
            edited["title"] = serde_json::json!("New title");
            edited["description"] = serde_json::json!("New story.");
            ResponseTemplate::new(200).set_body_json(edited)
        })
        .mount(&server)
        .await;

    let pr = forge(&server)
        .edit_pull_request(&repo(), 12, Some("New title"), Some("New story."), None)
        .await
        .expect("edited");
    assert_eq!(pr.number, 12);
    assert_eq!(pr.title, "New title");
    assert_eq!(pr.body.as_deref(), Some("New story."));
}
