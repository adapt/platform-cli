use crate::support::*;
use serde_json::{Value, json};

// Platform's hosted MCP server answers `tools/list` statelessly over Streamable HTTP.
// Its SDK replies with an event stream by default, so most cases use SSE replies.

fn tool(name: &str) -> Value {
    json!({
        "name": name,
        "description": format!("Run {name}. More detail."),
        "inputSchema": {"type":"object","properties":{}},
        "annotations": {"readOnlyHint":true,"destructiveHint":false},
    })
}

fn sse(messages: &[Value]) -> Reply {
    let mut body = String::from(": keepalive\n\n");
    for message in messages {
        body += &format!("event: message\ndata: {message}\n\n");
    }
    Reply::bytes("text/event-stream", body.into_bytes())
}

/// Expects one `tools/list` request with this id and params, then sends `reply`.
fn tools_list(id: u64, params: Value, reply: Reply) -> Handler {
    Box::new(move |request| {
        assert_eq!(request.method, "POST");
        assert_eq!(request.target, "/mcp");
        assert_eq!(request.headers["authorization"], "Bearer test-token");
        assert_eq!(request.headers["x-organization-id"], ORG);
        assert_eq!(request.headers["mcp-protocol-version"], "2025-03-26");
        assert_eq!(request.headers["content-type"], "application/json");
        let accept = &request.headers["accept"];
        assert!(accept.contains("application/json") && accept.contains("text/event-stream"));
        assert_eq!(
            request.json(),
            json!({"jsonrpc":"2.0","id":id,"method":"tools/list","params":params})
        );
        reply
    })
}

fn result(id: u64, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}

