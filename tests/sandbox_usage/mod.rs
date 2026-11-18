use crate::support::*;
use serde_json::json;

#[test]
fn usage_sends_the_window_and_passes_json_through() {
    let usage = json!({"status":"live","totals":{"cpuNanoseconds":"1513600000000"},"points":[]});
    let server = Server::start(vec![
        step(
            "GET",
            format!("/v1/sandboxes/{ID}/usage?hours=1"),
            None,
            usage.clone(),
        ),
        step(
            "GET",
            format!("/v1/sandboxes/{ID}/usage?hours=24"),
            None,
            usage.clone(),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    for args in [
        vec!["sandbox", "usage", ID],
        vec!["sandbox", "usage", ID, "--hours", "24"],
    ] {
        assert_eq!(success(&run(dir.path(), &server.url, &args)), usage);
    }
    server.finish();
}

#[test]
fn usage_of_an_unknown_sandbox_reports_the_api_error() {
    let server = Server::start(vec![Box::new(|_| {
        Reply::json(
            404,
            json!({"error":{"code":"NOT_FOUND","message":"Sandbox not found."}}),
        )
    })]);
    let dir = tempfile::tempdir().unwrap();
    let output = run(dir.path(), &server.url, &["sandbox", "usage", ID]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("404 NOT_FOUND: Sandbox not found."));
    server.finish();
}
