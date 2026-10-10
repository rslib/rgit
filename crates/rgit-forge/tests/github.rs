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
async fn pulls_one_pull_request_by_number() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pr_json(12, "feature", "main")))
        .mount(&server)
        .await;

    let pr = forge(&server)
        .pull_request(&repo(), 12)
        .await
        .expect("pull request");
    assert_eq!(pr.number, 12);
    assert_eq!(pr.head_branch, "feature");
    assert_mapped(&pr);
}

#[tokio::test]
async fn finds_the_pull_request_for_a_branch() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls"))
        .and(query_param("state", "all"))
        .and(query_param("head", "owner:feature"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            pr_json(11, "feature-next", "main"),
            pr_json(12, "feature", "main")
        ])))
        .mount(&server)
        .await;

    let pr = forge(&server)
        .pull_request_for_branch(&repo(), "feature")
        .await
        .expect("search")
        .expect("pull request");
    assert_eq!(pr.number, 12);
}

#[tokio::test]
async fn combines_check_runs_and_statuses_for_checks() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pr_json(12, "feature", "main")))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/commits/abc/check-runs"))
        .and(query_param("per_page", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "total_count": 2,
            "check_runs": [
                {"name": "build", "status": "completed", "conclusion": "success",
                 "html_url": "https://ci.example/build"},
                {"name": "publish", "status": "queued", "conclusion": null,
                 "html_url": null}
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/commits/abc/status"))
        .and(query_param("per_page", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "state": "success",
            "statuses": [
                {"state": "failure", "context": "unit", "target_url": "https://ci.example/unit"}
            ]
        })))
        .mount(&server)
        .await;

    let checks = forge(&server)
        .pull_request_checks(&repo(), 12)
        .await
        .expect("checks");
    assert_eq!(checks.rollup, rgit_forge::CheckRollup::Failing);
    let names: Vec<&str> = checks.checks.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["build", "publish", "unit"]);
    assert_eq!(checks.checks[0].status, "passing");
    assert_eq!(checks.checks[1].status, "pending");
    assert_eq!(checks.checks[1].url, None);
    assert_eq!(checks.checks[2].status, "failing");
    assert_eq!(
        checks.checks[2].url.as_deref(),
        Some("https://ci.example/unit")
    );
}

#[tokio::test]
async fn list_keeps_only_the_requests_of_the_author() {
    let server = MockServer::start().await;
    let own = pr_json(12, "feature", "main");
    let mut other = pr_json(13, "other", "main");
    other["user"] = serde_json::json!({"login": "someoneelse"});
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls"))
        .and(query_param("state", "all"))
        .and(query_param("head", "owner:feature"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([own, other])))
        .mount(&server)
        .await;

    let listing = forge(&server)
        .pull_requests(
            &repo(),
            1,
            PullRequestState::All,
            Some("feature"),
            Some(&AUTHOR),
        )
        .await
        .expect("listing");
    assert_eq!(listing.items.len(), 1);
    assert_eq!(listing.items[0].number, 12);
    assert_eq!(listing.items[0].author.as_deref(), Some(AUTHOR.as_str()));
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
        .pull_requests(&repo(), 1, PullRequestState::All, Some("feat-a"), None)
        .await
        .expect("listing");
    assert!(listing.items.is_empty());
    assert!(!listing.more);
}

#[tokio::test]
async fn merges_a_pull_request_and_deletes_its_branch() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/repos/owner/repo/pulls/12/merge"))
        .and(body_partial_json(serde_json::json!({"merge_method": "squash"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "merged": true,
            "message": "Pull Request successfully merged",
            "sha": "def"
        })))
        .mount(&server)
        .await;
    let mut merged = pr_json(12, "feature", "main");
    merged["merged"] = serde_json::json!(true);
    merged["state"] = serde_json::json!("closed");
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(merged))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/repos/owner/repo/git/refs/heads/feature"))
        .respond_with(ResponseTemplate::new(204))
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
async fn merge_reports_when_the_forge_refuses() {
    let server = MockServer::start().await;
    Mock::given(method("PUT"))
        .and(path("/repos/owner/repo/pulls/12/merge"))
        .and(body_partial_json(serde_json::json!({"merge_method": "merge"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "merged": false,
            "message": "Head branch is behind the base branch"
        })))
        .mount(&server)
        .await;

    let error = forge(&server)
        .merge_pull_request(&repo(), 12, rgit_forge::MergeMethod::Merge, false)
        .await
        .expect_err("refused merge");
    assert!(error.to_string().contains("behind"), "{error}");
}
