use super::*;

pub(super) fn local_command(state: &Path, args: &[&str]) -> Command {
    let mut cmd = command(state, "not-a-valid-cloud-origin");
    cmd.env("ADAPT_TOKEN", "cloud-secret-must-not-reach-goblin")
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .args(["--local", "--local-state-dir"])
        .arg(state)
        .args(args);
    cmd
}

fn result(state: &Path, args: &[&str]) -> Value {
    let output = local_command(state, args).output().unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn persistent_sandbox_lifecycle_uses_one_backend_across_cli_invocations() {
    let dir = tempfile::tempdir().unwrap();
    discovery(dir.path());
    let id = Arc::new(Mutex::new(String::new()));
    let created = id.clone();
    let server = Server::unix(
        &dir.path().join("goblind.sock"),
        vec![
            spec(),
            http(Box::new(move |r| {
                local_auth(r);
                assert_eq!((r.method.as_str(), r.target.as_str()), ("POST", "/v1/vms"));
                let body = r.json();
                *created.lock().unwrap() = body["id"].as_str().unwrap().into();
                assert_eq!(body["image"], "alpine:latest");
                Reply::json(
                    202,
                    json!({"id":body["id"],"state":"starting","vcpus":1,"memory_mib":1024}),
                )
            })),
            http(Box::new(|r| {
                local_auth(r);
                assert_eq!(r.method, "GET");
                Reply::json(200, json!({"state":"ready"}))
            })),
        ],
    );
    let created = result(
        dir.path(),
        &[
            "sandbox",
            "create",
            "--wait",
            "--image",
            "alpine:latest",
            "--env",
            "BASE=literal value",
            "--env",
            "OVERRIDE=old",
        ],
    );
    let id = id.lock().unwrap().clone();
    assert_eq!(created["id"], id);
    assert_eq!(created["status"], "running");
    server.finish();
    fs::remove_file(dir.path().join("goblind.sock")).unwrap();
    let metadata = dir.path().join(format!("adapt-{id}.json"));
    assert_eq!(
        fs::metadata(&metadata).unwrap().permissions().mode() & 0o077,
        0
    );

    let ids = Arc::new(Mutex::new(Ids {
        vm: id.clone(),
        session: String::new(),
    }));
    let executing = ids.clone();
    let mut steps = vec![
        spec(),
        http(Box::new(move |r| {
            local_auth(r);
            let mut ids = executing.lock().unwrap();
            assert_eq!(r.target, format!("/v1/vms/{}/sessions", ids.vm));
            let body = r.json();
            ids.session = body["id"].as_str().unwrap().into();
            assert_eq!(
                body["env"],
                json!({"BASE":"literal value","OVERRIDE":"new"})
            );
            assert_eq!(body["cwd"], "/tmp");
            assert_eq!(body["timeout_ms"], 5000);
            assert_eq!(body["stdin"], false);
            assert_eq!(body["argv"], json!(["true"]));
            Reply::json(202, json!({"id":ids.session,"state":"starting"}))
        })),
        stream(ids.clone(), |mut ws| {
            send(
                &mut ws,
                json!({"kind":"attached","session":{"stdin_closed":true,"stdin_credit":0}}),
            );
            send(
                &mut ws,
                json!({"kind":"stdout","sequence":1,"data_base64":"cGVyc2lzdGVudAo="}),
            );
            send(&mut ws, json!({"kind":"finished","exit":{"code":7}}));
            while let Ok(message) = ws.read() {
                if matches!(message, Message::Close(_)) {
                    break;
                }
            }
        }),
    ];
    steps.extend(session_cleanup(ids, "exited"));
    let server = Server::unix(&dir.path().join("goblind.sock"), steps);
    let output = local_command(
        dir.path(),
        &[
            "exec",
            &id,
            "--env",
            "OVERRIDE=new",
            "--cwd",
            "/tmp",
            "--timeout-ms",
            "5000",
            "--",
            "true",
        ],
    )
    .output()
    .unwrap();
    assert_eq!(
        output.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"persistent\n");
    server.finish();
    fs::remove_file(dir.path().join("goblind.sock")).unwrap();
    assert!(
        metadata.exists(),
        "exec must not delete a persistent sandbox"
    );

    for (args, method, suffix, state) in [
        (vec!["sandbox", "get", &id], "GET", "", "busy"),
        (vec!["sandbox", "wait", &id], "GET", "", "ready"),
        (vec!["sandbox", "stop", &id], "POST", "/stop", "stopped"),
    ] {
        let path = format!("/v1/vms/{id}{suffix}");
        let server = Server::unix(
            &dir.path().join("goblind.sock"),
            vec![
                spec(),
                http(Box::new(move |r| {
                    local_auth(r);
                    assert_eq!(
                        (r.method.as_str(), r.target.as_str()),
                        (method, path.as_str())
                    );
                    Reply::json(200, json!({"state":state,"vcpus":1,"memory_mib":1024}))
                })),
            ],
        );
        let snapshot = result(dir.path(), &args);
        assert_eq!(snapshot["id"], id);
        assert_eq!(
            snapshot["status"],
            if state == "stopped" {
                "stopped"
            } else {
                "running"
            }
        );
        assert_eq!(snapshot["cpu"], 1);
        assert_eq!(snapshot["memoryMiB"], 1024);
        server.finish();
        fs::remove_file(dir.path().join("goblind.sock")).unwrap();
    }
    let deleted_id = id.clone();
    let server = Server::unix(
        &dir.path().join("goblind.sock"),
        vec![
            spec(),
            http(Box::new(move |r| {
                local_auth(r);
                assert_eq!(r.method, "DELETE");
                assert_eq!(r.target, format!("/v1/vms/{deleted_id}"));
                Reply::json(204, Value::Null)
            })),
        ],
    );
    assert_eq!(
        result(dir.path(), &["sandbox", "delete", &id])["status"],
        "deleted"
    );
    assert!(!metadata.exists());
    server.finish();
    fs::remove_file(dir.path().join("goblind.sock")).unwrap();
}

#[test]
fn local_inventory_is_normalized_and_filters_ids() {
    let dir = tempfile::tempdir().unwrap();
    discovery(dir.path());
    for query in ["", "111111111111", "FFFFFF"] {
        let server = Server::unix(
            &dir.path().join("goblind.sock"),
            vec![
                spec(),
                http(Box::new(|r| {
                    local_auth(r);
                    assert_eq!(r.target, "/v1/vms");
                    Reply::json(
                        200,
                        json!([
                            {"id":KEY,"state":"stopped"}, {"id":ID,"state":"ready"}, {"id":"external-name","state":"ready"}
                        ]),
                    )
                })),
            ],
        );
        let mut args = vec!["sandbox", "list", "--all"];
        if !query.is_empty() {
            args.extend(["--query", query]);
        }
        let value = result(dir.path(), &args);
        let rows = value["sandboxes"].as_array().unwrap();
        assert_eq!(
            rows.len(),
            match query {
                "" => 2,
                "111111111111" => 1,
                _ => 0,
            }
        );
        if !rows.is_empty() {
            assert_eq!(rows[0]["id"], ID);
            assert_eq!(rows[0]["status"], "running");
        }
        assert!(value["nextCursor"].is_null());
        server.finish();
        fs::remove_file(dir.path().join("goblind.sock")).unwrap();
    }
}

#[test]
fn local_wait_handles_terminal_failure_deleted_resources_and_deadlines() {
    let dir = tempfile::tempdir().unwrap();
    discovery(dir.path());
    for (state, target, succeeds) in [
        ("failed", "running", false),
        ("stopped", "stopped", true),
        ("missing", "deleted", true),
    ] {
        let server = Server::unix(
            &dir.path().join("goblind.sock"),
            vec![
                spec(),
                http(Box::new(move |_| {
                    Reply::json(
                        if state == "missing" { 404 } else { 200 },
                        json!({"state":state,"error":"fixture failure"}),
                    )
                })),
            ],
        );
        let output = local_command(dir.path(), &["sandbox", "wait", ID, "--status", target])
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            succeeds,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if !succeeds {
            assert!(String::from_utf8_lossy(&output.stderr).contains("fixture failure"));
        }
        server.finish();
        fs::remove_file(dir.path().join("goblind.sock")).unwrap();
    }
    let server = Server::unix(
        &dir.path().join("goblind.sock"),
        vec![
            spec(),
            Box::new(|mut socket| {
                let request = read_request(&mut socket);
                std::thread::sleep(std::time::Duration::from_millis(1200));
                request
            }),
        ],
    );
    let start = Instant::now();
    let output = local_command(dir.path(), &["sandbox", "wait", ID, "--wait-timeout", "1"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Timed out waiting"));
    assert!(start.elapsed() < std::time::Duration::from_secs(3));
    server.finish();
    fs::remove_file(dir.path().join("goblind.sock")).unwrap();
}

#[test]
fn stop_wait_observes_the_final_state_and_delete_tolerates_missing_vms() {
    let dir = tempfile::tempdir().unwrap();
    discovery(dir.path());
    let server = Server::unix(
        &dir.path().join("goblind.sock"),
        vec![
            spec(),
            http(Box::new(|r| {
                assert_eq!(
                    (r.method.as_str(), r.target.as_str()),
                    ("POST", format!("/v1/vms/{ID}/stop").as_str())
                );
                Reply::json(200, json!({"id":ID,"state":"stopped"}))
            })),
            http(Box::new(|r| {
                assert_eq!(
                    (r.method.as_str(), r.target.as_str()),
                    ("GET", format!("/v1/vms/{ID}").as_str())
                );
                Reply::json(200, json!({"state":"stopped"}))
            })),
            spec(),
            http(Box::new(|r| {
                assert_eq!(r.method, "DELETE");
                Reply::json(404, json!({"error":"missing"}))
            })),
        ],
    );
    assert_eq!(
        result(dir.path(), &["sandbox", "stop", ID, "--wait"])["status"],
        "stopped"
    );
    assert_eq!(
        result(dir.path(), &["sandbox", "delete", ID])["status"],
        "deleted"
    );
    server.finish();
}

#[test]
fn execution_refuses_public_or_symlinked_environment_metadata_before_creating_a_session() {
    let dir = tempfile::tempdir().unwrap();
    discovery(dir.path());
    let path = dir.path().join(format!("adapt-{ID}.json"));
    fs::write(&path, "{\"SECRET\":\"do-not-print\"}").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    for symlinked in [false, true] {
        if symlinked {
            fs::remove_file(&path).unwrap();
            std::os::unix::fs::symlink(dir.path().join("token"), &path).unwrap();
        }
        let server = Server::unix(&dir.path().join("goblind.sock"), vec![spec()]);
        let output = local_command(dir.path(), &["exec", ID, "--", "true"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let error = String::from_utf8_lossy(&output.stderr);
        if !symlinked {
            assert!(error.contains("private, user-owned regular file"));
        }
        assert!(!error.contains("do-not-print"));
        assert!(!error.contains(TOKEN));
        assert!(output.stdout.is_empty());
        server.finish();
        fs::remove_file(dir.path().join("goblind.sock")).unwrap();
    }
}
