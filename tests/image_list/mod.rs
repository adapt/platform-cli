use crate::support::*;
use serde_json::json;

// Platform performs the matching (see `imageQuery` in the platform API's image routes).
// These tests verify that the CLI sends the filters unchanged on every page it requests.

#[test]
fn name_forwards_an_exact_trimmed_name() {
    let server = Server::start(vec![step(
        "GET",
        "/v1/images?limit=50&name=Ubuntu+LTS".into(),
        None,
        json!({"images":[{"id":ID,"name":"Ubuntu LTS"}],"nextCursor":null}),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let result = success(&run(
        dir.path(),
        &server.url,
        &["images", "list", "--name", "\u{FEFF} Ubuntu LTS\t"],
    ));
    assert_eq!(result["images"][0]["id"], ID);
    server.finish();
}

#[test]
fn status_forwards_each_listable_state() {
    let states = ["preparing", "ready", "failed", "deleting"];
    let server = Server::start(
        states
            .iter()
            .map(|status| {
                step(
                    "GET",
                    format!("/v1/images?limit=50&status={status}"),
                    None,
                    json!({"images":[{"id":ID,"status":status}],"nextCursor":null}),
                )
            })
            .collect(),
    );
    let dir = tempfile::tempdir().unwrap();
    for status in states {
        let result = success(&run(
            dir.path(),
            &server.url,
            &["image", "ls", "--status", status],
        ));
        assert_eq!(result["images"][0]["status"], status);
    }
    server.finish();
}

#[test]
fn name_and_status_combine() {
    let server = Server::start(vec![step(
        "GET",
        "/v1/images?limit=50&name=ubuntu&status=ready".into(),
        None,
        json!({"images":[{"id":ID,"name":"ubuntu","status":"ready"}],"nextCursor":null}),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let result = success(&run(
        dir.path(),
        &server.url,
        &["images", "list", "--status", "ready", "--name", "ubuntu"],
    ));
    assert_eq!(result["images"][0]["id"], ID);
    server.finish();
}

#[test]
fn filters_are_kept_on_every_page() {
    let server = Server::start(vec![
        step(
            "GET",
            "/v1/images?limit=1&name=ubuntu&status=ready".into(),
            None,
            json!({"images":[{"id":ID}],"nextCursor":KEY}),
        ),
        step(
            "GET",
            format!("/v1/images?limit=1&name=ubuntu&status=ready&cursor={KEY}"),
            None,
            json!({"images":[{"id":ORG}],"nextCursor":null}),
        ),
        // A manually supplied cursor also keeps the filters.
        step(
            "GET",
            format!("/v1/images?limit=50&name=ubuntu&status=ready&cursor={KEY}"),
            None,
            json!({"images":[{"id":ORG}],"nextCursor":null}),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let all = success(&run(
        dir.path(),
        &server.url,
        &[
            "images", "list", "--name", "ubuntu", "--status", "ready", "--limit", "1", "--all",
        ],
    ));
    assert_eq!(all["images"], json!([{"id":ID},{"id":ORG}]));
    assert!(all["nextCursor"].is_null());
    let next = success(&run(
        dir.path(),
        &server.url,
        &[
            "images", "list", "--name", "ubuntu", "--status", "ready", "--cursor", KEY,
        ],
    ));
    assert_eq!(next["images"], json!([{"id":ORG}]));
    server.finish();
}

#[test]
fn no_matches_names_the_filters() {
    let empty = json!({"images":[],"nextCursor":null});
    let cases = [
        (
            "name=zzz",
            vec!["--name", "zzz"],
            "No images named \"zzz\".",
        ),
        (
            "status=failed",
            vec!["--status", "failed"],
            "No failed images.",
        ),
        (
            "name=zzz&status=ready",
            vec!["--name", "zzz", "--status", "ready"],
            "No ready images named \"zzz\".",
        ),
    ];
    let mut steps: Vec<_> = cases
        .iter()
        .map(|(query, _, _)| {
            step(
                "GET",
                format!("/v1/images?limit=50&{query}"),
                None,
                empty.clone(),
            )
        })
        .collect();
    steps.push(step(
        "GET",
        "/v1/images?limit=50".into(),
        None,
        empty.clone(),
    ));
    let server = Server::start(steps);
    let dir = tempfile::tempdir().unwrap();
    for (_, flags, message) in &cases {
        let mut args = vec!["images", "list"];
        args.extend(flags);
        let output = run(dir.path(), &server.url, &args);
        assert_eq!(success(&output), empty);
        assert_eq!(String::from_utf8_lossy(&output.stderr).trim_end(), *message);
    }
    // An unfiltered empty list keeps its existing output.
    let output = run(dir.path(), &server.url, &["images", "list"]);
    assert_eq!(success(&output), empty);
    assert!(output.stderr.is_empty());
    server.finish();
}

#[test]
fn invalid_filters_are_rejected_locally() {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        // Platform never lists deleted images, so it rejects this state.
        vec!["images", "list", "--status", "deleted"],
        vec!["images", "list", "--status", "Ready"],
        vec!["images", "list", "--status", "bogus"],
        vec!["images", "list", "--status", ""],
        vec!["images", "list", "--name", ""],
        vec!["images", "list", "--name", " \u{FEFF}\u{3000} "],
    ] {
        let output = run(dir.path(), "http://localhost:1", &args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
    }
}
