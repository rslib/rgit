//! GitHub HTTP stubs for the pull request operations.

use std::sync::LazyLock;

use rgit_forge::{EnsureOutcome, Forge, GithubClient, PullRequestState, RepoRef};
use wiremock::matchers::{body_partial_json, body_string_contains, method, path, query_param};
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
        "node_id": format!("PR_node_{number}"),
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
    Mock::given(method("PATCH"))
        .and(path("/repos/owner/repo/pulls/12"))
        .and(body_string_contains("rgit-stack"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pr_json(12, "feat-a", "main")))
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
async fn ensure_pull_requests_marks_each_body_with_its_own_row() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/pulls"))
        .and(body_string_contains("\"head\":\"feat-a\""))
        .respond_with(ResponseTemplate::new(201).set_body_json(pr_json(12, "feat-a", "main")))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/pulls"))
        .and(body_string_contains("\"head\":\"feat-b\""))
        .respond_with(ResponseTemplate::new(201).set_body_json(pr_json(13, "feat-b", "main")))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/owner/repo/pulls/12"))
        .and(body_string_contains("- #13 feat-b"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pr_json(12, "feat-a", "main")))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/owner/repo/pulls/13"))
        .and(body_string_contains("- #12 feat-a"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pr_json(13, "feat-b", "main")))
        .mount(&server)
        .await;

    let lines = forge(&server)
        .ensure_pull_requests(
            &repo(),
            &[
                rgit_forge::StackEntry {
                    branch: "feat-a".to_owned(),
                    base: "main".to_owned(),
                    message: "First\n".to_owned(),
                },
                rgit_forge::StackEntry {
                    branch: "feat-b".to_owned(),
                    base: "feat-a".to_owned(),
                    message: "Second\n".to_owned(),
                },
            ],
        )
        .await
        .expect("lines");
    assert_eq!(lines.len(), 2);
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
        .and(body_partial_json(
            serde_json::json!({"merge_method": "squash"}),
        ))
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
        .and(body_partial_json(
            serde_json::json!({"merge_method": "merge"}),
        ))
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

#[tokio::test]
async fn edits_the_title_and_base_of_a_pull_request() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/repos/owner/repo/pulls/12"))
        .and(body_partial_json(
            serde_json::json!({"title": "New title", "base": "main"}),
        ))
        .respond_with({
            let mut edited = pr_json(12, "feature", "develop");
            edited["title"] = serde_json::json!("New title");
            edited["base"] = serde_json::json!({"ref": "main", "sha": "def"});
            ResponseTemplate::new(200).set_body_json(edited)
        })
        .mount(&server)
        .await;

    let pr = forge(&server)
        .edit_pull_request(&repo(), 12, Some("New title"), None, Some("main"))
        .await
        .expect("edited");
    assert_eq!(pr.number, 12);
    assert_eq!(pr.title, "New title");
    assert_eq!(pr.base_branch, "main");
    assert_mapped(&pr);
}

#[tokio::test]
async fn marks_a_pull_request_ready_through_graphql() {
    let server = MockServer::start().await;
    let mut ready = pr_json(12, "feature", "main");
    ready["draft"] = serde_json::json!(false);
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ready))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("markPullRequestReadyForReview"))
        .and(body_string_contains("PR_node_12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {
                "markPullRequestReadyForReview": {
                    "pullRequest": {"id": "PR_node_12", "isDraft": false}
                }
            }
        })))
        .mount(&server)
        .await;

    let pr = forge(&server)
        .mark_pull_request_ready(&repo(), 12)
        .await
        .expect("ready");
    assert_eq!(pr.number, 12);
    assert!(!pr.draft);
}

#[tokio::test]
async fn marks_a_pull_request_draft_through_graphql() {
    let server = MockServer::start().await;
    let mut draft = pr_json(12, "feature", "main");
    draft["draft"] = serde_json::json!(true);
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/pulls/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(draft))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("convertPullRequestToDraft"))
        .and(body_string_contains("PR_node_12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {
                "convertPullRequestToDraft": {
                    "pullRequest": {"id": "PR_node_12", "isDraft": true}
                }
            }
        })))
        .mount(&server)
        .await;

    let pr = forge(&server)
        .mark_pull_request_draft(&repo(), 12)
        .await
        .expect("draft");
    assert_eq!(pr.number, 12);
    assert!(pr.draft);
}

