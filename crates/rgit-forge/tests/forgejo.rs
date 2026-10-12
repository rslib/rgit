//! Forgejo HTTP stubs for the user, repository, branch, pull request and
//! check operations.

use rgit_forge::{
    CheckRollup, CreateIssue, CreatePullRequest, CreateRepository, ForgeError, ForgejoClient,
    IssueState, MergeMethod, Provider, PullRequestState, RepoRef, ThreadKind,
};
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn repo() -> RepoRef {
    RepoRef {
        owner: "alice".to_owned(),
        name: "project".to_owned(),
    }
}

fn client(server: &MockServer) -> ForgejoClient {
    ForgejoClient::from_token("secret".to_owned(), Some(&server.uri())).expect("client")
}

fn repository_json() -> serde_json::Value {
    serde_json::json!({
        "name": "project",
        "full_name": "alice/project",
        "html_url": "https://forge.example/alice/project",
        "clone_url": "https://forge.example/alice/project.git",
        "ssh_url": "git@forge.example:alice/project.git",
        "private": true,
        "default_branch": "main",
        "description": "A project"
    })
}

fn create_request() -> CreateRepository {
    CreateRepository {
        name: "project".to_owned(),
        description: Some("A project".to_owned()),
        private: true,
        auto_init: true,
    }
}

#[tokio::test]
async fn repository_read_maps_fields() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/project"))
        .respond_with(ResponseTemplate::new(200).set_body_json(repository_json()))
        .mount(&server)
        .await;
    let found = client(&server).repository(&repo()).await.expect("repo");
    assert_eq!(found.full_name, "alice/project");
    assert_eq!(
        found.clone_url.as_deref(),
        Some("https://forge.example/alice/project.git")
    );
    assert_eq!(
        found.ssh_url.as_deref(),
        Some("git@forge.example:alice/project.git")
    );
    assert!(found.private);
    assert_eq!(found.default_branch.as_deref(), Some("main"));
    assert_eq!(found.description.as_deref(), Some("A project"));
}

#[tokio::test]
async fn repository_requests_send_the_token_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/project"))
        .and(header("authorization", "token secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(repository_json()))
        .expect(1)
        .mount(&server)
        .await;
    client(&server).repository(&repo()).await.expect("repo");
}

#[tokio::test]
async fn repository_missing_is_an_api_error_naming_forgejo() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/project"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let error = client(&server).repository(&repo()).await.expect_err("404");
    assert!(matches!(
        error,
        ForgeError::Api {
            provider: Provider::Forgejo,
            ..
        }
    ));
    assert!(error.to_string().contains("forgejo"));
}

#[tokio::test]
async fn repository_create_under_the_user() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/user/repos"))
        .and(body_partial_json(serde_json::json!({
            "name": "project",
            "description": "A project",
            "private": true,
            "auto_init": true
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(repository_json()))
        .expect(1)
        .mount(&server)
        .await;
    let created = client(&server)
        .create_repository(&create_request(), None)
        .await
        .expect("create");
    assert_eq!(created.name, "project");
}

#[tokio::test]
async fn repository_create_under_an_organization() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/orgs/team/repos"))
        .and(body_partial_json(serde_json::json!({"name": "project"})))
        .respond_with(ResponseTemplate::new(201).set_body_json(repository_json()))
        .expect(1)
        .mount(&server)
        .await;
    client(&server)
        .create_repository(&create_request(), Some("team"))
        .await
        .expect("create");
}

#[tokio::test]
async fn repository_delete_calls_the_repo_route() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/alice/project"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    client(&server)
        .delete_repository(&repo())
        .await
        .expect("delete");
}

