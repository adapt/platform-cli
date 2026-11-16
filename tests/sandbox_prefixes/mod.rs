use crate::support::*;
use serde_json::json;

#[test]
fn sandbox_commands_send_prefixes_without_listing_resources() {
    for (args, method, path) in [
        (
            vec!["sandbox", "get", "ABCDEF01-1234"],
            "GET",
            "/v1/sandboxes/abcdef01-1234",
        ),
        (vec!["sandbox", "delete", "a"], "DELETE", "/v1/sandboxes/a"),
        (
            vec!["sandbox", "start", "a8f731bc"],
            "POST",
            "/v1/sandboxes/a8f731bc/start",
        ),
        (
            vec!["sandbox", "stop", "a8f731bc"],
            "POST",
            "/v1/sandboxes/a8f731bc/stop",
        ),
    ] {
        let server = Server::start(vec![step(method, path.into(), None, json!({"id":ID}))]);
        let dir = tempfile::tempdir().unwrap();
        success(&run(dir.path(), &server.url, &args));
        server.finish();
    }
}

#[test]
fn lifecycle_waits_poll_the_canonical_id_returned_by_the_operation() {
    for (action, status) in [("start", "running"), ("stop", "stopped")] {
        let server = Server::start(vec![
            step(
                "POST",
                format!("/v1/sandboxes/11111111/{action}"),
                Some(json!({})),
                json!({"id":ID,"status":"provisioning"}),
            ),
            step(
                "GET",
                format!("/v1/sandboxes/{ID}"),
                None,
                json!({"id":ID,"status":status}),
            ),
        ]);
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            success(&run(
                dir.path(),
                &server.url,
                &["sandbox", action, "11111111", "--wait"]
            ))["id"],
            ID
        );
        server.finish();
    }
}

#[test]
fn standalone_wait_resolves_once_and_can_observe_deletion() {
    let server = Server::start(vec![
        step(
            "GET",
            "/v1/sandboxes/11111111".into(),
            None,
            json!({"id":ID,"status":"deleting"}),
        ),
        step(
            "GET",
            format!("/v1/sandboxes/{ID}"),
            None,
            json!({"id":ID,"status":"deleted"}),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        success(&run(
            dir.path(),
            &server.url,
            &["sandbox", "wait", "11111111", "--status", "deleted"]
        ))["id"],
        ID
    );
    server.finish();
}

#[test]
fn exec_sends_prefix_and_preserves_canonical_output_and_idempotency() {
    let server = Server::start(vec![Box::new(|r| {
        assert_eq!(r.method, "POST");
        assert_eq!(r.target, "/v1/sandboxes/abcdef01/exec");
        assert_eq!(r.headers["idempotency-key"], KEY);
        assert_eq!(r.json()["argv"], json!(["uname", "-a"]));
        Reply::json(202, json!({"id":KEY,"sandboxId":ID}))
    })]);
    let dir = tempfile::tempdir().unwrap();
    let result = success(&run(
        dir.path(),
        &server.url,
        &[
            "exec",
            "ABCDEF01",
            "--detach",
            "--idempotency-key",
            KEY,
            "--",
            "uname",
            "-a",
        ],
    ));
    assert_eq!(result["sandboxId"], ID);
    server.finish();
}

#[test]
fn files_use_prefixes_and_upload_output_keeps_the_full_id() {
    let server = Server::start(vec![
        Box::new(|r| {
            assert_eq!(r.method, "PUT");
            assert_eq!(r.target, "/v1/sandboxes/11111111/files?path=%2Ffile");
            assert_eq!(r.body, [0, 255, 10]);
            Reply {
                status: 204,
                content_type: "application/octet-stream",
                headers: vec![("X-Sandbox-Id".into(), ID.into())],
                body: vec![],
            }
        }),
        Box::new(|r| {
            assert_eq!(r.method, "GET");
            assert_eq!(r.target, "/v1/sandboxes/11111111/files?path=%2Ffile");
            Reply::bytes("application/octet-stream", vec![0, 255, 10])
        }),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = command(dir.path(), &server.url);
    cmd.env("ADAPT_TOKEN", "test-token")
        .args(["files", "put", "11111111", "-", "/file"]);
    assert_eq!(success(&with_stdin(cmd, &[0, 255, 10]))["sandboxId"], ID);
    let downloaded = run(
        dir.path(),
        &server.url,
        &["files", "get", "11111111", "/file", "-"],
    );
    assert!(downloaded.status.success());
    assert_eq!(downloaded.stdout, [0, 255, 10]);
    server.finish();
}

#[test]
fn ambiguous_and_missing_prefixes_report_api_errors() {
    for (status, code, message) in [
        (
            409,
            "CONFLICT",
            "Ambiguous sandbox ID; supply more characters.",
        ),
        (404, "NOT_FOUND", "Sandbox not found."),
    ] {
        let server = Server::start(vec![Box::new(move |_| {
            Reply::json(status, json!({"error":{"code":code,"message":message}}))
        })]);
        let dir = tempfile::tempdir().unwrap();
        let output = run(dir.path(), &server.url, &["sandbox", "get", "11111111"]);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains(message));
        assert!(output.stdout.is_empty());
        server.finish();
    }
}

#[test]
fn invalid_references_cursors_and_idempotency_keys_are_rejected_locally() {
    let dir = tempfile::tempdir().unwrap();
    for reference in [
        "",
        "-",
        "ab/c",
        "ab%",
        "ab_",
        " ab",
        "xyz",
        "abcdef01--",
        "abcdef01-1-",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    ] {
        let output = run(
            dir.path(),
            "http://localhost:1",
            &["sandbox", "get", reference],
        );
        assert_eq!(output.status.code(), Some(2), "{reference:?}");
    }
    for args in [
        vec!["sandbox", "list", "--cursor", "11111111"],
        vec!["exec", ID, "--idempotency-key", "11111111", "--", "true"],
    ] {
        assert_eq!(
            run(dir.path(), "http://localhost:1", &args).status.code(),
            Some(2)
        );
    }
}
