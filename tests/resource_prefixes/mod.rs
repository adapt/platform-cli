use crate::support::*;
use serde_json::json;

#[test]
fn resource_commands_and_linked_ids_send_prefixes_to_the_api() {
    for (args, method, path, body) in [
        (
            vec!["images", "get", "ABCDEF01"],
            "GET",
            "/v1/images/abcdef01",
            None,
        ),
        (
            vec!["volumes", "quota", "abcdef01", "--bytes", "4096"],
            "PUT",
            "/v1/volumes/abcdef01/quota",
            Some(json!({"quotaBytes":4096})),
        ),
        (
            vec!["registry", "delete", "abcdef01"],
            "DELETE",
            "/v1/registry-credentials/abcdef01",
            None,
        ),
        (
            vec![
                "endpoints",
                "bind",
                "abcdef01",
                "12345678",
                "--port",
                "8080",
            ],
            "PUT",
            "/v1/ingresses/abcdef01/binding",
            Some(json!({"sandboxId":"12345678","port":8080})),
        ),
        (
            vec!["execs", "get", "abcdef01"],
            "GET",
            "/v1/execs/abcdef01",
            None,
        ),
        (
            vec!["connections", "describe", "abcdef01", "search"],
            "POST",
            "/v1/gateway/describe",
            Some(json!({"connectionId":"abcdef01","tool":"search"})),
        ),
        (
            vec!["org", "members", "role", "abcdef01", "member"],
            "POST",
            "/v1/auth/organization/update-member-role",
            Some(json!({"organizationId":ORG,"memberId":"abcdef01","role":"member"})),
        ),
        (
            vec!["org", "invitations", "cancel", "abcdef01"],
            "POST",
            "/v1/auth/organization/cancel-invitation",
            Some(json!({"invitationId":"abcdef01"})),
        ),
    ] {
        let server = Server::start(vec![step(method, path.into(), body, json!({"id":ID}))]);
        let dir = tempfile::tempdir().unwrap();
        success(&run(dir.path(), &server.url, &args));
        server.finish();
    }
}

#[test]
fn sandbox_and_image_creation_accept_linked_resource_prefixes() {
    for (args, path, field, value) in [
        (
            vec![
                "sandbox",
                "create",
                "--image-id",
                "ABCDEF01",
                "--variable-set",
                "12345678",
            ],
            "/v1/sandboxes",
            "imageId",
            "abcdef01",
        ),
        (
            vec![
                "images",
                "import",
                "ubuntu:24.04",
                "--name",
                "Ubuntu",
                "--credential",
                "ABCDEF01",
            ],
            "/v1/images",
            "registryCredentialId",
            "abcdef01",
        ),
    ] {
        let server = Server::start(vec![Box::new(move |r| {
            assert_eq!(r.method, "POST");
            assert_eq!(r.target, path);
            assert_eq!(r.json()[field], value);
            if path == "/v1/sandboxes" {
                assert_eq!(r.json()["variableSetIds"], json!(["12345678"]));
            }
            Reply::json(202, json!({"id":ID}))
        })]);
        let dir = tempfile::tempdir().unwrap();
        success(&run(dir.path(), &server.url, &args));
        server.finish();
    }
}

#[test]
fn image_wait_and_execution_logs_pin_canonical_ids() {
    let server = Server::start(vec![
        step(
            "GET",
            "/v1/images/11111111".into(),
            None,
            json!({"id":ID,"status":"preparing"}),
        ),
        step(
            "GET",
            format!("/v1/images/{ID}"),
            None,
            json!({"id":ID,"status":"ready"}),
        ),
        step("GET", "/v1/execs/33333333".into(), None, json!({"id":KEY})),
        Box::new(|r| {
            assert_eq!(r.target, format!("/v1/execs/{KEY}/events?cursor=0"));
            Reply::bytes(
                "text/event-stream",
                b"event: exit\ndata: {\"status\":\"exited\",\"exitCode\":0,\"truncated\":false}\n\n".to_vec(),
            )
        }),
    ]);
    let dir = tempfile::tempdir().unwrap();
    success(&run(
        dir.path(),
        &server.url,
        &["images", "wait", "11111111"],
    ));
    assert!(
        run(dir.path(), &server.url, &["execs", "logs", "33333333"])
            .status
            .success()
    );
    server.finish();
}