#[tokio::test]
async fn repository_branches_page_with_total_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/project/branches"))
        .and(query_param("page", "1"))
        .and(query_param("limit", "100"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-total-count", "150")
                .set_body_json(serde_json::json!([
                    {"name": "main", "protected": true, "commit": {"id": "abc"}}
                ])),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/project/branches"))
        .and(query_param("page", "2"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-total-count", "150")
                .set_body_json(serde_json::json!([
                    {"name": "dev", "protected": false, "commit": {"id": "def"}}
                ])),
        )
        .mount(&server)
        .await;
    let first = client(&server).branches(&repo(), 1).await.expect("page 1");
    assert_eq!(first.items[0].name, "main");
    assert_eq!(first.items[0].sha, "abc");
    assert!(first.items[0].protected);
    assert!(first.more);
    assert_eq!(first.total, Some(150));
    let second = client(&server).branches(&repo(), 2).await.expect("page 2");
    assert_eq!(second.items[0].name, "dev");
    assert!(!second.more);
    assert_eq!(second.total, Some(150));
}

#[tokio::test]
async fn repository_delete_branch_encodes_the_name() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/v1/repos/alice/project/branches/feature%2Fx"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    client(&server)
        .delete_branch(&repo(), "feature/x")
        .await
        .expect("delete");
}

#[tokio::test]
async fn repository_current_user_maps_fields() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/user"))
        .and(header("authorization", "token secret"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "login": "alice",
            "full_name": "Alice A",
            "html_url": "https://forge.example/alice",
            "avatar_url": "https://forge.example/avatar/alice"
        })))
        .mount(&server)
        .await;
    let user = client(&server).current_user().await.expect("user");
    assert_eq!(user.login, "alice");
    assert_eq!(user.name.as_deref(), Some("Alice A"));
    assert_eq!(user.html_url, "https://forge.example/alice");
    assert_eq!(user.avatar_url, "https://forge.example/avatar/alice");
}

fn pull_request_json() -> serde_json::Value {
    serde_json::json!({
        "id": 70,
        "number": 701,
        "index": 7,
        "title": "Improve forge",
        "body": "Adds Forgejo pulls",
        "state": "open",
        "html_url": "https://forge.example/alice/project/pulls/7",
        "draft": false,
        "mergeable": true,
        "merged": false,
        "user": {"login": "alice"},
        "head": {"label": "alice:feature", "ref": "feature", "sha": "abc123"},
        "base": {"label": "alice:main", "ref": "main"},
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-02T00:00:00Z",
        "labels": [{"name": "bug"}],
        "requested_reviewers": [{"login": "bob"}]
    })
}

