use crate::support::*;
use serde_json::json;
use std::process::Command;
use tokio_tungstenite::tungstenite::Message;

fn created() -> Exchange {
    http(step(
        "POST",
        "/v1/sandboxes".into(),
        None,
        json!({"id":ID,"status":"provisioning"}),
    ))
}

fn running() -> Exchange {
    http(step(
        "GET",
        format!("/v1/sandboxes/{ID}"),
        None,
        json!({"id":ID,"status":"running"}),
    ))
}

fn execution(interactive: bool) -> Exchange {
    http(Box::new(move |r| {
        assert_eq!(r.target, format!("/v1/sandboxes/{ID}/exec"));
        let body = r.json();
        assert_eq!(body["stdin"], true);
        assert_eq!(body["timeoutMs"], 86_400_000);
        assert_ne!(r.headers["idempotency-key"], KEY);
        if interactive {
            assert_eq!(body["terminal"]["cols"], 100);
            assert_eq!(body["terminal"]["rows"], 30);
            assert_eq!(body["argv"][0], "/bin/sh");
            assert_eq!(body["argv"][1], "-c");
            assert!(
                body["argv"][2]
                    .as_str()
                    .unwrap()
                    .contains("exec \"$shell\" -il")
            );
        } else {
            assert!(body.get("terminal").is_none());
            assert_eq!(body["argv"], json!(["printf", "%s", " a=b\n$HOME "]));
        }
        Reply::json(202, json!({"id":KEY}))
    }))
}

fn deleted() -> Exchange {
    http(Box::new(|r| {
        assert_eq!(r.method, "DELETE");
        assert_eq!(r.target, format!("/v1/sandboxes/{ID}"));
        Reply {
            status: 204,
            content_type: "application/json",
            headers: vec![],
            body: vec![],
        }
    }))
}

fn finish(mut ws: tokio_tungstenite::tungstenite::WebSocket<std::net::TcpStream>, code: u8) {
    ws.send(Message::Text(
        json!({"kind":"stdout","sequence":1,"data_base64":"aGVsbG8K"})
            .to_string()
            .into(),
    ))
    .unwrap();
    ws.send(Message::Text(
        json!({"kind":"finished","exit":{"code":code}})
            .to_string()
            .into(),
    ))
    .unwrap();
    while let Ok(message) = ws.read() {
        if matches!(message, Message::Close(_)) {
            break;
        }
    }
}

fn shell_command(dir: &std::path::Path, url: &str) -> Command {
    let mut cmd = command(dir, url);
    cmd.env("ADAPT_TOKEN", "test-token")
        .env("ADAPT_ORG_ID", ORG)
        .args(["sandbox", "sh"]);
    cmd
}

