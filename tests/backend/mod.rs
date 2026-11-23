use crate::support::*;
use serde_json::json;

#[test]
fn local_unsupported_operations_fail_before_config_credentials_or_bootstrap() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.json"), "invalid cloud config").unwrap();
    let state = dir.path().join("uncreated");
    let cases = [
        vec!["sandbox", "start", ID, "--wait"],
        vec!["sandbox", "usage", ID],
        vec!["sandbox", "wait", ID, "--status", "unsupported"],
        vec!["sandbox", "list", "--limit", "10"],
        vec!["sandbox", "list", "--cursor", ID],
        vec!["sandbox", "get", "1111"],
        vec!["sandbox", "stop", "1111"],
        vec!["sandbox", "delete", "1111"],
        vec!["sandbox", "wait", "1111"],
        vec!["sandbox", "create", "--ttl", "60"],
        vec!["sandbox", "create", "--image-id", ID],
        vec!["sandbox", "create", "--label", "name=test"],
        vec!["sandbox", "create", "--variable-set", ID],
        vec!["sandbox", "create", "--data", "@missing.json"],
        vec!["sandbox", "create", "--run-image"],
        vec!["sandbox", "create", "--idempotency-key", ID],
        vec!["exec", ID, "--detach", "--", "true"],
        vec!["exec", ID, "--idempotency-key", ID, "--", "true"],
        vec!["execs", "get", ID],
        vec!["execs", "cancel", ID],
        vec!["execs", "logs", ID],
        vec!["execs", "attach", ID],
        vec!["files", "put", ID, "missing.txt", "/remote"],
        vec!["files", "get", ID, "/remote", "-"],
        vec!["volumes", "list"],
        vec!["images", "list"],
        vec!["endpoints", "list"],
        vec!["env", "list"],
        vec!["registry", "list"],
        vec!["connections", "list"],
        vec!["models", "list"],
        vec!["keys", "list"],
        vec!["sessions", "list"],
        vec!["mcp", "list"],
        vec!["events"],
        vec!["health"],
        vec!["api", "DELETE", "/v1/sandboxes"],
        vec!["auth", "status"],
        vec!["auth", "login"],
        vec!["whoami"],
        vec!["org", "list"],
    ];
    for args in cases {
        let output = command(dir.path(), "not-a-cloud-origin")
            .env("ADAPT_TOKEN", "cloud-secret")
            .args(["--local", "--local-state-dir"])
            .arg(&state)
            .args(&args)
            .output()
            .unwrap();
        let error = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{args:?}: {error}");
        assert!(
            error.contains("not supported by the local backend"),
            "{args:?}: {error}"
        );
        assert!(output.stdout.is_empty());
        assert!(!error.contains("cloud-secret"));
        assert!(!state.exists(), "{args:?} started a runtime");
    }
}

#[test]
fn target_flags_are_global_and_mutually_exclusive() {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        vec!["sandbox", "usage", ID, "--local"],
        vec!["sandbox", "--local", "usage", ID],
        vec!["--local", "sandbox", "usage", ID],
    ] {
        let output = run(dir.path(), "invalid", &args);
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("sandbox usage is not supported by the local backend")
        );
    }
    let output = run(
        dir.path(),
        "invalid",
        &["--cloud", "sandbox", "list", "--local"],
    );
    assert_eq!(output.status.code(), Some(2));

    let server = Server::start(vec![Box::new(|r| {
        assert_eq!(r.target, "/v1/sandboxes?limit=50");
        assert_eq!(r.headers["authorization"], "Bearer test-token");
        Reply::json(200, json!({"sandboxes":[],"nextCursor":null}))
    })]);
    let output = command(dir.path(), &server.url)
        .env("ADAPT_TOKEN", "test-token")
        .env("ADAPT_LOCAL_STATE_DIR", "/unused/local/state")
        .args(["sandbox", "list", "--cloud"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    server.finish();
}

#[test]
fn local_logout_still_removes_only_the_saved_credential() {
    let dir = tempfile::tempdir().unwrap();
    let config = json!({"profiles":{"default":{"api_url":"http://localhost:3000","organization_id":ORG,"credential":{"kind":"session","token":"saved-secret"}}}});
    let path = dir.path().join("config.json");
    std::fs::write(&path, config.to_string()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let output = run(
        dir.path(),
        "http://localhost:3000",
        &["auth", "logout", "--local"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !std::fs::read_to_string(path)
            .unwrap()
            .contains("saved-secret")
    );
}
