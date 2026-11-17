use crate::support::*;
use serde_json::json;

// Platform performs the matching (see `sandboxMatches` in the platform API). These tests
// verify that the CLI sends the filter unchanged on every page it requests.

#[test]
fn query_forwards_a_trimmed_mixed_case_name_fragment() {
    let server = Server::start(vec![step(
        "GET",
        "/v1/sandboxes?limit=50&query=WeB+App".into(),
        None,
        json!({"sandboxes":[{"id":ID,"labels":{"name":"Staging web app"}}],"nextCursor":null}),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let result = success(&run(
        dir.path(),
        &server.url,
        &["sandbox", "list", "--query", "  WeB App "],
    ));
    assert_eq!(result["sandboxes"][0]["id"], ID);
    server.finish();
}

#[test]
fn query_forwards_an_id_prefix() {
    let server = Server::start(vec![step(
        "GET",
        "/v1/sandboxes?limit=50&query=11111111-11".into(),
        None,
        json!({"sandboxes":[{"id":ID}],"nextCursor":null}),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let result = success(&run(
        dir.path(),
        &server.url,
        &["sandboxes", "ls", "--query", "11111111-11"],
    ));
    assert_eq!(result["sandboxes"][0]["id"], ID);
    server.finish();
}

#[test]
fn query_is_kept_on_every_page() {
    let server = Server::start(vec![
        step(
            "GET",
            "/v1/sandboxes?limit=1&query=web".into(),
            None,
            json!({"sandboxes":[{"id":ID}],"nextCursor":KEY}),
        ),
        step(
            "GET",
            format!("/v1/sandboxes?limit=1&query=web&cursor={KEY}"),
            None,
            json!({"sandboxes":[{"id":ORG}],"nextCursor":null}),
        ),
        // A manually supplied cursor also keeps the filter.
        step(
            "GET",
            format!("/v1/sandboxes?limit=50&query=web&cursor={KEY}"),
            None,
            json!({"sandboxes":[{"id":ORG}],"nextCursor":null}),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let all = success(&run(
        dir.path(),
        &server.url,
        &["sandbox", "list", "--query", "web", "--limit", "1", "--all"],
    ));
    assert_eq!(all["sandboxes"], json!([{"id":ID},{"id":ORG}]));
    assert!(all["nextCursor"].is_null());
    let next = success(&run(
        dir.path(),
        &server.url,
        &["sandbox", "list", "--query", "web", "--cursor", KEY],
    ));
    assert_eq!(next["sandboxes"], json!([{"id":ORG}]));
    server.finish();
}

#[test]
fn no_matches_is_not_an_error() {
    let server = Server::start(vec![
        step(
            "GET",
            "/v1/sandboxes?limit=50&query=zzz".into(),
            None,
            json!({"sandboxes":[],"nextCursor":null}),
        ),
        step(
            "GET",
            "/v1/sandboxes?limit=50".into(),
            None,
            json!({"sandboxes":[],"nextCursor":null}),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let output = run(
        dir.path(),
        &server.url,
        &["sandbox", "list", "--query", "zzz"],
    );
    assert_eq!(success(&output), json!({"sandboxes":[],"nextCursor":null}));
    assert!(String::from_utf8_lossy(&output.stderr).contains("No sandboxes match \"zzz\"."));
    // An unfiltered empty list keeps its existing output.
    let output = run(dir.path(), &server.url, &["sandbox", "list"]);
    assert_eq!(success(&output)["sandboxes"], json!([]));
    assert!(output.stderr.is_empty());
    server.finish();
}

#[test]
fn blank_queries_and_unsupported_lists_are_rejected_locally() {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        vec!["sandbox", "list", "--query", ""],
        vec!["sandbox", "list", "--query", "   "],
        vec!["volumes", "list", "--query", "web"],
    ] {
        let output = run(dir.path(), "http://localhost:1", &args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
    }
}
