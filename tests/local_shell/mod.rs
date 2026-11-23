mod backend;
use crate::support::*;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
    time::Instant,
};
use tokio_tungstenite::tungstenite::{self, Message};

const TOKEN: &str = "local-runtime-token-that-is-not-a-cloud-token";
const INSTANCE: &str = "11111111-1111-4111-8111-111111111111";

fn private_file(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn command_for(state: &Path) -> Command {
    let mut cmd = command(state, "not-a-valid-cloud-origin");
    cmd.env("ADAPT_TOKEN", "cloud-secret-must-not-reach-goblin")
        .env("ADAPT_ORG_ID", ORG)
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .args(["sandbox", "sh", "--local", "--local-state-dir"])
        .arg(state);
    cmd
}

fn discovery(state: &Path) {
    fs::set_permissions(state, fs::Permissions::from_mode(0o700)).unwrap();
    private_file(&state.join("token"), TOKEN.as_bytes());
    private_file(&state.join("ready.json"), json!({"version":1,"socket_path":state.canonicalize().unwrap().join("goblind.sock"),"instance_id":INSTANCE,"runtime_version":"test"}).to_string().as_bytes());
    // Local operation must not even deserialize the saved cloud profile.
    private_file(&state.join("config.json"), b"not valid JSON");
}

fn local_auth(r: &Request) {
    assert_eq!(r.headers["authorization"], format!("Bearer {TOKEN}"));
    assert!(!r.headers.contains_key("x-organization-id"));
}

fn spec() -> Exchange {
    http(Box::new(|r| {
        local_auth(r);
        assert_eq!(r.target, "/v1/spec");
        Reply::json(
            200,
            json!({"version":1,"instance_id":INSTANCE,"runtime_version":"test","features":{"sessions":true,"image_pull":true}}),
        )
    }))
}

#[derive(Default)]
struct Ids {
    vm: String,
    session: String,
}

fn setup(ids: &Arc<Mutex<Ids>>) -> Vec<Exchange> {
    setup_with_tty(ids, false)
}

fn setup_with_tty(ids: &Arc<Mutex<Ids>>, tty: bool) -> Vec<Exchange> {
    let create_ids = ids.clone();
    let ready_ids = ids.clone();
    let exec_ids = ids.clone();
    vec![
        spec(),
        http(Box::new(move |r| {
            local_auth(r);
            assert_eq!((r.method.as_str(), r.target.as_str()), ("POST", "/v1/vms"));
            let mut body = r.json();
            let id = body.as_object_mut().unwrap().remove("id").unwrap();
            uuid::Uuid::parse_str(id.as_str().unwrap()).unwrap();
            create_ids.lock().unwrap().vm = id.as_str().unwrap().into();
            assert_eq!(
                body,
                json!({"image":"ubuntu:24.04","resources":{"vcpus":2,"memory_mib":512},"state_size_mib":128,"network":{"mode":"user","egress":"public","dns":"1.1.1.1"}})
            );
            Reply::json(202, json!({"id":id,"state":"starting"}))
        })),
        http(Box::new(move |r| {
            local_auth(r);
            assert_eq!(
                r.target,
                format!("/v1/vms/{}", ready_ids.lock().unwrap().vm)
            );
            Reply::json(200, json!({"state":"ready"}))
        })),
        http(Box::new(move |r| {
            local_auth(r);
            let mut ids = exec_ids.lock().unwrap();
            assert_eq!(r.target, format!("/v1/vms/{}/sessions", ids.vm));
            let body = r.json();
            ids.session = body["id"].as_str().unwrap().into();
            uuid::Uuid::parse_str(&ids.session).unwrap();
            if tty {
                assert_eq!(body["argv"][0], "/bin/sh");
                assert_eq!(body["argv"][1], "-c");
            } else {
                assert_eq!(body["argv"], json!(["cat"]));
            }
            assert_eq!(body["env"], json!({"LITERAL":" a=b\n$HOME "}));
            assert_eq!(body["stdin"], true);
            assert_eq!(body["timeout_ms"], 86_400_000);
            assert_eq!(body.get("terminal").is_some(), tty);
            Reply::json(202, json!({"id":ids.session,"state":"starting"}))
        })),
    ]
}

#[allow(clippy::result_large_err)]
fn stream(
    ids: Arc<Mutex<Ids>>,
    body: impl FnOnce(tungstenite::WebSocket<TestSocket>) + Send + 'static,
) -> Exchange {
    Box::new(move |socket| {
        let mut received = None;
        let ws = tungstenite::accept_hdr(
            socket,
            |request: &tungstenite::handshake::server::Request, response| {
                let ids = ids.lock().unwrap();
                assert_eq!(
                    request.uri().path(),
                    format!("/v1/vms/{}/sessions/{}/stream", ids.vm, ids.session)
                );
                assert_eq!(request.uri().query(), Some("after=0&read_only=false"));
                assert_eq!(
                    request.headers()["authorization"],
                    format!("Bearer {TOKEN}")
                );
                assert!(!request.headers().contains_key("x-organization-id"));
                received = Some(Request {
                    method: "GET".into(),
                    target: request.uri().to_string(),
                    headers: HashMap::new(),
                    body: vec![],
                    at: Instant::now(),
                });
                Ok(response)
            },
        )
        .unwrap();
        body(ws);
        received.unwrap()
    })
}

fn deleted(ids: Arc<Mutex<Ids>>) -> Exchange {
    http(Box::new(move |r| {
        local_auth(r);
        assert_eq!(r.method, "DELETE");
        assert_eq!(r.target, format!("/v1/vms/{}", ids.lock().unwrap().vm));
        Reply::json(204, Value::Null)
    }))
}

fn send(ws: &mut tungstenite::WebSocket<TestSocket>, value: Value) {
    ws.send(Message::Text(value.to_string().into())).unwrap();
}

fn args(cmd: &mut Command) -> &mut Command {
    cmd.args([
        "--cpu",
        "2",
        "--memory",
        "512",
        "--disk",
        "128",
        "--env",
        "LITERAL= a=b\n$HOME ",
        "--",
        "cat",
    ])
}

#[test]
fn local_shell_uses_its_own_credentials_honors_credit_and_cleans_up_on_nonzero_exit() {
    let ids = Arc::new(Mutex::new(Ids::default()));
    let mut steps = setup(&ids);
    steps.push(stream(ids.clone(), |mut ws| {
        // Begin with less than a frame of credit; later replenish each chunk.
        send(
            &mut ws,
            json!({"kind":"attached","session":{"stdin_credit":1024,"stdin_closed":false}}),
        );
        let mut total = 0;
        loop {
            match ws.read().unwrap() {
                Message::Binary(bytes) => {
                    assert!(bytes.len() <= 1024);
                    assert!(bytes.iter().all(|b| *b == b'x'));
                    total += bytes.len();
                    send(
                        &mut ws,
                        json!({"kind":"stdin_consumed","bytes":bytes.len()}),
                    );
                }
                Message::Text(text) if text.contains("stdin_eof") => break,
                message => panic!("Unexpected message: {message:?}"),
            }
        }
        assert_eq!(total, 128 * 1024);
        send(
            &mut ws,
            json!({"kind":"stdout","sequence":1,"data_base64":"aGVsbG8K"}),
        );
        send(&mut ws, json!({"kind":"finished","exit":{"code":7}}));
        while let Ok(message) = ws.read() {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
    }));
    steps.push(deleted(ids));
    let dir = tempfile::tempdir().unwrap();
    discovery(dir.path());
    let server = Server::unix(&dir.path().join("goblind.sock"), steps);
    let mut cmd = command_for(dir.path());
    args(&mut cmd);
    let output = with_stdin(cmd, &vec![b'x'; 128 * 1024]);
    assert_eq!(
        output.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"hello\n");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("cloud-secret"));
    server.finish();
}

#[test]
fn local_stream_failure_still_deletes_the_vm() {
    let ids = Arc::new(Mutex::new(Ids::default()));
    let mut steps = setup(&ids);
    steps.push(stream(ids.clone(), |mut ws| {
        send(
            &mut ws,
            json!({"kind":"error","message":"guest execution failed"}),
        );
    }));
    steps.push(deleted(ids));
    let dir = tempfile::tempdir().unwrap();
    discovery(dir.path());
    let server = Server::unix(&dir.path().join("goblind.sock"), steps);
    let output = args(&mut command_for(dir.path())).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("guest execution failed"));
    server.finish();
}

