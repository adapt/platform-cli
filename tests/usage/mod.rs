use crate::support::*;
use serde_json::json;

// Platform enforces permissions and applies the filters (see the platform API's usage routes).
// These tests verify that the CLI sends the filters unchanged and passes JSON through.

#[test]
fn list_help_distinguishes_full_sandbox_uuids_from_prefixes() {
    let dir = tempfile::tempdir().unwrap();
    for flag in ["-h", "--help"] {
        let output = run(dir.path(), "http://localhost:1", &["usage", "list", flag]);
        assert!(output.status.success());
        let help = String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(help.contains("Full UUIDs include deleted-sandbox history"));
        assert!(help.contains("prefixes require a non-deleted sandbox"));
        assert!(!help.contains("one existing sandbox"));
    }
}

#[test]
fn list_forwards_filters() {
    let events = json!({"events":[{"id":ID,"kind":"sandbox"}],"asOf":"2026-11-19T16:20:00.000Z","nextCursor":null});
    let server = Server::start(vec![
        step(
            "GET",
            "/v1/usage?limit=50&kind=sandbox".into(),
            None,
            events.clone(),
        ),
        step(
            "GET",
            format!("/v1/usage?limit=50&kind=sandbox&userId={KEY}&days=7&sandboxId=1111abcd"),
            None,
            events.clone(),
        ),
        step(
            "GET",
            format!("/v1/usage?limit=50&kind=sandbox&sandboxId={ID}"),
            None,
            events.clone(),
        ),
        step(
            "GET",
            "/v1/usage?limit=50&kind=model&days=90".into(),
            None,
            events.clone(),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    for args in [
        vec!["usage", "list"],
        vec![
            "usage",
            "ls",
            "--user",
            KEY,
            "--days",
            "7",
            "--sandbox",
            "1111ABCD",
        ],
        vec!["usage", "list", "--sandbox", ID],
        vec!["usage", "list", "--kind", "model", "--days", "90", "--json"],
    ] {
        assert_eq!(success(&run(dir.path(), &server.url, &args)), events);
    }
    server.finish();
}

#[test]
fn list_passes_allocation_estimates_through_as_json_without_notes() {
    let events = json!({"events":[{"id":ID,"kind":"sandbox","usage":{
        "cpuNanoseconds":"1834000000000","residentByteMilliseconds":"3865470566400000",
        "observedMs":3600000,"memoryObservedMs":3600000,"estimatedIntervals":3
    }}],"nextCursor":null});
    let server = Server::start(
        (0..2)
            .map(|_| {
                step(
                    "GET",
                    "/v1/usage?limit=50&kind=sandbox".into(),
                    None,
                    events.clone(),
                )
            })
            .collect(),
    );
    let dir = tempfile::tempdir().unwrap();
    for args in [vec!["usage", "list"], vec!["usage", "list", "--json"]] {
        let output = run(dir.path(), &server.url, &args);
        assert_eq!(success(&output), events);
        assert!(output.stderr.is_empty());
    }
    server.finish();
}

#[test]
fn summary_defaults_to_30_days_and_forwards_filters() {
    let summary = json!({"kind":"sandbox","members":[],"totals":{"runs":0}});
    let server = Server::start(vec![
        step(
            "GET",
            "/v1/usage/summary?kind=sandbox&days=30".into(),
            None,
            summary.clone(),
        ),
        step(
            "GET",
            format!("/v1/usage/summary?kind=model&userId={KEY}&days=7"),
            None,
            summary.clone(),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    for args in [
        vec!["usage", "summary"],
        vec![
            "usage", "summary", "--kind", "model", "--user", KEY, "--days", "7",
        ],
    ] {
        assert_eq!(success(&run(dir.path(), &server.url, &args)), summary);
    }
    server.finish();
}

#[test]
fn list_all_follows_every_page_with_the_same_filters() {
    let server = Server::start(vec![
        step(
            "GET",
            "/v1/usage?limit=1&kind=model&days=30".into(),
            None,
            json!({"events":[{"id":ID}],"asOf":"2026-11-19T16:20:00.000Z","nextCursor":KEY}),
        ),
        step(
            "GET",
            format!("/v1/usage?limit=1&kind=model&days=30&cursor={KEY}"),
            None,
            json!({"events":[{"id":ORG}],"asOf":"2026-11-19T16:20:01.000Z","nextCursor":null}),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let all = success(&run(
        dir.path(),
        &server.url,
        &[
            "usage", "list", "--kind", "model", "--days", "30", "--limit", "1", "--all",
        ],
    ));
    assert_eq!(
        all,
        json!({"events":[{"id":ID},{"id":ORG}],"asOf":"2026-11-19T16:20:00.000Z","nextCursor":null})
    );
    server.finish();
}

#[test]
fn empty_usage_keeps_the_json_envelope() {
    let empty = json!({"events":[],"asOf":"2026-11-19T16:20:00.000Z","nextCursor":null});
    let server = Server::start(vec![step(
        "GET",
        "/v1/usage?limit=50&kind=sandbox&days=7".into(),
        None,
        empty.clone(),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let output = run(dir.path(), &server.url, &["usage", "list", "--days", "7"]);
    assert_eq!(success(&output), empty);
    assert!(output.stderr.is_empty());
    server.finish();
}

#[test]
fn invalid_options_are_rejected_locally() {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        // Platform supports only these fixed windows, so ranges cannot be invalid or reversed.
        vec!["usage", "list", "--days", "14"],
        vec!["usage", "summary", "--days", "1"],
        vec!["usage", "list", "--kind", "audit"],
        vec!["usage", "summary", "--user", "alice@example.com"],
        vec!["usage", "list", "--sandbox", "not-an-id"],
        vec!["usage", "list", "--limit", "101"],
        vec!["usage", "summary", "--all"],
    ] {
        let output = run(dir.path(), "http://localhost:1", &args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
    }
}
