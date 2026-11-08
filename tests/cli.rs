mod support;
use serde_json::{Value, json};
use std::time::Duration;
use support::*;

mod sandbox_sh;

#[test]
fn help_and_argument_validation() {
    let dir = tempfile::tempdir().unwrap();
    let output = run(dir.path(), "http://localhost:3000", &["--help"]);
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("Usage: adapt"));
    assert!(help.contains("auth"));
    for args in [
        vec!["sandbox", "create", "--cpu", "0"],
        vec!["sandbox", "create", "--image", "alpine", "--image-id", ID],
        vec!["exec", ID],
        vec!["volumes", "quota", ID],
        vec!["endpoints", "bind", ID, ID, "--port", "0"],
    ] {
        assert_eq!(
            run(dir.path(), "http://localhost:3000", &args)
                .status
                .code(),
            Some(2)
        );
    }
}

#[test]
fn sandbox_creation_preserves_literals_and_idempotency() {
    let server = Server::start(vec![Box::new(|r| {
        assert_eq!(r.method, "POST");
        assert_eq!(r.target, "/v1/sandboxes");
        assert_eq!(r.headers["idempotency-key"], KEY);
        assert_eq!(r.headers["x-organization-id"], ORG);
        assert_eq!(r.headers["authorization"], "Bearer test-token");
        assert!(r.headers["user-agent"].starts_with("adapt-cli/"));
        assert_eq!(
            r.json(),
            json!({"image":"alpine:latest","cpu":2,"memoryMiB":256,"diskMiB":128,"ttlSeconds":null,"env":{"SECRET":" a=b\n$HOME "},"labels":{"team":"dev"},"variableSetIds":[ID]})
        );
        Reply::json(202, json!({"id":ID,"status":"provisioning"}))
    })]);
    let dir = tempfile::tempdir().unwrap();
    let output = run(
        dir.path(),
        &server.url,
        &[
            "sandbox",
            "create",
            "--image",
            "alpine:latest",
            "--cpu",
            "2",
            "--memory",
            "256",
            "--disk",
            "128",
            "--no-expiry",
            "--env",
            "SECRET= a=b\n$HOME ",
            "--label",
            "team=dev",
            "--variable-set",
            ID,
            "--idempotency-key",
            KEY,
        ],
    );
    assert_eq!(success(&output)["status"], "provisioning");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("$HOME"));
    server.finish();
}