#[test]
fn local_preflight_rejects_unsupported_options_without_starting_a_runtime() {
    let dir = tempfile::tempdir().unwrap();
    for flags in [
        vec!["--ttl", "60"],
        vec!["--label", "a=b"],
        vec!["--variable-set", ID],
        vec!["--image-id", ID],
        vec!["--data", "{}"],
        vec!["--run-image"],
        vec!["--idempotency-key", KEY],
    ] {
        let output = command_for(dir.path())
            .args(&flags)
            .args(["--", "true"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("not supported by the local backend")
        );
        assert!(!dir.path().join("bootstrap.lock").exists());
    }
    assert_eq!(
        command_for(dir.path())
            .args(["--cloud", "--", "true"])
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
    let output = command_for(dir.path()).output().unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("interactive terminal"));
    assert!(!dir.path().join("bootstrap.lock").exists());
}

#[test]
fn saved_local_runtime_settings_do_not_change_an_explicit_cloud_target() {
    let server = Server::start(vec![Box::new(|r| {
        assert_eq!(r.target, "/v1/sandboxes");
        assert_eq!(r.headers["authorization"], "Bearer test-token");
        Reply::json(
            400,
            json!({"error":{"message":"cloud request reached Platform"}}),
        )
    })]);
    let dir = tempfile::tempdir().unwrap();
    let output = command(dir.path(), &server.url)
        .env("ADAPT_TOKEN", "test-token")
        .env("ADAPT_LOCAL_RUNTIME", "/unused/local/bundle")
        .env("ADAPT_LOCAL_STATE_DIR", "/unused/local/state")
        .args(["sandbox", "sh", "--cloud", "--", "true"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("cloud request reached Platform"));
    server.finish();
}

#[test]
fn rejects_untrusted_discovery_before_sending_a_token() {
    let dir = tempfile::tempdir().unwrap();
    discovery(dir.path());
    // A stale TCP address may have been rebound by another local user. Never
    // connect to it, even when the discovery record and token are private.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    private_file(&dir.path().join("ready.json"), json!({"version":1,"address":listener.local_addr().unwrap(),"instance_id":INSTANCE,"runtime_version":"test"}).to_string().as_bytes());
    let output = command_for(dir.path())
        .args(["--", "true"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Invalid local runtime discovery file")
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    private_file(
        &dir.path().join("ready.json"),
        json!({"version":1,"socket_path":dir.path().canonicalize().unwrap().join("goblind.sock"),"instance_id":"not-a-uuid","runtime_version":"test"})
            .to_string()
            .as_bytes(),
    );
    let output = command_for(dir.path())
        .args(["--", "true"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Invalid local runtime discovery file")
    );
    fs::set_permissions(dir.path().join("token"), fs::Permissions::from_mode(0o644)).unwrap();
    let output = command_for(dir.path())
        .args(["--", "true"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("private, user-owned"));
}

struct ManagerGuard(std::path::PathBuf);
impl Drop for ManagerGuard {
    fn drop(&mut self) {
        if let Ok(bytes) = fs::read(self.0.join("ready.json"))
            && let Ok(ready) = serde_json::from_slice::<Value>(&bytes)
            && let Some(pid) = ready["pid"].as_i64()
        {
            // SAFETY: this fixture exclusively owns its private manager state.
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
    }
}

#[test]
fn concurrent_clients_bootstrap_one_detached_manager_and_reuse_it() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let _manager = ManagerGuard(state.clone());
    fs::write(dir.path().join("goblind"), include_str!("manager.py")).unwrap();
    fs::set_permissions(
        dir.path().join("goblind"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    fs::write(dir.path().join("goblin-worker"), b"fixture").unwrap();
    let launch = || {
        command_for(&state)
            .arg("--runtime-dir")
            .arg(dir.path())
            .args(["--", "true"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let one = launch();
    let two = launch();
    let outputs = [
        one.wait_with_output().unwrap(),
        two.wait_with_output().unwrap(),
    ];
    let diagnostics = format!(
        "first client: {}\nsecond client: {}\nmanager log: {}",
        String::from_utf8_lossy(&outputs[0].stderr),
        String::from_utf8_lossy(&outputs[1].stderr),
        fs::read_to_string(state.join("goblind.log")).unwrap_or_default()
    );
    for output in outputs {
        assert_eq!(output.status.code(), Some(1));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("fixture rejected image"),
            "{diagnostics}"
        );
    }
    assert_eq!(
        fs::read_to_string(state.join("starts")).unwrap(),
        "started\n"
    );
    // Discovery must work after the launching processes exit, without a bundle path.
    let output = command_for(&state).args(["--", "true"]).output().unwrap();
    assert!(String::from_utf8_lossy(&output.stderr).contains("fixture rejected image"));
    assert_eq!(
        fs::read_to_string(state.join("starts")).unwrap(),
        "started\n"
    );
    assert_eq!(
        fs::metadata(state.join("token"))
            .unwrap()
            .permissions()
            .mode()
            & 0o077,
        0
    );
}

#[test]
fn terminal_detaches_with_zero_credit_even_after_the_input_queue_fills() {
    use crate::sandbox_sh::unix::{Terminal, wait};
    use std::{io::Write, os::fd::AsRawFd, time::Duration};

    for initial_credit in [0, 1] {
        let terminal = Terminal::new();
        let mut master = terminal.master.try_clone().unwrap();
        let ids = Arc::new(Mutex::new(Ids::default()));
        let mut steps = setup_with_tty(&ids, true);
        steps.push(stream(ids.clone(), move |mut ws| {
            send(&mut ws, json!({"kind":"attached","session":{"stdin_credit":initial_credit,"stdin_closed":false}}));
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                // SAFETY: the live PTY descriptor and output pointer are valid.
                let raw = unsafe {
                    let mut state: libc::termios = std::mem::zeroed();
                    assert_eq!(libc::tcgetattr(master.as_raw_fd(), &mut state), 0);
                    state.c_lflag & libc::ICANON == 0
                };
                if raw { break; }
                assert!(Instant::now() < deadline, "terminal did not enter raw mode");
                std::thread::sleep(Duration::from_millis(5));
            }
            if initial_credit > 0 {
                master.write_all(b"x").unwrap();
                assert_eq!(ws.read().unwrap(), Message::Binary(vec![b'x'].into()));
            }
            // Fill beyond the bounded queue while no credit can arrive. Input
            // must still be consumed so an escape behind it remains reachable.
            let mut bytes = vec![b'y'; 64 * 1024];
            bytes.push(0x1d);
            let mut written = 0;
            while written < bytes.len() {
                match master.write(&bytes[written..]) {
                    Ok(count) => written += count,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "input stopped before escape");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    other => panic!("PTY write failed: {other:?}"),
                }
            }
            assert!(matches!(ws.read().unwrap(), Message::Close(_)));
        }));
        steps.push(terminal.restored_then(deleted(ids)));
        let dir = tempfile::tempdir().unwrap();
        discovery(dir.path());
        let server = Server::unix(&dir.path().join("goblind.sock"), steps);
        let mut cmd = command_for(dir.path());
        cmd.args([
            "--cpu",
            "2",
            "--memory",
            "512",
            "--disk",
            "128",
            "--env",
            "LITERAL= a=b\n$HOME ",
        ]);
        let (code, stderr) = wait(terminal.spawn(cmd), Some(&terminal));
        assert_eq!(code, 0, "{stderr}");
        assert!(stderr.contains("Input buffer full"), "{stderr}");
        server.finish();
    }
}

#[test]
fn discovery_rejects_external_and_symlinked_unix_sockets_before_authentication() {
    use std::os::unix::{fs::symlink, net::UnixListener};
    let dir = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let external_path = external.path().join("goblind.sock");
    let listener = UnixListener::bind(&external_path).unwrap();
    listener.set_nonblocking(true).unwrap();
    for socket_path in [
        external_path.clone(),
        dir.path().canonicalize().unwrap().join("goblind.sock"),
    ] {
        discovery(dir.path());
        private_file(&dir.path().join("ready.json"), json!({"version":1,"socket_path":socket_path,"instance_id":INSTANCE,"runtime_version":"test"}).to_string().as_bytes());
        if socket_path.starts_with(dir.path().canonicalize().unwrap()) {
            symlink(&external_path, &socket_path).unwrap();
        }
        let output = command_for(dir.path())
            .args(["--", "true"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}
