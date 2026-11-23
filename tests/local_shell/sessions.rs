use super::{backend::local_command, *};
use std::sync::atomic::{AtomicUsize, Ordering};

fn create(ids: Arc<Mutex<Ids>>, retained: Arc<AtomicUsize>) -> Exchange {
    http(Box::new(move |r| {
        local_auth(r);
        assert_eq!(
            (r.method.as_str(), r.target.as_str()),
            ("POST", format!("/v1/vms/{ID}/sessions").as_str())
        );
        assert!(
            retained.fetch_add(1, Ordering::SeqCst) < 64,
            "Goblin's retained session capacity was exhausted"
        );
        let mut ids = ids.lock().unwrap();
        ids.session = r.json()["id"].as_str().unwrap().into();
        Reply::json(202, json!({"id":ids.session,"state":"starting"}))
    }))
}

fn finished(ids: Arc<Mutex<Ids>>, code: u8) -> Exchange {
    stream(ids, move |mut ws| {
        send(&mut ws, json!({"kind":"finished","exit":{"code":code}}));
        assert!(matches!(ws.read().unwrap(), Message::Close(_)));
    })
}

fn session_reply(ids: Arc<Mutex<Ids>>, method: &'static str, status: u16, body: Value) -> Exchange {
    http(Box::new(move |r| {
        local_auth(r);
        let ids = ids.lock().unwrap();
        assert_eq!(r.method, method);
        assert_eq!(
            r.target,
            format!("/v1/vms/{}/sessions/{}", ids.vm, ids.session)
        );
        Reply::json(status, body)
    }))
}

fn ids() -> Arc<Mutex<Ids>> {
    Arc::new(Mutex::new(Ids {
        vm: ID.into(),
        session: String::new(),
    }))
}

#[test]
fn more_than_64_sequential_execs_release_sessions_and_preserve_exit_codes() {
    let dir = tempfile::tempdir().unwrap();
    discovery(dir.path());
    let retained = Arc::new(AtomicUsize::new(0));
    let mut steps = Vec::new();
    for i in 0..70 {
        let ids = ids();
        steps.extend([
            spec(),
            create(ids.clone(), retained.clone()),
            finished(ids.clone(), i % 2 * 7),
        ]);
        steps.push(session_reply(
            ids.clone(),
            "GET",
            200,
            json!({"state":"exited"}),
        ));
        let count = retained.clone();
        steps.push(http(Box::new(move |r| {
            local_auth(r);
            assert_eq!(r.method, "DELETE");
            assert_eq!(
                r.target,
                format!("/v1/vms/{ID}/sessions/{}", ids.lock().unwrap().session)
            );
            assert_eq!(count.fetch_sub(1, Ordering::SeqCst), 1);
            Reply::json(204, Value::Null)
        })));
    }
    let server = Server::unix(&dir.path().join("goblind.sock"), steps);
    for i in 0..70 {
        let output = local_command(dir.path(), &["exec", ID, "--", "true"])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(i % 2 * 7),
            "execution {i}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(retained.load(Ordering::SeqCst), 0);
    }
    server.finish();
}

#[test]
fn finished_session_cleanup_retries_detach_races_and_tolerates_already_deleted_sessions() {
    let dir = tempfile::tempdir().unwrap();
    discovery(dir.path());
    for state in ["exited", "failed"] {
        let ids = ids();
        let server = Server::unix(
            &dir.path().join("goblind.sock"),
            vec![
                spec(),
                create(ids.clone(), Arc::new(AtomicUsize::new(0))),
                finished(ids.clone(), 7),
                session_reply(ids.clone(), "GET", 200, json!({"state":state})),
                session_reply(
                    ids.clone(),
                    "DELETE",
                    409,
                    json!({"error":"stop and detach session before deleting"}),
                ),
                session_reply(
                    ids.clone(),
                    "DELETE",
                    429,
                    json!({"error":"worker capacity reached"}),
                ),
                session_reply(ids, "DELETE", 404, json!({"error":"session not found"})),
            ],
        );
        let output = local_command(dir.path(), &["exec", ID, "--", "false"])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(7),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("Could not release"));
        server.finish();
        fs::remove_file(dir.path().join("goblind.sock")).unwrap();
    }
}

