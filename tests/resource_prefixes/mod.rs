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