fn pulls_list_stub(state: &str, body: serde_json::Value) -> Mock {
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/project/pulls"))
        .and(query_param("state", state))
        .and(query_param("page", "1"))
        .and(query_param("limit", "100"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-total-count", "1")
                .set_body_json(body),
        )
}

#[tokio::test]
async fn pull_request_list_maps_fields() {
    let server = MockServer::start().await;
    pulls_list_stub("open", serde_json::json!([pull_request_json()]))
        .mount(&server)
        .await;
    let listing = client(&server)
        .pull_requests(&repo(), 1, PullRequestState::Open, None, None)
        .await
        .expect("pulls");
    assert_eq!(listing.items.len(), 1);
    let pr = &listing.items[0];
    assert_eq!(pr.number, 7);
    assert_eq!(pr.title, "Improve forge");
    assert_eq!(pr.state, "open");
    assert_eq!(pr.head_branch, "feature");
    assert_eq!(pr.base_branch, "main");
    assert_eq!(pr.author.as_deref(), Some("alice"));
    assert_eq!(pr.head_sha.as_deref(), Some("abc123"));
    assert!(!pr.merged);
    assert_eq!(pr.mergeable, Some(true));
    assert_eq!(pr.labels, vec!["bug".to_owned()]);
    assert_eq!(pr.reviewers, vec!["bob".to_owned()]);
    assert!(!listing.more);
    assert_eq!(listing.total, Some(1));
}

#[tokio::test]
async fn pull_request_list_empty_reports_zero() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/project/pulls"))
        .and(query_param("state", "open"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;
    let listing = client(&server)
        .pull_requests(&repo(), 1, PullRequestState::Open, None, None)
        .await
        .expect("pulls");
    assert!(listing.items.is_empty());
    assert!(!listing.more);
    assert_eq!(listing.total, Some(0));
}

#[tokio::test]
async fn pull_request_list_merged_keeps_only_merged() {
    let server = MockServer::start().await;
    let mut open = pull_request_json();
    open["merged"] = serde_json::json!(false);
    let mut merged = pull_request_json();
    merged["merged"] = serde_json::json!(true);
    merged["state"] = serde_json::json!("closed");
    pulls_list_stub("closed", serde_json::json!([open, merged]))
        .mount(&server)
        .await;
    let listing = client(&server)
        .pull_requests(&repo(), 1, PullRequestState::Merged, None, None)
        .await
        .expect("pulls");
    assert_eq!(listing.items.len(), 1);
    assert!(listing.items[0].merged);
}

#[tokio::test]
async fn pull_request_list_filters_head_and_author() {
    let server = MockServer::start().await;
    let mut other = pull_request_json();
    other["user"] = serde_json::json!({"login": "mallory"});
    pulls_list_stub("open", serde_json::json!([pull_request_json(), other]))
        .mount(&server)
        .await;
    let listing = client(&server)
        .pull_requests(
            &repo(),
            1,
            PullRequestState::Open,
            Some("feature"),
            Some("alice"),
        )
        .await
        .expect("pulls");
    assert_eq!(listing.items.len(), 1);
    assert_eq!(listing.items[0].author.as_deref(), Some("alice"));
}

#[tokio::test]
async fn pull_request_get_maps_fields() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/project/pulls/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pull_request_json()))
        .mount(&server)
        .await;
    let pr = client(&server)
        .pull_request(&repo(), 7)
        .await
        .expect("pull");
    assert_eq!(pr.number, 7);
    assert_eq!(pr.title, "Improve forge");
    assert_eq!(pr.html_url, "https://forge.example/alice/project/pulls/7");
    assert_eq!(pr.body.as_deref(), Some("Adds Forgejo pulls"));
}

