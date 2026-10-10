//! GitLab HTTP stubs for the pull request (merge request) operations.

use std::sync::LazyLock;

use rgit_forge::{EnsureOutcome, Forge, GitlabClient, PullRequestState, RepoRef};
use wiremock::matchers::{body_json, body_string, method, path_regex, query_param};
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
        .pull_requests(&repo(), 1, PullRequestState::Closed, Some("feat-a"))
        .await
        .expect("listing");
    assert!(listing.items.is_empty());
    assert!(!listing.more);
}
