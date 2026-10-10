//! GitHub HTTP stubs for the pull request operations.

use std::sync::LazyLock;

use rgit_forge::{EnsureOutcome, Forge, GithubClient, PullRequestState, RepoRef};
use wiremock::matchers::{body_partial_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn random_login(prefix: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .subsec_nanos();
    format!("{prefix}-{nanos:x}")
}

static AUTHOR: LazyLock<String> = LazyLock::new(|| random_login("dev"));
static REVIEWER: LazyLock<String> = LazyLock::new(|| random_login("rev"));

fn repo() -> RepoRef {
    RepoRef {
        owner: "owner".to_owned(),
        name: "repo".to_owned(),
    }
}

fn pr_json(number: u64, head: &str, base: &str) -> serde_json::Value {
    serde_json::json!({
        "number": number,
        "title": "Title",
        "state": "open",
        "html_url": format!("https://github.com/owner/repo/pull/{number}"),
        "draft": false,
        "body": " Longer story.",
        "user": {"login": AUTHOR.as_str()},
        "created_at": "2026-01-02T03:04:05Z",
        "updated_at": "2026-01-03T03:04:05Z",
        "mergeable": true,
        "merged": false,
        "labels": [{"name": "ci"}, {"name": "forge"}],
        "requested_reviewers": [{"login": REVIEWER.as_str()}],
        "head": {"ref": head, "sha": "abc"},
        "base": {"ref": base, "sha": "def"}
    })
}

fn assert_mapped(pr: &rgit_forge::PullRequest) {
    assert_eq!(pr.author.as_deref(), Some(AUTHOR.as_str()));
    assert_eq!(pr.body.as_deref(), Some(" Longer story."));
    assert_eq!(pr.created_at.as_deref(), Some("2026-01-02T03:04:05Z"));
    assert_eq!(pr.updated_at.as_deref(), Some("2026-01-03T03:04:05Z"));
    assert_eq!(pr.head_sha.as_deref(), Some("abc"));
    assert_eq!(pr.mergeable, Some(true));
    assert!(!pr.merged);
    assert_eq!(pr.labels, vec!["ci", "forge"]);
    assert_eq!(pr.reviewers, vec![REVIEWER.clone()]);
}

fn forge(server: &MockServer) -> Forge {
    Forge::Github(
        GithubClient::from_token_at("token".to_owned(), Some(&server.uri())).expect("client"),
    )
}

async fn stub_open_prs(server: &MockServer, body: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls"))
        .and(query_param("state", "open"))
        .and(query_param("head", "owner:feature"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

#[tokio::test]
async fn ensure_pull_request_creates_when_no_open_pr_matches() {
    let server = MockServer::start().await;
    stub_open_prs(&server, serde_json::json!([])).await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/pulls"))
        .and(body_partial_json(serde_json::json!({
            "title": "Add forge PRs",
            "head": "feature",
            "base": "main",
            "draft": false,
            "body": "Longer story."
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(pr_json(12, "feature", "main")))
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
    stub_open_prs(&server, serde_json::json!([pr_json(12, "feature", "main")])).await;

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
    stub_open_prs(
        &server,
        serde_json::json!([pr_json(12, "feature", "develop")]),
    )
    .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/owner/repo/pulls/12"))
        .and(body_partial_json(serde_json::json!({"base": "main"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(pr_json(12, "feature", "main")))
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
async fn ensure_pull_requests_reports_one_word_per_branch() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/pulls"))
        .respond_with(ResponseTemplate::new(201).set_body_json(pr_json(12, "feat-a", "main")))
        .mount(&server)
        .await;

    let lines = forge(&server)
        .ensure_pull_requests(
            &repo(),
            &[rgit_forge::StackEntry {
                branch: "feat-a".to_owned(),
                base: "main".to_owned(),
                message: "Add a thing\n\nIn detail.".to_owned(),
            }],
        )
        .await
        .expect("lines");
    assert_eq!(
        lines,
        vec!["opened https://github.com/owner/repo/pull/12 (feat-a)"]
    );
}

#[tokio::test]
async fn list_requests_the_state_and_head_filters() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls"))
        .and(query_param("state", "all"))
        .and(query_param("head", "owner:feat-a"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;

    let listing = forge(&server)
        .pull_requests(&repo(), 1, PullRequestState::All, Some("feat-a"))
        .await
        .expect("listing");
    assert!(listing.items.is_empty());
    assert!(!listing.more);
}