#[tokio::test]
async fn pull_request_create_posts_head_and_base() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/alice/project/pulls"))
        .and(body_partial_json(serde_json::json!({
            "title": "Improve forge",
            "head": "feature",
            "base": "main",
            "body": "Adds Forgejo pulls"
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(pull_request_json()))
        .expect(1)
        .mount(&server)
        .await;
    let pr = client(&server)
        .create_pull_request(
            &repo(),
            &CreatePullRequest {
                title: "Improve forge".to_owned(),
                head: "feature".to_owned(),
                base: "main".to_owned(),
                body: Some("Adds Forgejo pulls".to_owned()),
                draft: false,
            },
        )
        .await
        .expect("create");
    assert_eq!(pr.number, 7);
}

#[tokio::test]
async fn pull_request_retarget_patches_the_base() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/alice/project/pulls/7"))
        .and(body_partial_json(serde_json::json!({"base": "main"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(pull_request_json()))
        .expect(1)
        .mount(&server)
        .await;
    let pr = client(&server)
        .retarget_pull_request(&repo(), 7, "main")
        .await
        .expect("retarget");
    assert_eq!(pr.base_branch, "main");
}

#[tokio::test]
async fn pull_request_edit_patches_title_and_body() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/alice/project/pulls/7"))
        .and(body_partial_json(serde_json::json!({
            "title": "New title",
            "body": "New body"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(pull_request_json()))
        .expect(1)
        .mount(&server)
        .await;
    client(&server)
        .edit_pull_request(&repo(), 7, Some("New title"), Some("New body"), None)
        .await
        .expect("edit");
}

#[tokio::test]
async fn pull_request_close_patches_state_closed() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/alice/project/pulls/7"))
        .and(body_partial_json(serde_json::json!({"state": "closed"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(pull_request_json()))
        .expect(1)
        .mount(&server)
        .await;
    client(&server)
        .close_pull_request(&repo(), 7)
        .await
        .expect("close");
}

#[tokio::test]
async fn pull_request_reopen_patches_state_open() {
    let server = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/alice/project/pulls/7"))
        .and(body_partial_json(serde_json::json!({"state": "open"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(pull_request_json()))
        .expect(1)
        .mount(&server)
        .await;
    client(&server)
        .reopen_pull_request(&repo(), 7)
        .await
        .expect("reopen");
}

#[tokio::test]
async fn pull_request_merge_posts_the_squash_style() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/alice/project/pulls/7/merge"))
        .and(body_partial_json(serde_json::json!({"Do": "squash"})))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    let mut merged = pull_request_json();
    merged["merged"] = serde_json::json!(true);
    merged["state"] = serde_json::json!("closed");
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/project/pulls/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(merged))
        .mount(&server)
        .await;
    let pr = client(&server)
        .merge_pull_request(&repo(), 7, MergeMethod::Squash, false)
        .await
        .expect("merge");
    assert!(pr.merged);
}

#[tokio::test]
async fn pull_request_merge_posts_the_merge_style() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/alice/project/pulls/7/merge"))
        .and(body_partial_json(serde_json::json!({"Do": "merge"})))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/project/pulls/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pull_request_json()))
        .mount(&server)
        .await;
    client(&server)
        .merge_pull_request(&repo(), 7, MergeMethod::Merge, false)
        .await
        .expect("merge");
}

#[tokio::test]
async fn pull_request_comment_posts_the_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/alice/project/issues/7/comments"))
        .and(body_partial_json(serde_json::json!({"body": "Looks good"})))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": 3,
            "body": "Looks good",
            "user": {"login": "bob"},
            "html_url": "https://forge.example/alice/project/pulls/7#issuecomment-3",
            "created_at": "2026-01-03T00:00:00Z"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let comment = client(&server)
        .comment_pull_request(&repo(), 7, "Looks good")
        .await
        .expect("comment");
    assert_eq!(comment.id, 3);
    assert_eq!(comment.author.as_deref(), Some("bob"));
}

#[tokio::test]
async fn pull_request_checks_roll_up_statuses() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/project/pulls/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pull_request_json()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/project/commits/abc123/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "state": "failure",
            "statuses": [
                {"status": "success", "context": "build", "target_url": "https://ci.example/1"},
                {"status": "success", "context": "lint", "target_url": "https://ci.example/2"},
                {"status": "failure", "context": "test", "target_url": "https://ci.example/3"}
            ]
        })))
        .mount(&server)
        .await;
    let checks = client(&server)
        .pull_request_checks(&repo(), 7)
        .await
        .expect("checks");
    assert_eq!(checks.rollup, CheckRollup::Failing);
    assert_eq!(checks.checks.len(), 3);
    assert_eq!(checks.checks[0].name, "build");
    assert_eq!(checks.checks[0].status, "passing");
    assert_eq!(checks.checks[2].status, "failing");
}

fn issue_json() -> serde_json::Value {
    serde_json::json!({
        "id": 71,
        "number": 7,
        "title": "Fix crash",
        "body": "It crashes on start",
        "state": "open",
        "html_url": "https://forge.example/alice/project/issues/7",
        "user": {"login": "alice"},
        "labels": [{"name": "bug"}],
        "assignees": [{"login": "bob"}],
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-02T00:00:00Z"
    })
}

fn issue_get_stub() -> Mock {
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/project/issues/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json()))
}