#[tokio::test]
async fn reopens_a_closed_pull_request() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/repos/owner/repo/pulls/12"))
        .and(body_partial_json(serde_json::json!({"state": "open"})))
        .respond_with({
            let mut open = pr_json(12, "feature", "main");
            open["state"] = serde_json::json!("open");
            ResponseTemplate::new(200).set_body_json(open)
        })
        .mount(&server)
        .await;

    let pr = forge(&server)
        .reopen_pull_request(&repo(), 12)
        .await
        .expect("reopened");
    assert_eq!(pr.number, 12);
    assert_eq!(pr.state, "open");
    assert_mapped(&pr);
}

#[tokio::test]
async fn comments_on_a_pull_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues/12/comments"))
        .and(body_partial_json(serde_json::json!({"body": "Nice work."})))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": 99,
            "body": "Nice work.",
            "user": {"login": AUTHOR.as_str()},
            "html_url": "https://github.com/owner/repo/pull/12#issuecomment-99",
            "created_at": "2026-01-04T03:04:05Z"
        })))
        .mount(&server)
        .await;

    let comment = forge(&server)
        .comment_pull_request(&repo(), 12, "Nice work.")
        .await
        .expect("comment");
    assert_eq!(comment.id, 99);
    assert_eq!(comment.body, "Nice work.");
    assert_eq!(comment.author.as_deref(), Some(AUTHOR.as_str()));
    assert_eq!(
        comment.url.as_deref(),
        Some("https://github.com/owner/repo/pull/12#issuecomment-99")
    );
    assert_eq!(comment.created_at.as_deref(), Some("2026-01-04T03:04:05Z"));
}

#[tokio::test]
async fn approves_a_pull_request_with_a_review() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/pulls/12/reviews"))
        .and(body_partial_json(
            serde_json::json!({"event": "APPROVE", "body": "Ship it."}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": 80,
            "state": "APPROVED",
            "body": "Ship it.",
            "user": {"login": REVIEWER.as_str()},
            "html_url": "https://github.com/owner/repo/pull/12#pullrequestreview-80",
            "submitted_at": "2026-01-04T03:04:05Z"
        })))
        .mount(&server)
        .await;

    let review = forge(&server)
        .review_pull_request(
            &repo(),
            12,
            rgit_forge::ReviewAction::Approve,
            Some("Ship it."),
        )
        .await
        .expect("review");
    assert_eq!(review.id, Some(80));
    assert_eq!(review.state, "approved");
    assert_eq!(review.body.as_deref(), Some("Ship it."));
    assert_eq!(review.author.as_deref(), Some(REVIEWER.as_str()));
    assert_eq!(
        review.url.as_deref(),
        Some("https://github.com/owner/repo/pull/12#pullrequestreview-80")
    );
}

fn issue_json(number: u64) -> serde_json::Value {
    serde_json::json!({
        "number": number,
        "title": "Fix login",
        "state": "open",
        "user": {"login": AUTHOR.as_str()},
        "body": "Details.",
        "labels": [{"name": "bug"}],
        "assignees": [{"login": REVIEWER.as_str()}],
        "html_url": format!("https://github.com/owner/repo/issues/{number}"),
        "created_at": "2026-01-02T03:04:05Z",
        "updated_at": "2026-01-03T03:04:05Z"
    })
}

fn assert_issue_mapped(issue: &rgit_forge::Issue) {
    assert_eq!(issue.title, "Fix login");
    assert_eq!(issue.state, "open");
    assert_eq!(issue.author.as_deref(), Some(AUTHOR.as_str()));
    assert_eq!(issue.body.as_deref(), Some("Details."));
    assert_eq!(issue.labels, vec!["bug"]);
    assert_eq!(issue.assignees, vec![REVIEWER.clone()]);
    assert_eq!(
        issue.url,
        format!("https://github.com/owner/repo/issues/{}", issue.number)
    );
}

