use crate::support::*;
use serde_json::json;
use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Child, Stdio},
    sync::mpsc,
    time::Duration,
};

fn endpoint(app: &Server) -> serde_json::Value {
    json!({"id":ID,"url":format!("{}/", app.url),"accessMode":"public"})
}

fn spawn(dir: &std::path::Path, platform: &Server, args: &[&str]) -> Child {
    command(dir, &platform.url)
        .env("ADAPT_TOKEN", "test-token")
        .env("ADAPT_ORG_ID", ORG)
        .args(args)
        // Held open until the child is dropped, so `-d -` waits for input.
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// The application must never see Platform credentials, whatever else it receives.
fn assert_no_platform_credentials(request: &Request) {
    assert!(!request.headers.contains_key("x-organization-id"));
    for value in request.headers.values() {
        assert!(!value.contains("test-token"), "Platform token leaked");
    }
}

#[test]
fn streams_events_as_they_arrive_without_platform_credentials() {
    let (read_first, first_read) = mpsc::channel::<()>();
    let app = Server::script(vec![Box::new(move |mut socket| {
        let request = read_request(&mut socket);
        assert_eq!(request.method, "GET");
        assert_eq!(request.target, "/events?since=1");
        assert_eq!(request.headers["accept"], "text/event-stream");
        assert!(!request.headers.contains_key("authorization"));
        assert!(!request.headers.contains_key("x-adapt-authorization"));
        assert_no_platform_credentials(&request);
        // No Content-Length: the body runs until the connection closes.
        write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: one\n\n"
        )
        .unwrap();
        socket.flush().unwrap();
        // A buffering client would wait for the end of the body, so end it and
        // let the test's send fail instead of hanging.
        first_read
            .recv_timeout(Duration::from_secs(10))
            .expect("The CLI did not print the first event while the stream was open");
        write!(socket, "data: two\n\n").unwrap();
        request
    })]);
    let platform = Server::start(vec![step(
        "GET",
        format!("/v1/ingresses/{ID}"),
        None,
        endpoint(&app),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn(
        dir.path(),
        &platform,
        &[
            "endpoints",
            "request",
            ID,
            "/events?since=1",
            "-H",
            "Accept: text/event-stream",
        ],
    );
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    stdout.read_line(&mut first).unwrap();
    stdout.read_line(&mut first).unwrap();
    // The first event reached stdout while the application held the stream open.
    assert_eq!(first, "data: one\n\n");
    read_first
        .send(())
        .expect("The stream ended before the first event was printed");
    let mut rest = String::new();
    stdout.read_to_string(&mut rest).unwrap();
    assert_eq!(rest, "data: two\n\n");
    let status = child.wait().unwrap();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(status.success(), "{stderr}");
    platform.finish();
    app.finish();
}

#[test]
fn authenticated_requests_send_only_the_endpoint_token() {
    let app = Server::start(vec![
        Box::new(|r| {
            assert_eq!(r.method, "POST");
            assert_eq!(r.target, "/api/items");
            assert_eq!(r.headers["x-adapt-authorization"], "Bearer minted-token");
            // The application's own Authorization header passes through untouched.
            assert_eq!(r.headers["authorization"], "Bearer app-secret");
            assert_eq!(r.headers["content-type"], "application/json");
            assert_eq!(r.json(), json!({"name":"a"}));
            assert_no_platform_credentials(r);
            Reply::json(201, json!({"created":true}))
        }),
        Box::new(|r| {
            // The path is appended to the endpoint URL, keeping its prefix.
            assert_eq!(r.target, "/prefix/hello?x=1");
            assert_eq!(r.headers["x-adapt-authorization"], "Bearer given-token");
            assert!(!r.headers.contains_key("authorization"));
            assert_no_platform_credentials(r);
            Reply::bytes("text/plain", b"hello".to_vec())
        }),
    ]);
    let platform = Server::start(vec![
        step("GET", "/v1/ingresses/11111111".into(), None, endpoint(&app)),
        // The token is created for the resolved ID, not the prefix.
        step(
            "POST",
            format!("/v1/ingresses/{ID}/access-tokens"),
            Some(json!({})),
            json!({"token":"minted-token","expiresAt":"2030-01-01T00:00:00Z"}),
        ),
        step(
            "GET",
            format!("/v1/ingresses/{ID}"),
            None,
            json!({"id":ID,"url":format!("{}/prefix/", app.url)}),
        ),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let output = run(
        dir.path(),
        &platform.url,
        &[
            "endpoints",
            "request",
            "11111111",
            "/api/items",
            "-X",
            "POST",
            "--new-token",
            "-H",
            "Authorization: Bearer app-secret",
            "-H",
            "Content-Type: application/json",
            "-d",
            r#"{"name":"a"}"#,
        ],
    );
    assert_eq!(success(&output), json!({"created":true}));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("minted-token"));

    let output = run(
        dir.path(),
        &platform.url,
        &[
            "endpoints",
            "request",
            ID,
            "/hello?x=1",
            "--token",
            "given-token",
        ],
    );
    assert!(output.status.success());
    assert_eq!(output.stdout, b"hello");
    platform.finish();
    app.finish();
}

#[test]
fn body_sources_preserve_bytes() {
    const BODY: &[u8] = b"first\0\xff\nlast";
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("body.bin");
    std::fs::write(&path, BODY).unwrap();
    let mut sources = vec![
        "-".to_owned(),
        "@-".to_owned(),
        format!("@{}", path.display()),
    ];
    if cfg!(unix) {
        sources.push("@/dev/stdin".into());
    }
    for source in sources {
        let app = Server::start(vec![Box::new(|request| {
            assert_eq!(request.method, "POST");
            assert_eq!(request.body, BODY);
            assert!(!request.headers.contains_key("content-type"));
            Reply::bytes("application/octet-stream", BODY.to_vec())
        })]);
        let platform = Server::start(vec![step(
            "GET",
            format!("/v1/ingresses/{ID}"),
            None,
            endpoint(&app),
        )]);
        let mut cmd = command(dir.path(), &platform.url);
        cmd.env("ADAPT_TOKEN", "test-token")
            .env("ADAPT_ORG_ID", ORG)
            .args(["endpoints", "request", ID, "-X", "POST", "-d", &source]);
        let output = with_stdin(cmd, BODY);
        assert!(
            output.status.success(),
            "{source}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, BODY, "{source}");
        platform.finish();
        app.finish();
    }
}

#[test]
fn body_file_errors_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let large = dir.path().join("large.bin");
    std::fs::File::create(&large)
        .unwrap()
        .set_len(16 * 1024 * 1024 + 1)
        .unwrap();
    for (path, message) in [
        (dir.path().join("missing.bin"), "Cannot read"),
        (large, "Input exceeds the 16777216-byte limit"),
    ] {
        let platform = Server::start(vec![step(
            "GET",
            format!("/v1/ingresses/{ID}"),
            None,
            json!({"id":ID,"url":"http://127.0.0.1:9/"}),
        )]);
        let output = run(
            dir.path(),
            &platform.url,
            &[
                "endpoints",
                "request",
                ID,
                "-d",
                &format!("@{}", path.display()),
            ],
        );
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(message), "{stderr}");
        platform.finish();
    }
}

#[test]
fn error_statuses_print_the_body_and_redirects_are_not_followed() {
    let app = Server::start(vec![
        Box::new(|_| Reply {
            status: 404,
            content_type: "text/plain",
            headers: vec![],
            body: b"no such page".to_vec(),
        }),
        Box::new(|_| Reply {
            status: 302,
            content_type: "text/plain",
            headers: vec![("Location".into(), "https://other.example/login".into())],
            body: vec![],
        }),
    ]);
    let platform = Server::start(vec![
        step("GET", format!("/v1/ingresses/{ID}"), None, endpoint(&app)),
        step("GET", format!("/v1/ingresses/{ID}"), None, endpoint(&app)),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let output = run(
        dir.path(),
        &platform.url,
        &["endpoints", "request", ID, "/missing"],
    );
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, b"no such page");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("HTTP 404"), "{stderr}");
    // An application 401/404 is not a Platform sign-in problem.
    assert!(!stderr.contains("auth login"), "{stderr}");

    let output = run(dir.path(), &platform.url, &["endpoints", "request", ID]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("HTTP 302"), "{stderr}");
    assert!(stderr.contains("redirects are not followed"), "{stderr}");
    assert!(stderr.contains("https://other.example/login"), "{stderr}");
    platform.finish();
    app.finish();
}

#[test]
fn paths_and_headers_cannot_redirect_or_impersonate_credentials() {
    let dir = tempfile::tempdir().unwrap();
    for path in [
        "https://other.example/x",
        "//other.example/x",
        "/\\other.example/x",
        "relative",
        "/page#fragment",
    ] {
        let output = run(
            dir.path(),
            "http://localhost:3000",
            &["endpoints", "request", ID, path],
        );
        assert_eq!(output.status.code(), Some(1), "{path}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("absolute path"));
    }
    // Rejected headers may hold credentials, so errors never repeat them.
    for header in [
        "X-Adapt-Authorization: Bearer header-secret",
        "Authorization: Bearer header-secret\u{1}",
        "header-secret",
    ] {
        let output = run(
            dir.path(),
            "http://localhost:3000",
            &["endpoints", "request", ID, "-H", header],
        );
        assert_eq!(output.status.code(), Some(1), "{header}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!stderr.contains("header-secret"), "{stderr}");
    }
    let output = run(
        dir.path(),
        "http://localhost:3000",
        &["endpoints", "request", ID, "--token", "a", "--new-token"],
    );
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn transport_errors_do_not_print_the_request_url() {
    // Nothing listens on this port once the listener is dropped.
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let platform = Server::start(vec![step(
        "GET",
        format!("/v1/ingresses/{ID}"),
        None,
        json!({"id":ID,"url":format!("http://{closed}/")}),
    )]);
    let dir = tempfile::tempdir().unwrap();
    let output = run(
        dir.path(),
        &platform.url,
        &["endpoints", "request", ID, "/api?token=query-secret"],
    );
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Endpoint request failed"), "{stderr}");
    assert!(!stderr.contains("query-secret"), "{stderr}");
    platform.finish();
}

/// A Platform endpoint lookup that reports when it arrives.
#[cfg(unix)]
fn lookup(url: String, seen: mpsc::Sender<()>) -> Handler {
    let reply = step(
        "GET",
        format!("/v1/ingresses/{ID}"),
        None,
        json!({"id":ID,"url":url}),
    );
    Box::new(move |request| {
        let reply = reply(request);
        seen.send(()).unwrap();
        reply
    })
}

/// Sends Ctrl-C after the lookup, once the CLI has had time to block, and returns
/// the exit code. Fails instead of hanging if Ctrl-C does not stop the CLI.
#[cfg(unix)]
fn interrupt(mut child: Child, looked_up: mpsc::Receiver<()>) -> Option<i32> {
    // While the lookup is pending, the CLI's Ctrl-C handler is already installed.
    looked_up.recv_timeout(Duration::from_secs(10)).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    for _ in 0..100 {
        if let Some(status) = child.try_wait().unwrap() {
            return status.code();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    panic!("Ctrl-C did not stop the request");
}

#[cfg(unix)]
fn assert_body_read_is_interruptible(source: &str) {
    let (seen, looked_up) = mpsc::channel();
    // The body never ends, so the endpoint is never contacted.
    let platform = Server::start(vec![lookup("http://127.0.0.1:9/".into(), seen)]);
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn(
        dir.path(),
        &platform,
        &["endpoints", "request", ID, "-d", source],
    );
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"partial body")
        .unwrap();
    assert_eq!(interrupt(child, looked_up), Some(130), "{source}");
    platform.finish();
}

#[cfg(unix)]
#[test]
fn ctrl_c_stops_a_request_waiting_for_stdin() {
    assert_body_read_is_interruptible("-");
}

#[cfg(unix)]
#[test]
fn ctrl_c_stops_a_request_waiting_for_stdin_via_at_dash() {
    assert_body_read_is_interruptible("@-");
}

#[cfg(unix)]
#[test]
fn ctrl_c_stops_a_request_waiting_for_stdin_device() {
    assert_body_read_is_interruptible("@/dev/stdin");
}

#[cfg(unix)]
fn assert_fifo_read_is_interruptible(with_writer: bool) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("body.fifo");
    let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(
        unsafe { libc::mkfifo(name.as_ptr(), 0o600) },
        0,
        "{}",
        std::io::Error::last_os_error()
    );
    // Without a writer, open blocks; with one held open, reading waits for EOF.
    let _writer = with_writer.then(|| {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap()
    });
    assert_body_read_is_interruptible(&format!("@{}", path.display()));
}

#[cfg(unix)]
#[test]
fn ctrl_c_stops_a_request_opening_a_fifo() {
    assert_fifo_read_is_interruptible(false);
}

#[cfg(unix)]
#[test]
fn ctrl_c_stops_a_request_reading_a_fifo() {
    assert_fifo_read_is_interruptible(true);
}

#[cfg(unix)]
#[test]
fn ctrl_c_stops_a_request_whose_stdout_is_full() {
    let app = Server::script(vec![Box::new(|mut socket| {
        let request = read_request(&mut socket);
        // Far more than a pipe holds: the CLI blocks writing to its unread stdout.
        let _ = write!(socket, "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n");
        let _ = socket.write_all(&vec![b'x'; 8 << 20]);
        request
    })]);
    let (seen, looked_up) = mpsc::channel();
    let platform = Server::start(vec![lookup(format!("{}/", app.url), seen)]);
    let dir = tempfile::tempdir().unwrap();
    let child = spawn(dir.path(), &platform, &["endpoints", "request", ID]);
    assert_eq!(interrupt(child, looked_up), Some(130));
    platform.finish();
    app.finish();
}