#[tokio::test]
async fn issue_list_maps_fields() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/project/issues"))
        .and(query_param("state", "open"))
        .and(query_param("type", "issues"))
        .and(query_param("page", "1"))
        .and(query_param("limit", "100"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-total-count", "1")
                .set_body_json(serde_json::json!([issue_json()])),
        )
        .mount(&server)
        .await;
    let listing = client(&server)
        .issues(&repo(), 1, IssueState::Open, None, None, None)
        .await
        .expect("issues");
    assert_eq!(listing.items.len(), 1);
    let issue = &listing.items[0];
    assert_eq!(issue.number, 7);
    assert_eq!(issue.title, "Fix crash");
    assert_eq!(issue.state, "open");
    assert_eq!(issue.author.as_deref(), Some("alice"));
    assert_eq!(issue.body.as_deref(), Some("It crashes on start"));
    assert_eq!(issue.labels, vec!["bug".to_owned()]);
    assert_eq!(issue.assignees, vec!["bob".to_owned()]);
    assert_eq!(issue.url, "https://forge.example/alice/project/issues/7");
    assert!(!listing.more);
    assert_eq!(listing.total, Some(1));
}

#[tokio::test]
async fn issue_list_passes_the_label_filter() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/project/issues"))
        .and(query_param("labels", "bug"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .expect(1)
        .mount(&server)
        .await;
    let listing = client(&server)
        .issues(&repo(), 1, IssueState::Open, None, Some("bug"), None)
        .await
        .expect("issues");
    assert!(listing.items.is_empty());
}

#[tokio::test]
async fn issue_list_filters_author_and_assignee() {
    let server = MockServer::start().await;
    let mut other = issue_json();
    other["user"] = serde_json::json!({"login": "mallory"});
    other["assignees"] = serde_json::json!([]);
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/project/issues"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([issue_json(), other])),
        )
        .mount(&server)
        .await;
    let listing = client(&server)
        .issues(
            &repo(),
            1,
            IssueState::Open,
            Some("alice"),
            None,
            Some("bob"),
        )
        .await
        .expect("issues");
    assert_eq!(listing.items.len(), 1);
    assert_eq!(listing.items[0].number, 7);
}

#[tokio::test]
async fn issue_list_drops_pull_requests() {
    let server = MockServer::start().await;
    let mut pull = issue_json();
    pull["pull_request"] = serde_json::json!({"merged": false});
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/project/issues"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([pull, issue_json()])),
        )
        .mount(&server)
        .await;
    let listing = client(&server)
        .issues(&repo(), 1, IssueState::Open, None, None, None)
        .await
        .expect("issues");
    assert_eq!(listing.items.len(), 1);
    assert_eq!(listing.items[0].number, 7);
    assert_eq!(listing.total, Some(1));
}

#[tokio::test]
async fn issue_get_maps_fields() {
    let server = MockServer::start().await;
    issue_get_stub().mount(&server).await;
    let issue = client(&server).issue(&repo(), 7).await.expect("issue");
    assert_eq!(issue.number, 7);
    assert_eq!(issue.title, "Fix crash");
    assert_eq!(issue.state, "open");
}

#[tokio::test]
async fn issue_get_refuses_a_pull_request() {
    let server = MockServer::start().await;
    let mut pull = issue_json();
    pull["pull_request"] = serde_json::json!({"merged": false});
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/project/issues/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(pull))
        .mount(&server)
        .await;
    let error = client(&server).issue(&repo(), 7).await.expect_err("pull");
    assert!(error.to_string().contains("pull request"));
}

