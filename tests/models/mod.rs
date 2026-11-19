use crate::support::*;
use serde_json::json;

#[test]
fn model_commands_request_their_paths_and_pass_json_through() {
    let catalog = json!({"object":"list","data":[
        {"id":"openai/gpt-5","object":"model","created":1762800000,"owned_by":"openai"}
    ]});
    let providers = json!({"providers":[{
        "id":ID,"provider":"openai","name":"OpenAI","disabledModels":["gpt-4o"],"enabled":true,
        "version":2,"createdAt":"2026-11-11T00:00:00Z","updatedAt":"2026-11-12T00:00:00Z"
    }],"adaptProviders":["anthropic"]});
    let discovered = json!({"version":2,"models":[
        {"id":"gpt-5","name":"GPT-5"},{"id":"gpt-4o","name":"GPT-4o"}
    ]});
    let server = Server::start(vec![
        step("GET", "/v1/models".into(), None, catalog.clone()),
        step(
            "GET",
            "/v1/model-gateway/providers".into(),
            None,
            providers.clone(),
        ),
        step(
            "GET",
            "/v1/model-gateway/providers/11111111/models".into(),
            None,
            discovered.clone(),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        success(&run(dir.path(), &server.url, &["models", "list"])),
        catalog
    );
    assert_eq!(
        success(&run(dir.path(), &server.url, &["models", "providers"])),
        providers
    );
    assert_eq!(
        success(&run(
            dir.path(),
            &server.url,
            &["models", "discover", "11111111"]
        )),
        discovered
    );
    server.finish();
}

#[test]
fn discover_reports_the_api_error() {
    let server = Server::start(vec![Box::new(|_| {
        Reply::json(
            403,
            json!({"error":{"code":"FORBIDDEN","message":"Sign in as an owner or admin to manage provider keys."}}),
        )
    })]);
    let dir = tempfile::tempdir().unwrap();
    let output = run(dir.path(), &server.url, &["models", "discover", ID]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("403 FORBIDDEN: Sign in as an owner or admin to manage provider keys.")
    );
    server.finish();
}