#[test]
fn failed_attachment_does_not_cancel_live_commands_or_mask_the_original_error() {
    let dir = tempfile::tempdir().unwrap();
    discovery(dir.path());
    for state in ["starting", "running", "stopping", "failed"] {
        let ids = ids();
        let mut steps = vec![
            spec(),
            create(ids.clone(), Arc::new(AtomicUsize::new(0))),
            stream(ids.clone(), |mut ws| {
                send(
                    &mut ws,
                    json!({"kind":"error","message":"original stream failure"}),
                );
            }),
        ];
        steps.extend(session_cleanup(ids, state));
        let server = Server::unix(&dir.path().join("goblind.sock"), steps);
        let output = local_command(dir.path(), &["exec", ID, "--", "sleep", "100"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("original stream failure"));
        server.finish();
        fs::remove_file(dir.path().join("goblind.sock")).unwrap();
    }
}

#[test]
fn session_cleanup_errors_preserve_failed_exit_codes_and_report_successful_command_leaks() {
    let dir = tempfile::tempdir().unwrap();
    discovery(dir.path());
    for code in [0, 7] {
        let ids = ids();
        let server = Server::unix(
            &dir.path().join("goblind.sock"),
            vec![
                spec(),
                create(ids.clone(), Arc::new(AtomicUsize::new(0))),
                finished(ids.clone(), code),
                session_reply(ids.clone(), "GET", 200, json!({"state":"exited"})),
                session_reply(
                    ids,
                    "DELETE",
                    403,
                    json!({"error":"fixture denied cleanup"}),
                ),
            ],
        );
        let output = local_command(dir.path(), &["exec", ID, "--", "true"])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(if code == 0 { 1 } else { i32::from(code) })
        );
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("Could not release local execution session"));
        assert!(error.contains("fixture denied cleanup"));
        server.finish();
        fs::remove_file(dir.path().join("goblind.sock")).unwrap();
    }
}

#[test]
fn session_cleanup_deadline_includes_inspection_and_delete_retries() {
    let dir = tempfile::tempdir().unwrap();
    discovery(dir.path());
    let ids = ids();
    let server = Server::unix(
        &dir.path().join("goblind.sock"),
        vec![
            spec(),
            create(ids.clone(), Arc::new(AtomicUsize::new(0))),
            finished(ids.clone(), 0),
            session_reply(ids.clone(), "GET", 200, json!({"state":"exited"})),
            session_reply(ids, "DELETE", 409, json!({"error":"still detaching"})),
            Box::new(|mut socket| {
                let request = read_request(&mut socket);
                assert_eq!(request.method, "DELETE");
                std::thread::sleep(std::time::Duration::from_millis(1200));
                request
            }),
        ],
    );
    let start = Instant::now();
    let output = command_with_timeout(dir.path(), "invalid-cloud-origin", 1)
        .args(["--local", "--local-state-dir"])
        .arg(dir.path())
        .args(["exec", ID, "--", "true"])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("Timed out releasing"));
    assert!(start.elapsed() < std::time::Duration::from_secs(4));
    server.finish();
}

#[test]
fn invalid_persisted_or_merged_environments_never_create_an_execution() {
    let dir = tempfile::tempdir().unwrap();
    discovery(dir.path());
    let mut full = serde_json::Map::new();
    for i in 0..128 {
        full.insert(format!("KEY_{i}"), json!("default"));
    }
    for (defaults, extra) in [
        (json!({"BAD-NAME":"do-not-print"}), vec![]),
        (json!({"KEY":"do-not-print\u{0}"}), vec![]),
        (Value::Object(full), vec!["--env", "EXTRA=value"]),
        (
            json!({"KEY":"x".repeat(32765)}),
            vec!["--env", "EXTRA=value"],
        ),
    ] {
        private_file(
            &dir.path().join(format!("adapt-{ID}.json")),
            defaults.to_string().as_bytes(),
        );
        let server = Server::unix(&dir.path().join("goblind.sock"), vec![spec()]);
        let mut args = vec!["exec", ID];
        args.extend(extra);
        args.extend(["--", "true"]);
        let output = local_command(dir.path(), &args).output().unwrap();
        assert_eq!(output.status.code(), Some(1));
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(error.contains("environment"), "{error}");
        assert!(!error.contains("do-not-print"));
        server.finish();
        fs::remove_file(dir.path().join("goblind.sock")).unwrap();
    }
}