#[tokio::test]
async fn issue_create_posts_title_body_assignees_and_label_ids() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/project/labels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": 5, "name": "bug"},
            {"id": 6, "name": "feature"}
        ])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/alice/project/issues"))
        .and(body_partial_json(serde_json::json!({
            "title": "Fix crash",
            "body": "It crashes on start",
            "labels": [5],
            "assignees": ["bob"]
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue_json()))
        .expect(1)
        .mount(&server)
        .await;
    let issue = client(&server)
        .create_issue(
            &repo(),
            &CreateIssue {
                title: "Fix crash".to_owned(),
                body: Some("It crashes on start".to_owned()),
                labels: vec!["bug".to_owned()],
                assignees: vec!["bob".to_owned()],
            },
        )
        .await
        .expect("create");
    assert_eq!(issue.number, 7);
}

#[tokio::test]
async fn issue_close_patches_state_closed() {
    let server = MockServer::start().await;
    issue_get_stub().mount(&server).await;
    let mut closed = issue_json();
    closed["state"] = serde_json::json!("closed");
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/alice/project/issues/7"))
        .and(body_partial_json(serde_json::json!({"state": "closed"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(closed))
        .expect(1)
        .mount(&server)
        .await;
    let issue = client(&server)
        .close_issue(&repo(), 7)
        .await
        .expect("close");
    assert_eq!(issue.state, "closed");
}

#[tokio::test]
async fn issue_reopen_patches_state_open() {
    let server = MockServer::start().await;
    issue_get_stub().mount(&server).await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/alice/project/issues/7"))
        .and(body_partial_json(serde_json::json!({"state": "open"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json()))
        .expect(1)
        .mount(&server)
        .await;
    let issue = client(&server)
        .reopen_issue(&repo(), 7)
        .await
        .expect("reopen");
    assert_eq!(issue.state, "open");
}

#[tokio::test]
async fn issue_comment_posts_the_body() {
    let server = MockServer::start().await;
    issue_get_stub().mount(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/v1/repos/alice/project/issues/7/comments"))
        .and(body_partial_json(serde_json::json!({"body": "On it"})))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({
            "id": 4,
            "body": "On it",
            "user": {"login": "bob"},
            "html_url": "https://forge.example/alice/project/issues/7#issuecomment-4",
            "created_at": "2026-01-03T00:00:00Z"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let comment = client(&server)
        .comment_issue(&repo(), 7, "On it")
        .await
        .expect("comment");
    assert_eq!(comment.id, 4);
    assert_eq!(comment.author.as_deref(), Some("bob"));
}

#[tokio::test]
async fn issue_edit_patches_title_body_and_assignees() {
    let server = MockServer::start().await;
    issue_get_stub().mount(&server).await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/alice/project/issues/7"))
        .and(body_partial_json(serde_json::json!({
            "title": "New title",
            "body": "New body",
            "assignees": ["bob"]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json()))
        .expect(1)
        .mount(&server)
        .await;
    client(&server)
        .edit_issue(
            &repo(),
            7,
            Some("New title"),
            Some("New body"),
            None,
            Some(&["bob".to_owned()]),
        )
        .await
        .expect("edit");
}

#[tokio::test]
async fn issue_edit_replaces_labels_by_id() {
    let server = MockServer::start().await;
    issue_get_stub().mount(&server).await;
    Mock::given(method("PATCH"))
        .and(path("/api/v1/repos/alice/project/issues/7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue_json()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/project/labels"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!([{"id": 5, "name": "bug"}])),
        )
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/api/v1/repos/alice/project/issues/7/labels"))
        .and(body_partial_json(serde_json::json!({"labels": [5]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .expect(1)
        .mount(&server)
        .await;
    client(&server)
        .edit_issue(&repo(), 7, None, None, Some(&["bug".to_owned()]), None)
        .await
        .expect("edit");
}

#[tokio::test]
async fn issue_thread_lists_comments_oldest_first() {
    let server = MockServer::start().await;
    issue_get_stub().mount(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repos/alice/project/issues/7/comments"))
        .and(query_param("page", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": 2, "body": "Second", "user": {"login": "bob"},
             "html_url": "https://forge.example/c/2", "created_at": "2026-01-02T00:00:00Z"},
            {"id": 1, "body": "First", "user": {"login": "alice"},
             "html_url": "https://forge.example/c/1", "created_at": "2026-01-01T00:00:00Z"}
        ])))
        .mount(&server)
        .await;
    let thread = client(&server)
        .issue_thread(&repo(), 7)
        .await
        .expect("thread");
    assert_eq!(thread.len(), 2);
    assert_eq!(thread[0].kind, ThreadKind::Comment);
    assert_eq!(thread[0].body, "First");
    assert_eq!(thread[0].author.as_deref(), Some("alice"));
    assert_eq!(thread[1].body, "Second");
}