#[test]
fn follows_pages_and_retains_json_envelope() {
    let server = Server::start(vec![
        step(
            "GET",
            "/v1/sandboxes?limit=2".into(),
            None,
            json!({"sandboxes":[{"id":ID}],"nextCursor":KEY}),
        ),
        step(
            "GET",
            format!("/v1/sandboxes?limit=2&cursor={KEY}"),
            None,
            json!({"sandboxes":[{"id":ORG}],"nextCursor":null}),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let result = success(&run(
        dir.path(),
        &server.url,
        &["sandbox", "list", "--limit", "2", "--all"],
    ));
    assert_eq!(result["sandboxes"].as_array().unwrap().len(), 2);
    assert!(result["nextCursor"].is_null());
    server.finish();
}

#[test]
fn refuses_pagination_cycles() {
    let server = Server::start(vec![
        step(
            "GET",
            "/v1/images?limit=50".into(),
            None,
            json!({"images":[],"nextCursor":KEY}),
        ),
        step(
            "GET",
            format!("/v1/images?limit=50&cursor={KEY}"),
            None,
            json!({"images":[],"nextCursor":KEY}),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let output = run(dir.path(), &server.url, &["images", "list", "--all"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("repeated pagination cursor"));
    server.finish();
}

#[test]
fn reports_resource_error_without_retrying_a_creation() {
    let server = Server::start(vec![Box::new(|r| {
        uuid::Uuid::parse_str(&r.headers["idempotency-key"]).unwrap();
        let mut response = Reply::json(
            503,
            json!({"error":{"code":"SERVICE_UNAVAILABLE","message":"Backend unavailable","resourceId":ID}}),
        );
        response.headers.push(("Retry-After".into(), "10".into()));
        response
    })]);
    let dir = tempfile::tempdir().unwrap();
    let output = run(dir.path(), &server.url, &["volumes", "create"]);
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    for text in [
        "SERVICE_UNAVAILABLE",
        ID,
        "retry after 10",
        "--idempotency-key",
    ] {
        assert!(error.contains(text), "{error}");
    }
    assert!(output.stdout.is_empty());
    server.finish();
}

#[test]
fn named_commands_match_platform_resource_contracts() {
    let cases = vec![
        (
            vec!["sandbox", "stop", ID],
            "POST",
            format!("/v1/sandboxes/{ID}/stop"),
            Some(json!({})),
        ),
        (
            vec!["sandbox", "start", ID],
            "POST",
            format!("/v1/sandboxes/{ID}/start"),
            Some(json!({})),
        ),
        (
            vec!["volumes", "quota", ID, "--unlimited"],
            "PUT",
            format!("/v1/volumes/{ID}/quota"),
            Some(json!({"quotaBytes":null})),
        ),
        (
            vec!["endpoints", "bind", ID, ORG, "--port", "3000"],
            "PUT",
            format!("/v1/ingresses/{ID}/binding"),
            Some(json!({"sandboxId":ORG,"port":3000})),
        ),
        (
            vec!["endpoints", "access", ID, "authenticated"],
            "PUT",
            format!("/v1/ingresses/{ID}/access"),
            Some(json!({"accessMode":"authenticated"})),
        ),
        (
            vec!["endpoints", "token", ID],
            "POST",
            format!("/v1/ingresses/{ID}/access-tokens"),
            Some(json!({})),
        ),
        (
            vec!["endpoints", "revoke-tokens", ID],
            "DELETE",
            format!("/v1/ingresses/{ID}/access-tokens"),
            None,
        ),
        (
            vec!["images", "pull", ID, "--public"],
            "POST",
            format!("/v1/images/{ID}/pull"),
            Some(json!({"registryCredentialId":null})),
        ),
        (
            vec![
                "connections",
                "call",
                ID,
                "read_file",
                "--arguments",
                "{\"path\":\"a b\"}",
            ],
            "POST",
            "/v1/gateway/call".into(),
            Some(json!({"connectionId":ID,"tool":"read_file","arguments":{"path":"a b"}})),
        ),
        (
            vec!["sessions", "revoke", ID],
            "POST",
            "/v1/auth/sessions/revoke".into(),
            Some(json!({"id":ID})),
        ),
        (
            vec!["mcp", "revoke", ID],
            "POST",
            "/v1/auth/mcp/revoke".into(),
            Some(json!({"id":ID})),
        ),
        (
            vec!["keys", "create", "automation"],
            "POST",
            "/v1/auth/api-key/create".into(),
            Some(json!({"configId":"platform","organizationId":ORG,"name":"automation"})),
        ),
        (
            vec!["org", "members", "role", ID, "admin"],
            "POST",
            "/v1/auth/organization/update-member-role".into(),
            Some(json!({"organizationId":ORG,"memberId":ID,"role":"admin"})),
        ),
        (
            vec![
                "org",
                "invitations",
                "create",
                "alice@example.com",
                "--link",
            ],
            "POST",
            "/v1/auth/organization/invite-member?delivery=link".into(),
            Some(
                json!({"organizationId":ORG,"email":"alice@example.com","role":"member","resend":false}),
            ),
        ),
    ];
    let dir = tempfile::tempdir().unwrap();
    for (args, method, path, body) in cases {
        let server = Server::start(vec![step(method, path, body, json!({"ok":true}))]);
        success(&run(dir.path(), &server.url, &args));
        server.finish();
    }
}

#[test]
fn json_file_and_stdin_are_supported_without_secret_echoes() {
    let dir = tempfile::tempdir().unwrap();
    let data = json!({"name":"secrets","env":{"KEY":"don't-print-me"}});
    std::fs::write(dir.path().join("input.json"), data.to_string()).unwrap();
    let server = Server::start(vec![step(
        "POST",
        "/v1/environment-variable-sets".into(),
        Some(data.clone()),
        json!({"id":ID,"environmentNames":["KEY"]}),
    )]);
    let source = format!("@{}", dir.path().join("input.json").display());
    let output = run(
        dir.path(),
        &server.url,
        &["env", "create", "--data", &source],
    );
    success(&output);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("don't-print-me"));
    server.finish();
    let server = Server::start(vec![step(
        "POST",
        "/v1/environment-variable-sets".into(),
        Some(data.clone()),
        json!({"id":ID}),
    )]);
    let mut cmd = command(dir.path(), &server.url);
    cmd.env("ADAPT_TOKEN", "test-token")
        .env("ADAPT_ORG_ID", ORG)
        .args(["env", "create", "--data", "-"]);
    success(&with_stdin(cmd, data.to_string().as_bytes()));
    server.finish();
}

#[test]
fn file_transfer_is_binary_and_query_paths_are_encoded() {
    let bytes = vec![0, 255, 128, b'\n', b'\r'];
    let expected = bytes.clone();
    let server = Server::start(vec![
        Box::new(move |r| {
            assert_eq!(r.method, "PUT");
            assert_eq!(
                r.target,
                format!("/v1/sandboxes/{ID}/files?path=%2Fapp%2Fa+b%3F%23")
            );
            assert_eq!(r.headers["content-type"], "application/octet-stream");
            assert_eq!(r.body, expected);
            Reply {
                status: 204,
                content_type: "application/json",
                headers: vec![],
                body: vec![],
            }
        }),
        Box::new(move |_| Reply::bytes("application/octet-stream", bytes.clone())),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = command(dir.path(), &server.url);
    cmd.env("ADAPT_TOKEN", "test-token")
        .args(["files", "put", ID, "-", "/app/a b?#"]);
    success(&with_stdin(cmd, &[0, 255, 128, b'\n', b'\r']));
    let out = run(
        dir.path(),
        &server.url,
        &["files", "get", ID, "/app/a b?#", "-"],
    );
    assert!(out.status.success());
    assert_eq!(out.stdout, [0, 255, 128, b'\n', b'\r']);
    server.finish();
}

#[test]
fn failed_download_does_not_replace_local_file() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("file.bin");
    std::fs::write(&dest, b"original").unwrap();
    let server = Server::start(vec![Box::new(|_| {
        Reply::json(
            404,
            json!({"error":{"code":"NOT_FOUND","message":"Missing file"}}),
        )
    })]);
    let output = run(
        dir.path(),
        &server.url,
        &[
            "files",
            "get",
            ID,
            "/missing",
            dest.to_str().unwrap(),
            "--force",
        ],
    );
    assert!(!output.status.success());
    assert_eq!(std::fs::read(dest).unwrap(), b"original");
    server.finish();
}

fn device_response() -> Reply {
    Reply::json(
        200,
        json!({"device_code":"private-device-secret","user_code":"ABCD-EFGH","verification_uri":"https://platform.example/device","verification_uri_complete":"https://platform.example/device?user_code=ABCD-EFGH","expires_in":30,"interval":1}),
    )
}

#[test]
fn browser_login_obeys_pending_and_slow_down_and_saves_private_profile() {
    let server = Server::start(vec![
        Box::new(|r| {
            assert_eq!(r.target, "/v1/auth/device/code");
            assert_eq!(r.json(), json!({"client_id":"adapt-cli"}));
            assert!(!r.headers.contains_key("authorization"));
            device_response()
        }),
        Box::new(|r| {
            assert_eq!(
                r.json(),
                json!({"client_id":"adapt-cli","device_code":"private-device-secret","grant_type":"urn:ietf:params:oauth:grant-type:device_code"})
            );
            Reply::json(400, json!({"error":"authorization_pending"}))
        }),
        Box::new(|_| Reply::json(400, json!({"error":"slow_down"}))),
        Box::new(|_| {
            Reply::json(
                200,
                json!({"access_token":"new-session-secret","token_type":"Bearer","expires_in":3600}),
            )
        }),
        Box::new(|r| {
            assert_eq!(r.target, "/v1/auth/get-session");
            assert_eq!(r.headers["authorization"], "Bearer new-session-secret");
            Reply::json(
                200,
                json!({"user":{"email":"alice@example.com"},"session":{"id":ID,"token":"must-not-print","expiresAt":"2030-01-01T00:00:00Z"}}),
            )
        }),
        Box::new(|r| {
            assert_eq!(r.target, "/v1/auth/organization/list");
            Reply::json(200, json!([{"id":ORG,"name":"Org","slug":"org"}]))
        }),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let output = command(dir.path(), &server.url)
        .env("ADAPT_TOKEN", "unrelated-env-token")
        .args(["auth", "login", "--no-browser"])
        .output()
        .unwrap();
    let identity = success(&output);
    assert_eq!(identity["organizationId"], ORG);
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for secret in [
        "private-device-secret",
        "new-session-secret",
        "must-not-print",
        "unrelated-env-token",
    ] {
        assert!(!all.contains(secret));
    }
    assert!(all.contains("ABCD-EFGH"));
    let saved: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("config.json")).unwrap()).unwrap();
    assert_eq!(
        saved["profiles"]["default"]["credential"]["token"],
        "new-session-secret"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(dir.path().join("config.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let requests = server.finish();
    assert!(requests[2].at.duration_since(requests[1].at) >= Duration::from_millis(950));
    assert!(requests[3].at.duration_since(requests[2].at) >= Duration::from_millis(5900));
}

#[test]
fn denied_login_leaves_existing_profile_untouched() {
    let server = Server::start(vec![
        Box::new(|_| device_response()),
        Box::new(|_| Reply::json(400, json!({"error":"access_denied"}))),
    ]);
    let dir = tempfile::tempdir().unwrap();
    save_profile(dir.path(), &server.url);
    let before = std::fs::read(dir.path().join("config.json")).unwrap();
    let output = command(dir.path(), &server.url)
        .args(["auth", "login", "--no-browser"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("denied"));
    assert_eq!(
        std::fs::read(dir.path().join("config.json")).unwrap(),
        before
    );
    server.finish();
}

#[test]
fn login_expiry_stops_polling() {
    let server = Server::start(vec![Box::new(|_| {
        Reply::json(
            200,
            json!({"device_code":"private","user_code":"CODE","verification_uri":"https://platform.example/device","expires_in":1,"interval":2}),
        )
    })]);
    let dir = tempfile::tempdir().unwrap();
    let output = command(dir.path(), &server.url)
        .args(["auth", "login", "--no-browser"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("expired"));
    assert!(!dir.path().join("config.json").exists());
    server.finish();
}

#[test]
fn logout_revokes_saved_session_not_env_override() {
    let server = Server::start(vec![Box::new(|r| {
        assert_eq!(r.target, "/v1/auth/sign-out");
        assert_eq!(r.headers["authorization"], "Bearer saved-secret");
        Reply::json(200, json!({"success":true}))
    })]);
    let dir = tempfile::tempdir().unwrap();
    save_profile(dir.path(), &server.url);
    success(&run(dir.path(), &server.url, &["auth", "logout"]));
    let saved: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("config.json")).unwrap()).unwrap();
    assert!(saved["profiles"]["default"].get("credential").is_none());
    server.finish();
}

#[test]
fn failed_logout_keeps_credential_for_retry() {
    let server = Server::start(vec![Box::new(|_| {
        Reply::json(503, json!({"message":"Unavailable"}))
    })]);
    let dir = tempfile::tempdir().unwrap();
    save_profile(dir.path(), &server.url);
    let output = command(dir.path(), &server.url)
        .args(["auth", "logout"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        std::fs::read_to_string(dir.path().join("config.json"))
            .unwrap()
            .contains("saved-secret")
    );
    server.finish();
}

#[test]
fn credentials_never_follow_origin_overrides_or_redirects() {
    let dir = tempfile::tempdir().unwrap();
    save_profile(dir.path(), "https://original.example");
    let out = command(dir.path(), "https://other.example")
        .args(["whoami"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Not signed in"));
    for path in [
        "https://other.example/v1/whoami",
        "//other.example/v1/whoami",
        "/\\other.example/v1/whoami",
    ] {
        let out = run(dir.path(), "http://localhost:3000", &["api", "GET", path]);
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("absolute API path"));
    }
    let server = Server::start(vec![Box::new(|_| Reply {
        status: 302,
        content_type: "text/plain",
        body: vec![],
        headers: vec![("Location".into(), "https://other.example".into())],
    })]);
    let out = run(dir.path(), &server.url, &["whoami"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("302"));
    server.finish();
}

#[test]
fn api_key_stdin_validates_and_uses_key_organization() {
    let server = Server::start(vec![Box::new(|r| {
        assert_eq!(r.target, "/v1/whoami");
        assert_eq!(r.headers["authorization"], "Bearer imported-key");
        Reply::json(200, json!({"type":"apiKey","organizationId":ORG}))
    })]);
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = command(dir.path(), &server.url);
    cmd.args(["auth", "login", "--token-stdin"]);
    let output = with_stdin(cmd, b"imported-key\n");
    assert_eq!(success(&output)["organizationId"], ORG);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("imported-key"));
    server.finish();
}

#[test]
fn exec_uses_argv_and_reconnects_logs_without_replaying_output() {
    let server = Server::start(vec![
        step(
            "POST",
            format!("/v1/sandboxes/{ID}/exec"),
            Some(json!({"argv":["/bin/sh","-lc","printf hi; exit 7"],"env":{},"stdin":false})),
            json!({"id":ID}),
        ),
        Box::new(|r| {
            assert_eq!(r.target, format!("/v1/execs/{ID}/events?cursor=0"));
            Reply::bytes("text/event-stream",b": heartbeat\r\n\r\nid: 1\r\nevent: stdout\r\ndata: {\"sequence\":1,\"stream\":\"stdout\",\"data\":\"hi\"}\r\n\r\n".to_vec())
        }),
        Box::new(|r| {
            assert_eq!(r.target, format!("/v1/execs/{ID}/events?cursor=1"));
            Reply::bytes("text/event-stream",b"event: stdout\ndata: {\"sequence\":1,\"data\":\"hi\"}\n\nevent: stderr\ndata: {\"sequence\":2,\"data\":\"remote error\"}\n\nevent: exit\ndata: {\"status\":\"exited\",\"exitCode\":7,\"sequence\":2}\n\n".to_vec())
        }),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let out = run(
        dir.path(),
        &server.url,
        &["exec", ID, "--", "/bin/sh", "-lc", "printf hi; exit 7"],
    );
    assert_eq!(out.status.code(), Some(7));
    assert_eq!(out.stdout, b"hi");
    assert!(String::from_utf8_lossy(&out.stderr).contains("remote error"));
    server.finish();
}

#[test]
fn login_keeps_redeemed_session_when_identity_discovery_fails() {
    let server = Server::start(vec![
        Box::new(|_| device_response()),
        Box::new(|_| {
            Reply::json(
                200,
                json!({"access_token":"redeemed-token","token_type":"Bearer","expires_in":3600}),
            )
        }),
        Box::new(|_| Reply::json(503, json!({"message":"Temporary outage"}))),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let out = command(dir.path(), &server.url)
        .args(["auth", "login", "--no-browser"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        std::fs::read_to_string(dir.path().join("config.json"))
            .unwrap()
            .contains("redeemed-token")
    );
    server.finish();
}

#[test]
fn auth_status_does_not_expose_session_tokens() {
    let server = Server::start(vec![Box::new(|_| {
        Reply::json(
            200,
            json!({"session":{"id":ID,"token":"hidden-token"},"user":{"email":"a@example.com"}}),
        )
    })]);
    let dir = tempfile::tempdir().unwrap();
    let out = run(dir.path(), &server.url, &["auth", "status"]);
    assert_eq!(success(&out)["user"]["email"], "a@example.com");
    assert!(!String::from_utf8_lossy(&out.stdout).contains("hidden-token"));
    server.finish();
}

#[test]
fn browser_login_retries_transient_errors_but_stops_on_unknown_protocol_errors() {
    for protocol in [
        "unsupported_grant_type",
        "invalid_client",
        "invalid_grant",
        "expired_token",
    ] {
        let protocol = protocol.to_owned();
        let server = Server::start(vec![
            Box::new(|_| device_response()),
            Box::new(|_| Reply::json(503, json!({"message":"Temporary outage"}))),
            Box::new(move |_| Reply::json(400, json!({"error":protocol}))),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let out = command(dir.path(), &server.url)
            .args(["auth", "login", "--no-browser"])
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(!dir.path().join("config.json").exists());
        let requests = server.finish();
        assert!(requests[2].at.duration_since(requests[1].at) >= Duration::from_millis(1900));
    }
}

#[test]
#[allow(clippy::result_large_err)] // Tungstenite defines the handshake callback's error type.
fn websocket_streams_binary_stdin_and_separate_output_with_remote_exit_status() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let handle = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_hdr_async(
                socket,
                |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                    assert_eq!(request.uri().path(), format!("/v1/execs/{ID}/stream"));
                    assert_eq!(request.uri().query(), Some("after=0&readOnly=false"));
                    assert_eq!(request.headers()["authorization"], "Bearer test-token");
                    assert_eq!(request.headers()["x-organization-id"], ORG);
                    Ok(response)
                },
            )
            .await
            .unwrap();
            ws.send(Message::Text(
                json!({"kind":"attached","session":{"stdin_credit":32768}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            let mut input = Vec::new();
            loop {
                match ws.next().await.unwrap().unwrap() {
                    Message::Binary(data) => input.extend_from_slice(&data),
                    Message::Text(data) => {
                        assert_eq!(
                            serde_json::from_str::<Value>(&data).unwrap(),
                            json!({"kind":"stdin_eof"})
                        );
                        break;
                    }
                    other => panic!("Unexpected message: {other:?}"),
                }
            }
            assert_eq!(input, [0, 255, b'\n']);
            ws.send(Message::Ping(vec![1, 2].into())).await.unwrap();
            ws.send(Message::Text(
                json!({"kind":"stdout","sequence":1,"data_base64":"AP8K"})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            ws.send(Message::Text(
                json!({"kind":"stderr","sequence":2,"data_base64":"ZXJyb3IK"})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            ws.send(Message::Text(
                json!({"kind":"finished","exit":{"code":9,"signal":null,"timed_out":false}})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
            while let Some(Ok(message)) = ws.next().await {
                if matches!(message, Message::Close(_)) {
                    break;
                }
            }
        });
    });
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = command(dir.path(), &url);
    cmd.env("ADAPT_TOKEN", "test-token")
        .env("ADAPT_ORG_ID", ORG)
        .args(["execs", "attach", ID, "--stdin"]);
    let out = with_stdin(cmd, &[0, 255, b'\n']);
    assert_eq!(
        out.status.code(),
        Some(9),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, [0, 255, b'\n']);
    assert!(String::from_utf8_lossy(&out.stderr).contains("error\n"));
    handle.join().unwrap();
}