fn failure(output: &std::process::Output) -> String {
    assert!(!output.status.success(), "command unexpectedly succeeded");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn reads_the_result_from_an_event_stream() {
    let notification = json!({"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info","data":"hi"}});
    let server = Server::start(vec![tools_list(
        1,
        json!({}),
        sse(&[
            notification,
            result(1, json!({"tools":[tool("whoami"), tool("list_sandboxes")]})),
        ]),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let output = success(&run(dir.path(), &server.url, &["mcp", "tools"]));
    assert_eq!(
        output,
        json!({"tools":[tool("whoami"), tool("list_sandboxes")]})
    );
    server.finish();
}

#[test]
fn reads_a_json_result() {
    let server = Server::start(vec![tools_list(
        1,
        json!({}),
        Reply::json(200, result(1, json!({"tools":[tool("whoami")]}))),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let output = success(&run(dir.path(), &server.url, &["mcp", "tools"]));
    assert_eq!(output["tools"][0]["name"], "whoami");
    server.finish();
}

#[test]
fn follows_cursors_until_the_last_page() {
    let server = Server::start(vec![
        tools_list(
            1,
            json!({}),
            sse(&[result(
                1,
                json!({"tools":[tool("first")],"nextCursor":"page-2"}),
            )]),
        ),
        tools_list(
            2,
            json!({"cursor":"page-2"}),
            sse(&[result(2, json!({"tools":[tool("second")]}))]),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let output = success(&run(dir.path(), &server.url, &["mcp", "tools"]));
    assert_eq!(output, json!({"tools":[tool("first"), tool("second")]}));
    server.finish();
}

#[test]
fn rejects_a_repeated_cursor() {
    let page = |id| sse(&[result(id, json!({"tools":[],"nextCursor":"same"}))]);
    let server = Server::start(vec![
        tools_list(1, json!({}), page(1)),
        tools_list(2, json!({"cursor":"same"}), page(2)),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let stderr = failure(&run(dir.path(), &server.url, &["mcp", "tools"]));
    assert!(stderr.contains("repeated pagination cursor"), "{stderr}");
    server.finish();
}

#[test]
fn fails_on_a_json_rpc_error() {
    let error = json!({"jsonrpc":"2.0","id":1,"error":{"code":-32602,"message":"Invalid params"}});
    let server = Server::start(vec![tools_list(1, json!({}), sse(&[error]))]);
    let dir = tempfile::tempdir().unwrap();
    let stderr = failure(&run(dir.path(), &server.url, &["mcp", "tools"]));
    assert!(
        stderr.contains("MCP tools/list failed: -32602 Invalid params"),
        "{stderr}"
    );
    server.finish();
}

#[test]
fn fails_when_the_stream_ends_before_the_result() {
    let unrelated = result(99, json!({"tools":[tool("not-ours")]}));
    let server = Server::start(vec![tools_list(1, json!({}), sse(&[unrelated]))]);
    let dir = tempfile::tempdir().unwrap();
    let stderr = failure(&run(dir.path(), &server.url, &["mcp", "tools"]));
    assert!(
        stderr.contains("ended before the tools/list result"),
        "{stderr}"
    );
    server.finish();
}

#[test]
fn unauthorized_explains_that_mcp_needs_an_api_key() {
    let server = Server::start(vec![tools_list(
        1,
        json!({}),
        Reply::json(
            401,
            json!({"error":"invalid_token","error_description":"Connect using OAuth, an identity session, or an organization API key."}),
        ),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let stderr = failure(&run(dir.path(), &server.url, &["mcp", "tools"]));
    assert!(
        stderr.contains("401 invalid_token: Connect using OAuth"),
        "{stderr}"
    );
    assert!(
        stderr.contains("sign-in sessions don't work here; use an active organization API key"),
        "{stderr}"
    );
    assert!(!stderr.contains("to sign in again"), "{stderr}");
    server.finish();
}

#[test]
fn organization_mismatch_explains_api_key_scope() {
    let server = Server::start(vec![tools_list(
        1,
        json!({}),
        Reply::json(
            403,
            json!({"error":{"code":"FORBIDDEN","message":"The API key cannot access this organization."}}),
        ),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let stderr = failure(&run(dir.path(), &server.url, &["mcp", "tools"]));
    assert!(stderr.contains("403 FORBIDDEN"), "{stderr}");
    assert!(stderr.contains("only in its own organization"), "{stderr}");
    server.finish();
}

#[test]
fn refuses_a_saved_sign_in_session_without_sending_it() {
    let server = Server::start(vec![]);
    let dir = tempfile::tempdir().unwrap();
    save_profile(dir.path(), &server.url);
    let output = command(dir.path(), &server.url)
        .args(["mcp", "tools"])
        .output()
        .unwrap();
    let stderr = failure(&output);
    assert!(stderr.contains("not CLI sign-in sessions"), "{stderr}");
    assert!(stderr.contains("--token-stdin"), "{stderr}");
    assert!(server.finish().is_empty());
}

#[test]
fn uses_a_saved_api_key() {
    let server = Server::start(vec![Box::new(|request: &Request| {
        assert_eq!(request.headers["authorization"], "Bearer saved-key");
        assert_eq!(request.headers["x-organization-id"], ORG);
        sse(&[result(1, json!({"tools":[tool("whoami")]}))])
    })]);
    let dir = tempfile::tempdir().unwrap();
    let config = json!({"profiles":{"default":{"api_url":server.url,"organization_id":ORG,"credential":{"token":"saved-key","kind":"api_key","expires_at":null}}}});
    std::fs::write(
        dir.path().join("config.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            dir.path().join("config.json"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    let output = command(dir.path(), &server.url)
        .args(["mcp", "tools"])
        .output()
        .unwrap();
    assert_eq!(success(&output)["tools"][0]["name"], "whoami");
    server.finish();
}

#[test]
fn not_signed_in_points_to_api_keys() {
    let server = Server::start(vec![]);
    let dir = tempfile::tempdir().unwrap();
    let output = command(dir.path(), &server.url)
        .args(["mcp", "tools"])
        .output()
        .unwrap();
    let stderr = failure(&output);
    assert!(stderr.contains("Not signed in"), "{stderr}");
    assert!(stderr.contains("organization API key"), "{stderr}");
    assert!(server.finish().is_empty());
}

#[test]
fn name_prints_one_tool_definition() {
    let server = Server::start(vec![tools_list(
        1,
        json!({}),
        sse(&[result(
            1,
            json!({"tools":[tool("whoami"), tool("read_file")]}),
        )]),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let output = success(&run(
        dir.path(),
        &server.url,
        &["mcp", "tools", "read_file"],
    ));
    assert_eq!(output, tool("read_file"));
    server.finish();
}

#[test]
fn unknown_name_fails_with_a_hint() {
    let server = Server::start(vec![tools_list(
        1,
        json!({}),
        sse(&[result(1, json!({"tools":[tool("whoami")]}))]),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let stderr = failure(&run(dir.path(), &server.url, &["mcp", "tools", "nope"]));
    assert!(
        stderr.contains("No Platform MCP tool is named \"nope\""),
        "{stderr}"
    );
    server.finish();
}