fn login_steps(organizations: serde_json::Value) -> Vec<Handler> {
    vec![
        Box::new(|r| {
            assert_eq!(r.target, "/v1/auth/device/code");
            crate::device_response()
        }),
        Box::new(|r| {
            assert_eq!(r.target, "/v1/auth/device/token");
            assert!(!r.headers.contains_key("authorization"));
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
                json!({"user":{"email":"alice@example.com"},"session":{"id":ID}}),
            )
        }),
        Box::new(move |r| {
            assert_eq!(r.target, "/v1/auth/organization/list");
            assert_eq!(r.headers["authorization"], "Bearer new-session-secret");
            Reply::json(200, organizations)
        }),
    ]
}

#[test]
fn login_org_selectors_ignore_slugs_and_persist_the_matching_id() {
    const SELECTED: &str = "abcdef01-1111-4111-8111-111111111111";
    for (reference, from_env) in [("ABCDEF01", false), (SELECTED, false), ("abcdef01", true)] {
        let mut steps = login_steps(json!([
            {"id":ORG,"name":"Slug collision","slug":reference.to_ascii_lowercase()},
            {"id":SELECTED,"name":"Selected by ID","slug":"selected"}
        ]));
        steps.push(Box::new(|r| {
            assert_eq!(r.target, "/v1/whoami");
            assert_eq!(r.headers["x-organization-id"], SELECTED);
            assert_eq!(r.headers["authorization"], "Bearer new-session-secret");
            Reply::json(200, json!({"organizationId":SELECTED}))
        }));
        let server = Server::start(steps);
        let dir = tempfile::tempdir().unwrap();
        let mut login = command(dir.path(), &server.url);
        login.args(["auth", "login", "--no-browser"]);
        if from_env {
            login.env("ADAPT_ORG_ID", reference);
        } else {
            login.args(["--org", reference]);
        }
        assert_eq!(
            success(&login.output().unwrap())["organizationId"],
            SELECTED
        );
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("config.json")).unwrap())
                .unwrap();
        assert_eq!(saved["profiles"]["default"]["organization_id"], SELECTED);
        success(
            &command(dir.path(), &server.url)
                .arg("whoami")
                .output()
                .unwrap(),
        );
        server.finish();
    }
}

#[test]
fn login_org_selectors_do_not_use_slugs_to_bypass_missing_or_ambiguous_ids() {
    for (organizations, message) in [
        (
            json!([
                {"id":"abcdef01-1111-4111-8111-111111111111","slug":"first"},
                {"id":"abcdef01-2222-4222-8222-222222222222","slug":"second"},
                {"id":ORG,"slug":"abcdef01"}
            ]),
            "Ambiguous organization ID",
        ),
        (
            json!([{"id":ORG,"slug":"abcdef01"}]),
            "That organization is not available",
        ),
    ] {
        let server = Server::start(login_steps(organizations));
        let dir = tempfile::tempdir().unwrap();
        let output = command(dir.path(), &server.url)
            .args(["auth", "login", "--no-browser", "--org", "abcdef01"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(message), "{stderr}");
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("config.json")).unwrap())
                .unwrap();
        assert!(saved["profiles"]["default"]["organization_id"].is_null());
        assert_eq!(
            saved["profiles"]["default"]["credential"]["token"],
            "new-session-secret"
        );
        server.finish();
    }
}

#[test]
fn org_use_still_prefers_an_exact_slug_to_an_id_prefix() {
    let server = Server::start(vec![step(
        "GET",
        "/v1/auth/organization/list".into(),
        None,
        json!([
            {"id":"abcdef01-1111-4111-8111-111111111111","slug":"first"},
            {"id":ORG,"slug":"abcdef01"}
        ]),
    )]);
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        success(&run(dir.path(), &server.url, &["org", "use", "abcdef01"]))["organizationId"],
        ORG
    );
    server.finish();
}