#[tokio::test]
async fn issue_list_drops_pull_requests() {
    let server = MockServer::start().await;
    let mut as_issue = pr_json(12, "feature", "main");
    as_issue["pull_request"] = serde_json::json!({"url": "https://api.github.com/x"});
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues"))
        .and(query_param("state", "open"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            issue_json(7),
            as_issue
        ])))
        .mount(&server)
        .await;

    let listing = forge(&server)
        .issues(&repo(), 1, rgit_forge::IssueState::Open, None, None, None)
        .await
        .expect("issues");
    assert_eq!(listing.items.len(), 1);
    assert_eq!(listing.items[0].number, 7);
    assert_issue_mapped(&listing.items[0]);
}

#[tokio::test]
async fn pulls_one_issue_by_number() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(7)))
        .mount(&server)
        .await;

    let issue = forge(&server).issue(&repo(), 7).await.expect("issue");
    assert_eq!(issue.number, 7);
    assert_issue_mapped(&issue);
}

#[tokio::test]
async fn creates_an_issue() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues"))
        .and(body_partial_json(serde_json::json!({
            "title": "Fix login",
            "body": "Details.",
            "labels": ["bug"],
            "assignees": [REVIEWER.as_str()]
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue_json(7)))
        .mount(&server)
        .await;

    let issue = forge(&server)
        .create_issue(
            &repo(),
            &rgit_forge::CreateIssue {
                title: "Fix login".to_owned(),
                body: Some("Details.".to_owned()),
                labels: vec!["bug".to_owned()],
                assignees: vec![REVIEWER.clone()],
            },
        )
        .await
        .expect("created");
    assert_eq!(issue.number, 7);
    assert_issue_mapped(&issue);
}

#[tokio::test]
async fn closes_and_reopens_an_issue() {
    let server = MockServer::start().await;
    let mut closed = issue_json(7);
    closed["state"] = serde_json::json!("closed");
    Mock::given(method("PATCH"))
        .and(path("/repos/owner/repo/issues/7"))
        .and(body_partial_json(serde_json::json!({"state": "closed"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(closed))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/owner/repo/issues/8"))
        .and(body_partial_json(serde_json::json!({"state": "open"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json(8)))
        .mount(&server)
        .await;

    let issue = forge(&server).close_issue(&repo(), 7).await.expect("closed");
    assert_eq!(issue.state, "closed");
    let issue = forge(&server).reopen_issue(&repo(), 8).await.expect("reopened");
    assert_eq!(issue.state, "open");
}

#[tokio::test]
async fn comments_on_an_issue() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues/7/comments"))
        .and(body_partial_json(serde_json::json!({"body": "Working on it."})))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": 99,
            "body": "Working on it.",
            "user": {"login": AUTHOR.as_str()},
            "html_url": "https://github.com/owner/repo/issues/7#issuecomment-99",
            "created_at": "2026-01-04T03:04:05Z"
        })))
        .mount(&server)
        .await;

    let comment = forge(&server)
        .comment_issue(&repo(), 7, "Working on it.")
        .await
        .expect("comment");
    assert_eq!(comment.id, 99);
    assert_eq!(comment.body, "Working on it.");
    assert_eq!(comment.author.as_deref(), Some(AUTHOR.as_str()));
}

#[tokio::test]
async fn edits_an_issue() {
    let server = MockServer::start().await;
    let mut edited = issue_json(7);
    edited["title"] = serde_json::json!("Fix login faster");
    Mock::given(method("PATCH"))
        .and(path("/repos/owner/repo/issues/7"))
        .and(body_partial_json(serde_json::json!({
            "title": "Fix login faster",
            "labels": ["bug"],
            "assignees": [REVIEWER.as_str()]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(edited))
        .mount(&server)
        .await;

    let issue = forge(&server)
        .edit_issue(
            &repo(),
            7,
            Some("Fix login faster"),
            None,
            Some(&["bug".to_owned()]),
            Some(std::slice::from_ref(&REVIEWER)),
        )
        .await
        .expect("edited");
    assert_eq!(issue.title, "Fix login faster");
    assert_eq!(issue.labels, vec!["bug"]);
}
