use crate::support::*;
use serde_json::{Value, json};

// Platform's identity routes take full UUIDs only, so the CLI resolves names, and
// updates replace the whole configuration, so the CLI merges changes onto it.

const OTHER: &str = "44444444-4444-4444-8444-444444444444";
const MEMBER: &str = "55555555-5555-4555-8555-555555555555";
const CONNECTION: &str = "66666666-6666-4666-8666-666666666666";
const SESSION: &str = "77777777-7777-4777-8777-777777777777";
const TOKEN: &str = "adapt_identity_AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

/// Expects one authenticated request and answers with `status` and `reply`.
fn call(
    method: &'static str,
    target: String,
    body: Option<Value>,
    status: u16,
    reply: Option<Value>,
) -> Handler {
    Box::new(move |request| {
        assert_eq!(request.method, method);
        assert_eq!(request.target, target);
        assert_eq!(request.headers["authorization"], "Bearer test-token");
        assert_eq!(request.headers["x-organization-id"], ORG);
        if let Some(body) = body {
            assert_eq!(request.json(), body);
        }
        match reply {
            Some(reply) => Reply::json(status, reply),
            None => Reply {
                status,
                content_type: "application/json",
                headers: vec![],
                body: vec![],
            },
        }
    })
}

fn detail() -> Value {
    json!({
        "id": ID, "organizationId": ORG, "name": "deploy-bot", "description": "Deploys",
        "enabled": true, "version": 3,
        "permissions": {"sandboxes": ["read", "write"], "volumes": ["read", "write"], "execs": ["read"], "ingresses": []},
        "connections": [{"connectionId": CONNECTION, "account": "shared", "tools": ["search"]}],
        "environmentVariableSetIds": [OTHER], "maxSessionSeconds": 3600,
        "createdAt": "2026-11-01T00:00:00.000Z", "updatedAt": "2026-11-01T00:00:00.000Z",
        "canAssume": true, "canManage": true,
        "grants": [{"memberId": MEMBER, "canAssume": true, "canManage": true, "name": "Ana", "email": "ana@example.com"}],
        "accounts": [], "connectionNames": {}, "environmentVariableSets": {},
    })
}

/// The update body the CLI should send: the current configuration without
/// response-only fields or grants, plus `changes`.
fn saved(changes: Value) -> Value {
    let mut body = json!({
        "name": "deploy-bot", "description": "Deploys", "enabled": true,
        "permissions": {"sandboxes": ["read", "write"], "volumes": ["read", "write"], "execs": ["read"], "ingresses": []},
        "connections": [{"connectionId": CONNECTION, "account": "shared", "tools": ["search"]}],
        "environmentVariableSetIds": [OTHER], "maxSessionSeconds": 3600, "version": 3,
    });
    for (key, value) in changes.as_object().unwrap() {
        body[key] = value.clone();
    }
    body
}

fn listed(rows: Value, next: Option<&str>) -> Value {
    json!({"identities": rows, "canCreate": true, "nextCursor": next})
}