#[test]
fn one_off_forwards_creation_options_literal_argv_and_stdin_then_waits_for_cleanup() {
    let server = Server::script(vec![
        http(Box::new(|r| {
            assert_eq!(r.headers["idempotency-key"], KEY);
            assert_eq!(
                r.json(),
                json!({"image":"alpine","cpu":2,"memoryMiB":512,"diskMiB":1024,"ttlSeconds":60,"env":{"A":"b"},"labels":{"task":"test"},"variableSetIds":[ORG],"startup":{"mode":"image"},"network":{"egress":"open","allow":[]}})
            );
            Reply::json(202, json!({"id":ID}))
        })),
        running(),
        execution(false),
        websocket(|mut ws| {
            let mut input = vec![];
            loop {
                match ws.read().unwrap() {
                    Message::Binary(bytes) => input.extend_from_slice(&bytes),
                    Message::Text(text) if text.contains("stdin_eof") => break,
                    message => panic!("Unexpected stdin message {message:?}"),
                }
            }
            assert_eq!(input, b"piped input\n");
            finish(ws, 7);
        }),
        http(step(
            "DELETE",
            format!("/v1/sandboxes/{ID}"),
            None,
            json!({"status":202}),
        )),
        http(step(
            "GET",
            format!("/v1/sandboxes/{ID}"),
            None,
            json!({"id":ID,"status":"deleted"}),
        )),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = shell_command(dir.path(), &server.url);
    cmd.args([
        "--data",
        "{\"imageId\":\"ignored\",\"network\":{\"egress\":\"open\",\"allow\":[]}}",
        "--image",
        "alpine",
        "--cpu",
        "2",
        "--memory",
        "512",
        "--disk",
        "1024",
        "--ttl",
        "60",
        "--env",
        "A=b",
        "--label",
        "task=test",
        "--variable-set",
        ORG,
        "--run-image",
        "--wait",
        "--idempotency-key",
        KEY,
        "--",
        "printf",
        "%s",
        " a=b\n$HOME ",
    ]);
    let out = with_stdin(cmd, b"piped input\n");
    assert_eq!(
        out.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"hello\n");
    server.finish();
}

#[test]
fn fast_command_replays_retained_output_when_it_finishes_before_attachment() {
    let server = Server::script(vec![
        created(),
        running(),
        execution(false),
        http(Box::new(|r| {
            assert!(r.target.starts_with(&format!("/v1/execs/{KEY}/stream?")));
            Reply::json(
                502,
                json!({"error":{"code":"BACKEND_ERROR","message":"Execution attachment failed"}}),
            )
        })),
        http(step(
            "GET",
            format!("/v1/execs/{KEY}"),
            None,
            json!({"id":KEY,"status":"exited","exitCode":7}),
        )),
        http(Box::new(|r| {
            assert_eq!(r.target, format!("/v1/execs/{KEY}/events?cursor=0"));
            Reply::bytes("text/event-stream", b"event: stdout\ndata: {\"sequence\":1,\"data\":\"fast command\\n\"}\n\nevent: exit\ndata: {\"status\":\"exited\",\"exitCode\":7}\n\n".to_vec())
        })),
        deleted(),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let out = shell_command(dir.path(), &server.url)
        .args(["--", "printf", "%s", " a=b\n$HOME "])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"fast command\n");
    server.finish();
}

#[test]
fn cleans_up_after_provisioning_execution_and_attachment_failures() {
    for failure in ["provisioning", "execution", "attachment"] {
        let mut steps = vec![created()];
        match failure {
            "provisioning" => steps.push(http(step(
                "GET",
                format!("/v1/sandboxes/{ID}"),
                None,
                json!({"id":ID,"status":"failed","lifecycleError":"startup failed"}),
            ))),
            "execution" => {
                steps.push(running());
                steps.push(http(Box::new(|_| Reply::json(503, json!({"error":{"code":"SERVICE_UNAVAILABLE","message":"exec unavailable"}})))));
            }
            _ => {
                steps.push(running());
                steps.push(execution(false));
                steps.push(websocket(|mut ws| {
                    ws.close(None).unwrap();
                }));
            }
        }
        steps.push(deleted());
        let server = Server::script(steps);
        let dir = tempfile::tempdir().unwrap();
        let out = shell_command(dir.path(), &server.url)
            .args(["--", "printf", "%s", " a=b\n$HOME "])
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(1),
            "{failure}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(String::from_utf8_lossy(&out.stderr).contains("Deleting temporary sandbox"));
        server.finish();
    }
}

#[test]
fn reports_cleanup_failure_and_preserves_command_failure_status() {
    for code in [0, 7] {
        let server = Server::script(vec![
            created(),
            running(),
            execution(false),
            websocket(move |ws| finish(ws, code)),
            http(Box::new(|_| {
                Reply::json(
                    503,
                    json!({"error":{"code":"SERVICE_UNAVAILABLE","message":"cleanup unavailable"}}),
                )
            })),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let out = shell_command(dir.path(), &server.url)
            .args(["--", "printf", "%s", " a=b\n$HOME "])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(if code == 0 { 1 } else { 7 }));
        assert!(
            String::from_utf8_lossy(&out.stderr).contains(&format!("adapt sandbox delete {ID}"))
        );
        server.finish();
    }
}

#[test]
fn interactive_preflight_rejects_missing_terminal_and_json_before_creation() {
    let dir = tempfile::tempdir().unwrap();
    for flags in [&[][..], &["--json"][..]] {
        let out = shell_command(dir.path(), "http://127.0.0.1:1")
            .args(flags)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1));
        let error = String::from_utf8_lossy(&out.stderr);
        assert!(
            error.contains("interactive terminal")
                || error.contains("cannot be combined with --json"),
            "{error}"
        );
        assert!(!error.contains("Idempotency key"));
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::{
        fs::File,
        io::{Read, Write},
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::process::CommandExt,
        },
        process::{Child, Stdio},
        time::{Duration, Instant},
    };

    struct Terminal {
        master: File,
        slave: File,
        original: libc::termios,
    }

    impl Terminal {
        fn new() -> Self {
            let mut master = -1;
            let mut slave = -1;
            let mut size = libc::winsize {
                ws_col: 100,
                ws_row: 30,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            // SAFETY: All pointers are valid, and successful descriptors are
            // immediately transferred to File so they have a single owner.
            unsafe {
                assert_eq!(
                    libc::openpty(
                        &mut master,
                        &mut slave,
                        std::ptr::null_mut(),
                        std::ptr::null_mut(),
                        // openpty takes *const winsize on Linux, *mut on macOS.
                        &raw mut size
                    ),
                    0
                );
                let master = File::from_raw_fd(master);
                let slave = File::from_raw_fd(slave);
                assert_eq!(
                    libc::fcntl(master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC),
                    0
                );
                assert_eq!(
                    libc::fcntl(slave.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC),
                    0
                );
                assert_eq!(
                    libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK),
                    0
                );
                let mut original = std::mem::zeroed();
                assert_eq!(libc::tcgetattr(slave.as_raw_fd(), &mut original), 0);
                Self {
                    master,
                    slave,
                    original,
                }
            }
        }

        fn spawn(&self, mut cmd: Command) -> Child {
            cmd.stdin(self.slave.try_clone().unwrap())
                .stdout(self.slave.try_clone().unwrap())
                .stderr(Stdio::piped());
            // SAFETY: Only async-signal-safe syscalls execute in the child.
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            cmd.spawn().unwrap()
        }

        fn cleanup(&self) -> Exchange {
            let slave = self.slave.try_clone().unwrap();
            let original = self.original;
            Box::new(move |socket| {
                // Check before process exit: macOS revokes the slave when its
                // controlling session exits, even if this descriptor is open.
                // SAFETY: The descriptor and output pointer remain valid.
                unsafe {
                    let mut current: libc::termios = std::mem::zeroed();
                    assert_eq!(libc::tcgetattr(slave.as_raw_fd(), &mut current), 0);
                    // PENDIN is transient kernel input state, not a raw-mode flag.
                    assert_eq!(
                        current.c_lflag & !libc::PENDIN,
                        original.c_lflag & !libc::PENDIN
                    );
                    assert_eq!(current.c_iflag, original.c_iflag);
                    assert_eq!(current.c_oflag, original.c_oflag);
                }
                deleted()(socket)
            })
        }
    }

    fn wait(mut child: Child, terminal: Option<&Terminal>) -> (i32, String) {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut master = terminal.map(|t| t.master.try_clone().unwrap());
        let status = loop {
            // macOS can block process teardown while terminal output is unread.
            if let Some(master) = &mut master {
                let mut bytes = [0; 4096];
                while matches!(master.read(&mut bytes), Ok(n) if n > 0) {}
            }
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                child.wait().unwrap();
                let mut stderr = String::new();
                child
                    .stderr
                    .take()
                    .unwrap()
                    .read_to_string(&mut stderr)
                    .unwrap();
                panic!("CLI did not exit after cleanup: {stderr}");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let mut stderr = String::new();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        (status.code().unwrap(), stderr)
    }

    #[test]
    fn interactive_exit_and_detach_delete_the_sandbox_and_restore_terminal() {
        for detach in [false, true] {
            let terminal = Terminal::new();
            let mut master = terminal.master.try_clone().unwrap();
            let server = Server::script(vec![
                created(),
                running(),
                execution(true),
                websocket(move |mut ws| {
                    // Wait until raw mode is enabled, then verify Ctrl-C reaches the
                    // guest instead of cancelling the CLI's cleanup supervisor.
                    let deadline = Instant::now() + Duration::from_secs(5);
                    loop {
                        let raw = unsafe {
                            let mut state: libc::termios = std::mem::zeroed();
                            assert_eq!(libc::tcgetattr(master.as_raw_fd(), &mut state), 0);
                            state.c_lflag & libc::ICANON == 0
                        };
                        if raw {
                            break;
                        }
                        assert!(Instant::now() < deadline, "Terminal did not enter raw mode");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    master.write_all(&[3]).unwrap();
                    assert_eq!(ws.read().unwrap(), Message::Binary(vec![3].into()));
                    if detach {
                        master.write_all(&[0x1d]).unwrap();
                        assert!(matches!(ws.read().unwrap(), Message::Close(_)));
                    } else {
                        finish(ws, 9);
                    }
                }),
                terminal.cleanup(),
            ]);
            let dir = tempfile::tempdir().unwrap();
            let (code, stderr) = wait(
                terminal.spawn(shell_command(dir.path(), &server.url)),
                Some(&terminal),
            );
            assert_eq!(code, if detach { 0 } else { 9 }, "{stderr}");
            server.finish();
        }
    }

    #[test]
    fn interruption_during_creation_waits_for_resource_id_then_cleans_up() {
        for (signal, code) in [
            (libc::SIGINT, 130),
            (libc::SIGTERM, 143),
            (libc::SIGHUP, 129),
        ] {
            let (pid_tx, pid_rx) = std::sync::mpsc::channel::<u32>();
            let server = Server::script(vec![
                http(Box::new(move |_| {
                    let pid = pid_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                    // The creation request proves the CLI installed signal handlers.
                    assert_eq!(unsafe { libc::kill(pid as i32, signal) }, 0);
                    std::thread::sleep(Duration::from_millis(100));
                    Reply::json(202, json!({"id":ID}))
                })),
                deleted(),
            ]);
            let dir = tempfile::tempdir().unwrap();
            let child = shell_command(dir.path(), &server.url)
                .args(["--", "true"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            pid_tx.send(child.id()).unwrap();
            let (actual, stderr) = wait(child, None);
            assert_eq!(actual, code, "{stderr}");
            server.finish();
        }
    }
}