fn failure(output: &std::process::Output) -> String {
    assert!(!output.status.success(), "command unexpectedly succeeded");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn list_follows_every_page_without_a_limit() {
    let server = Server::start(vec![
        call(
            "GET",
            "/v1/identities".into(),
            None,
            200,
            Some(listed(json!([{"id": ID, "name": "a"}]), Some(OTHER))),
        ),
        call(
            "GET",
            format!("/v1/identities?cursor={OTHER}"),
            None,
            200,
            Some(listed(json!([{"id": OTHER, "name": "b"}]), None)),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let output = success(&run(
        dir.path(),
        &server.url,
        &["identities", "list", "--all"],
    ));
    assert_eq!(output["identities"].as_array().unwrap().len(), 2);
    server.finish();
}

#[test]
fn list_rejects_a_limit_platform_does_not_accept() {
    let dir = tempfile::tempdir().unwrap();
    let stderr = failure(&run(
        dir.path(),
        "http://127.0.0.1:9",
        &["identities", "list", "--limit", "5"],
    ));
    assert!(stderr.contains("fixed page size"), "{stderr}");
}

#[test]
fn a_name_resolves_ignoring_case_and_surrounding_space() {
    let server = Server::start(vec![
        call(
            "GET",
            "/v1/identities".into(),
            None,
            200,
            Some(listed(
                json!([{"id": OTHER, "name": "other"}, {"id": ID, "name": "deploy-bot"}]),
                None,
            )),
        ),
        call(
            "GET",
            format!("/v1/identities/{ID}"),
            None,
            200,
            Some(detail()),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let output = success(&run(
        dir.path(),
        &server.url,
        &["identities", "get", " Deploy-Bot "],
    ));
    assert_eq!(output["id"], ID);
    server.finish();
}

#[test]
fn a_full_uuid_is_used_without_a_lookup() {
    let server = Server::start(vec![call(
        "GET",
        format!("/v1/identities/{ID}"),
        None,
        200,
        Some(detail()),
    )]);
    let dir = tempfile::tempdir().unwrap();
    success(&run(dir.path(), &server.url, &["identities", "get", ID]));
    assert_eq!(server.finish().len(), 1);
}

#[test]
fn an_unknown_name_fails_before_any_detail_request() {
    let server = Server::start(vec![call(
        "GET",
        "/v1/identities".into(),
        None,
        200,
        Some(listed(json!([{"id": ID, "name": "deploy-bot"}]), None)),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let stderr = failure(&run(
        dir.path(),
        &server.url,
        &["identities", "get", "missing"],
    ));
    assert!(
        stderr.contains("No identity named \"missing\" that you can assume or manage"),
        "{stderr}"
    );
    server.finish();
}

#[test]
fn an_ambiguous_name_lists_the_matching_ids() {
    let server = Server::start(vec![call(
        "GET",
        "/v1/identities".into(),
        None,
        200,
        Some(listed(
            json!([{"id": ID, "name": "Bot"}, {"id": OTHER, "name": "bot"}]),
            None,
        )),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let stderr = failure(&run(dir.path(), &server.url, &["identities", "get", "bot"]));
    assert!(
        stderr.contains(&format!("names 2 identities ({ID}, {OTHER}); use an ID")),
        "{stderr}"
    );
    server.finish();
}

#[test]
fn update_merges_changes_onto_the_current_configuration() {
    let server = Server::start(vec![
        call(
            "GET",
            format!("/v1/identities/{ID}"),
            None,
            200,
            Some(detail()),
        ),
        call(
            "PUT",
            format!("/v1/identities/{ID}"),
            Some(saved(
                json!({"maxSessionSeconds": 7200, "description": "New"}),
            )),
            200,
            Some(detail()),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let output = run(
        dir.path(),
        &server.url,
        &[
            "identities",
            "update",
            ID,
            "--data",
            r#"{"maxSessionSeconds":7200,"description":"New"}"#,
        ],
    );
    success(&output);
    assert!(String::from_utf8_lossy(&output.stderr).contains("ends an identity's active sessions"));
    server.finish();
}

#[test]
fn update_passes_grants_through_only_when_supplied() {
    let grants = json!([{"memberId": MEMBER, "canAssume": true, "canManage": false}]);
    let server = Server::start(vec![
        call(
            "GET",
            format!("/v1/identities/{ID}"),
            None,
            200,
            Some(detail()),
        ),
        call(
            "PUT",
            format!("/v1/identities/{ID}"),
            Some(saved(json!({ "grants": grants }))),
            200,
            Some(detail()),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let data = json!({ "grants": grants }).to_string();
    success(&run(
        dir.path(),
        &server.url,
        &["identities", "update", ID, "--data", &data],
    ));
    server.finish();
}

#[test]
fn update_refuses_a_stale_version_without_saving() {
    let server = Server::start(vec![call(
        "GET",
        format!("/v1/identities/{ID}"),
        None,
        200,
        Some(detail()),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let stderr = failure(&run(
        dir.path(),
        &server.url,
        &[
            "identities",
            "update",
            ID,
            "--data",
            r#"{"version":2,"enabled":false}"#,
        ],
    ));
    assert!(stderr.contains("version 3, not 2"), "{stderr}");
    assert_eq!(server.finish().len(), 1);
}

#[test]
fn a_server_version_conflict_is_reported() {
    let server = Server::start(vec![
        call(
            "GET",
            format!("/v1/identities/{ID}"),
            None,
            200,
            Some(detail()),
        ),
        call(
            "PUT",
            format!("/v1/identities/{ID}"),
            None,
            409,
            Some(
                json!({"error": {"code": "CONFLICT", "message": "This identity changed. Reload it before saving."}}),
            ),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let stderr = failure(&run(
        dir.path(),
        &server.url,
        &["identities", "disable", ID],
    ));
    assert!(
        stderr.contains("409 CONFLICT: This identity changed"),
        "{stderr}"
    );
    server.finish();
}

#[test]
fn enable_and_disable_change_only_enabled() {
    for (command, enabled) in [("disable", false), ("enable", true)] {
        let mut current = detail();
        current["enabled"] = json!(!enabled);
        let server = Server::start(vec![
            call(
                "GET",
                format!("/v1/identities/{ID}"),
                None,
                200,
                Some(current),
            ),
            call(
                "PUT",
                format!("/v1/identities/{ID}"),
                Some(saved(json!({ "enabled": enabled }))),
                200,
                Some(detail()),
            ),
        ]);
        let dir = tempfile::tempdir().unwrap();
        success(&run(dir.path(), &server.url, &["identities", command, ID]));
        server.finish();
    }
}

#[test]
fn delete_confirms_the_deleted_id() {
    let server = Server::start(vec![call(
        "DELETE",
        format!("/v1/identities/{ID}"),
        None,
        204,
        None,
    )]);
    let dir = tempfile::tempdir().unwrap();
    let output = success(&run(dir.path(), &server.url, &["identities", "delete", ID]));
    assert_eq!(output, json!({ "deleted": ID }));
    server.finish();
}

#[test]
fn permissions_without_changes_only_reads() {
    let server = Server::start(vec![call(
        "GET",
        format!("/v1/identities/{ID}"),
        None,
        200,
        Some(detail()),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let output = success(&run(
        dir.path(),
        &server.url,
        &["identities", "permissions", ID],
    ));
    assert_eq!(output["version"], 3);
    assert_eq!(output["permissions"]["sandboxes"], json!(["read", "write"]));
    assert_eq!(server.finish().len(), 1);
}

#[test]
fn permissions_allow_and_deny_save_the_merged_permissions() {
    let permissions = json!({
        "sandboxes": ["read", "write"], "volumes": ["read"], "execs": ["read", "write"],
        "ingresses": [], "images": ["read", "use"],
    });
    let mut updated = detail();
    updated["permissions"] = permissions.clone();
    let server = Server::start(vec![
        call(
            "GET",
            format!("/v1/identities/{ID}"),
            None,
            200,
            Some(detail()),
        ),
        call(
            "PUT",
            format!("/v1/identities/{ID}"),
            Some(saved(json!({ "permissions": permissions }))),
            200,
            Some(updated),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let output = success(&run(
        dir.path(),
        &server.url,
        &[
            "identities",
            "permissions",
            ID,
            "--allow",
            "commands:write",
            "--allow",
            "images:use",
            "--deny",
            "volumes:write",
        ],
    ));
    assert_eq!(output["permissions"]["images"], json!(["read", "use"]));
    server.finish();
}

#[test]
fn permissions_reject_an_action_the_resource_does_not_have() {
    let dir = tempfile::tempdir().unwrap();
    let output = run(
        dir.path(),
        "http://127.0.0.1:9",
        &["identities", "permissions", ID, "--allow", "sandboxes:use"],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("sandboxes accepts read, write"));
}

#[test]
fn access_finds_a_member_by_email_across_pages() {
    let members = |offset: u64, email: &str, id: &str| {
        call(
            "GET",
            format!(
                "/v1/auth/organization/list-members?organizationId={ORG}&limit=50&offset={offset}"
            ),
            None,
            200,
            Some(
                json!({"members": [{"id": id, "userId": OTHER, "user": {"email": email, "name": "x"}}], "total": 2}),
            ),
        )
    };
    let server = Server::start(vec![
        members(0, "someone@example.com", OTHER),
        members(1, "Ana@Example.com", MEMBER),
        call(
            "GET",
            format!("/v1/identities/{ID}"),
            None,
            200,
            Some(detail()),
        ),
        call(
            "PUT",
            format!("/v1/identities/{ID}/grants"),
            Some(json!({"memberId": MEMBER, "canAssume": true, "canManage": false})),
            204,
            None,
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let output = success(&run(
        dir.path(),
        &server.url,
        &["identities", "access", ID, "ana@example.com", "--assume"],
    ));
    assert_eq!(output["memberId"], MEMBER);
    assert_eq!(output["canAssume"], true);
    server.finish();
}

#[test]
fn access_without_flags_removes_a_member_by_id() {
    let server = Server::start(vec![
        call(
            "GET",
            format!("/v1/identities/{ID}"),
            None,
            200,
            Some(detail()),
        ),
        call(
            "PUT",
            format!("/v1/identities/{ID}/grants"),
            Some(json!({"memberId": MEMBER, "canAssume": false, "canManage": false})),
            204,
            None,
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    success(&run(
        dir.path(),
        &server.url,
        &["identities", "access", ID, MEMBER],
    ));
    server.finish();
}

#[test]
fn access_explains_that_grants_need_a_member_id() {
    let server = Server::start(vec![
        call(
            "GET",
            format!("/v1/identities/{ID}"),
            None,
            200,
            Some(detail()),
        ),
        call(
            "PUT",
            format!("/v1/identities/{ID}/grants"),
            None,
            400,
            Some(
                json!({"error": {"code": "INVALID_REQUEST", "message": "Choose a current member of this organization."}}),
            ),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let stderr = failure(&run(
        dir.path(),
        &server.url,
        &["identities", "access", ID, OTHER, "--manage"],
    ));
    assert!(stderr.contains("not a user ID"), "{stderr}");
    server.finish();
}

#[test]
fn account_selects_and_clears_a_personal_connection() {
    for (args, account) in [
        (vec!["--use", OTHER], json!(OTHER)),
        (vec!["--clear"], Value::Null),
    ] {
        let server = Server::start(vec![call(
            "PUT",
            format!("/v1/identities/{ID}/accounts/{CONNECTION}"),
            Some(json!({ "accountId": account })),
            204,
            None,
        )]);
        let dir = tempfile::tempdir().unwrap();
        let mut command = vec!["identities", "account", ID, CONNECTION];
        command.extend(args);
        assert_eq!(
            success(&run(dir.path(), &server.url, &command))["accountId"],
            account
        );
        server.finish();
    }
}

#[test]
fn account_requires_a_choice() {
    let dir = tempfile::tempdir().unwrap();
    let output = run(
        dir.path(),
        "http://127.0.0.1:9",
        &["identities", "account", ID, CONNECTION],
    );
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn session_start_prints_the_token_once_on_stdout_only() {
    let session = json!({"id": SESSION, "identityId": ID, "label": "ci", "expiresAt": "2026-11-18T00:15:00.000Z", "active": true});
    let server = Server::start(vec![call(
        "POST",
        format!("/v1/identities/{ID}/sessions"),
        Some(json!({"label": "ci", "durationSeconds": 900})),
        201,
        Some(json!({ "session": session, "token": TOKEN })),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let output = run(
        dir.path(),
        &server.url,
        &[
            "identities",
            "sessions",
            "start",
            ID,
            "--label",
            " ci ",
            "--duration",
            "900",
        ],
    );
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(success(&output)["token"], TOKEN);
    assert!(stderr.contains("shown once"), "{stderr}");
    assert!(!stderr.contains(TOKEN));
    assert!(
        !std::fs::read_to_string(dir.path().join("config.json")).is_ok_and(|c| c.contains(TOKEN))
    );
    server.finish();
}

#[test]
fn session_start_rejects_out_of_range_durations_and_blank_labels() {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        vec!["--label", "ci", "--duration", "60"],
        vec!["--label", "  "],
    ] {
        let mut command = vec!["identities", "sessions", "start", ID];
        command.extend(args);
        assert_eq!(
            run(dir.path(), "http://127.0.0.1:9", &command)
                .status
                .code(),
            Some(2)
        );
    }
}

#[test]
fn session_revoke_confirms_the_session_was_revoked() {
    let revoked = json!({"id": SESSION, "label": "ci", "active": false, "revokedAt": "2026-11-17T00:00:00.000Z"});
    let server = Server::start(vec![
        call(
            "DELETE",
            format!("/v1/identities/{ID}/sessions/{SESSION}"),
            None,
            204,
            None,
        ),
        call(
            "GET",
            format!("/v1/identities/{ID}/sessions"),
            None,
            200,
            Some(json!({"sessions": [revoked], "nextCursor": null})),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let output = success(&run(
        dir.path(),
        &server.url,
        &["identities", "sessions", "revoke", ID, SESSION],
    ));
    assert_eq!(output["revokedAt"], "2026-11-17T00:00:00.000Z");
    server.finish();
}

#[test]
fn session_revoke_reports_a_session_platform_did_not_find() {
    let server = Server::start(vec![
        call(
            "DELETE",
            format!("/v1/identities/{ID}/sessions/{SESSION}"),
            None,
            204,
            None,
        ),
        call(
            "GET",
            format!("/v1/identities/{ID}/sessions"),
            None,
            200,
            Some(json!({"sessions": [], "nextCursor": null})),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let stderr = failure(&run(
        dir.path(),
        &server.url,
        &["identities", "sessions", "revoke", ID, SESSION],
    ));
    assert!(
        stderr.contains(&format!("no session {SESSION} that you can revoke")),
        "{stderr}"
    );
    server.finish();
}

#[test]
fn identity_commands_explain_that_they_need_a_browser_sign_in() {
    let server = Server::start(vec![call(
        "GET",
        "/v1/identities".into(),
        None,
        403,
        Some(
            json!({"error": {"code": "FORBIDDEN", "message": "Use your signed-in account to manage or assume identities."}}),
        ),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let stderr = failure(&run(dir.path(), &server.url, &["identities", "list"]));
    assert!(
        stderr.contains("not an API key or identity token"),
        "{stderr}"
    );
    server.finish();
}

#[test]
fn an_expired_identity_token_points_to_a_new_session_not_a_login() {
    let server = Server::start(vec![call(
        "GET",
        "/v1/sandboxes?limit=50".into(),
        None,
        401,
        Some(
            json!({"error": {"code": "UNAUTHENTICATED", "message": "This identity session expired or its access changed. Start a new session."}}),
        ),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let stderr = failure(&run(dir.path(), &server.url, &["sandbox", "list"]));
    assert!(
        stderr.contains("adapt identities sessions start"),
        "{stderr}"
    );
    assert!(!stderr.contains("adapt auth login"), "{stderr}");
    server.finish();
}

#[test]
fn a_change_to_current_values_is_not_saved_so_sessions_continue() {
    let server = Server::start(vec![call(
        "GET",
        format!("/v1/identities/{ID}"),
        None,
        200,
        Some(detail()),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let output = run(dir.path(), &server.url, &["identities", "enable", ID]);
    assert_eq!(success(&output)["id"], ID);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Nothing was saved"), "{stderr}");
    assert!(
        !stderr.contains("ends an identity's active sessions"),
        "{stderr}"
    );
    assert_eq!(server.finish().len(), 1);
}

#[test]
fn allowing_a_held_permission_is_not_saved() {
    let server = Server::start(vec![call(
        "GET",
        format!("/v1/identities/{ID}"),
        None,
        200,
        Some(detail()),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let output = run(
        dir.path(),
        &server.url,
        &[
            "identities",
            "permissions",
            ID,
            "--allow",
            "sandboxes:write",
            "--deny",
            "images:use",
        ],
    );
    assert_eq!(
        success(&output)["permissions"]["sandboxes"],
        json!(["read", "write"])
    );
    assert_eq!(server.finish().len(), 1);
}

#[test]
fn other_401_messages_that_mention_identity_sessions_keep_the_login_hint() {
    let server = Server::start(vec![call(
        "GET",
        "/v1/sandboxes?limit=50".into(),
        None,
        401,
        Some(
            json!({"error": "invalid_token", "error_description": "Connect using OAuth, an identity session, or an organization API key."}),
        ),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let stderr = failure(&run(dir.path(), &server.url, &["sandbox", "list"]));
    assert!(stderr.contains("adapt auth login"), "{stderr}");
    assert!(!stderr.contains("identities sessions start"), "{stderr}");
    server.finish();
}

#[test]
fn access_that_already_matches_is_not_saved() {
    let server = Server::start(vec![call(
        "GET",
        format!("/v1/identities/{ID}"),
        None,
        200,
        Some(detail()),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let output = run(
        dir.path(),
        &server.url,
        &["identities", "access", ID, MEMBER, "--assume", "--manage"],
    );
    assert_eq!(success(&output)["canManage"], true);
    assert!(String::from_utf8_lossy(&output.stderr).contains("Nothing was saved"));
    assert_eq!(server.finish().len(), 1);
}

#[test]
fn access_from_a_non_manager_is_left_to_platform() {
    let mut current = detail();
    current["canManage"] = json!(false);
    current["grants"] = json!([]);
    let server = Server::start(vec![
        call(
            "GET",
            format!("/v1/identities/{ID}"),
            None,
            200,
            Some(current),
        ),
        call(
            "PUT",
            format!("/v1/identities/{ID}/grants"),
            Some(json!({"memberId": MEMBER, "canAssume": false, "canManage": false})),
            403,
            Some(
                json!({"error": {"code": "FORBIDDEN", "message": "Ask an identity manager to make this change."}}),
            ),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let stderr = failure(&run(
        dir.path(),
        &server.url,
        &["identities", "access", ID, MEMBER],
    ));
    assert!(stderr.contains("Ask an identity manager"), "{stderr}");
    server.finish();
}

#[test]
fn create_from_flags_fills_the_required_permission_resources() {
    let server = Server::start(vec![call(
        "POST",
        "/v1/identities".into(),
        Some(json!({
            "name": "Deploy Bot", "description": "Deploys", "maxSessionSeconds": 1800,
            "permissions": {"volumes": [], "sandboxes": ["read", "write"], "execs": [], "ingresses": [], "images": ["read", "use"]},
        })),
        201,
        Some(detail()),
    )]);
    let dir = tempfile::tempdir().unwrap();
    success(&run(
        dir.path(),
        &server.url,
        &[
            "identities",
            "create",
            "--name",
            " Deploy Bot ",
            "--description",
            "Deploys",
            "--allow",
            "sandboxes:write",
            "--allow",
            "images:use",
            "--max-session",
            "1800",
        ],
    ));
    server.finish();
}

#[test]
fn create_flags_override_fields_from_data() {
    let connections =
        json!([{"connectionId": CONNECTION, "account": "shared", "tools": ["search"]}]);
    let server = Server::start(vec![call(
        "POST",
        "/v1/identities".into(),
        Some(json!({
            "name": "new", "connections": connections,
            "permissions": {"sandboxes": ["read"], "volumes": [], "execs": [], "ingresses": [], "gateway": ["read", "use"]},
        })),
        201,
        Some(detail()),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let data =
        json!({"name": "old", "connections": connections, "permissions": {"sandboxes": ["read"]}})
            .to_string();
    success(&run(
        dir.path(),
        &server.url,
        &[
            "identities",
            "create",
            "--data",
            &data,
            "--name",
            "new",
            "--allow",
            "mcp-tools:use",
        ],
    ));
    server.finish();
}

#[test]
fn create_needs_a_name_or_data() {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        vec![],
        vec!["--name", "   "],
        vec!["--name", "x", "--max-session", "60"],
    ] {
        let mut command = vec!["identities", "create"];
        command.extend(args);
        assert_eq!(
            run(dir.path(), "http://127.0.0.1:9", &command)
                .status
                .code(),
            Some(2)
        );
    }
}

#[cfg(unix)]
#[test]
fn a_terminal_shows_the_whole_identity_even_without_connections() {
    let mut current = detail();
    current["connections"] = json!([]);
    let mut disabled = current.clone();
    disabled["enabled"] = json!(false);
    let mut saved_body = saved(json!({"enabled": false}));
    saved_body["connections"] = json!([]);
    for (args, steps) in [
        (
            vec!["identities", "get", ID],
            vec![call(
                "GET",
                format!("/v1/identities/{ID}"),
                None,
                200,
                Some(current.clone()),
            )],
        ),
        (
            vec!["identities", "disable", ID],
            vec![
                call(
                    "GET",
                    format!("/v1/identities/{ID}"),
                    None,
                    200,
                    Some(current.clone()),
                ),
                call(
                    "PUT",
                    format!("/v1/identities/{ID}"),
                    Some(saved_body.clone()),
                    200,
                    Some(disabled.clone()),
                ),
            ],
        ),
    ] {
        let server = Server::start(steps);
        let dir = tempfile::tempdir().unwrap();
        let mut command = command(dir.path(), &server.url);
        command
            .env("ADAPT_TOKEN", "test-token")
            .env("ADAPT_ORG_ID", ORG)
            .args(&args);
        let (code, text) = in_terminal(command);
        assert_eq!(code, 0, "{text}");
        assert!(!text.contains("No results."), "{text}");
        assert!(text.contains("\"name\": \"deploy-bot\""), "{text}");
        assert!(text.contains("\"connections\": []"), "{text}");
        server.finish();
    }
}
