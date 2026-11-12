use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::{Command, Output, Stdio},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub const ID: &str = "11111111-1111-4111-8111-111111111111";
pub const ORG: &str = "22222222-2222-4222-8222-222222222222";
pub const KEY: &str = "33333333-3333-4333-8333-333333333333";

pub struct Request {
    pub method: String,
    pub target: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    pub at: Instant,
}

impl Request {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).expect("JSON request body")
    }
}

pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            content_type: "application/json",
            headers: vec![],
            body: serde_json::to_vec(&body).unwrap(),
        }
    }
    pub fn bytes(content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status: 200,
            content_type,
            headers: vec![],
            body,
        }
    }
}

pub type Handler = Box<dyn FnOnce(&Request) -> Reply + Send>;
pub type Exchange = Box<dyn FnOnce(TestSocket) -> Request + Send>;

pub struct TestSocket(TcpStream);

impl Read for TestSocket {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        retry_interrupted(|| self.0.read(buffer))
    }
}

impl Write for TestSocket {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        retry_interrupted(|| self.0.write(buffer))
    }

    fn flush(&mut self) -> io::Result<()> {
        retry_interrupted(|| self.0.flush())
    }
}

fn retry_interrupted<T>(mut operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    loop {
        match operation() {
            // Concurrent tests spawn and signal subprocesses. Keep EINTR from
            // interrupting either HTTP requests or WebSocket mock sessions.
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => return result,
        }
    }
}

pub fn http(step: Handler) -> Exchange {
    Box::new(move |mut socket| {
        let request = read_request(&mut socket);
        let reply = step(&request);
        write!(
            socket,
            "HTTP/1.1 {} Test\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
            reply.status,
            reply.content_type,
            reply.body.len()
        )
        .unwrap();
        for (key, value) in reply.headers {
            write!(socket, "{key}: {value}\r\n").unwrap();
        }
        write!(socket, "\r\n").unwrap();
        socket.write_all(&reply.body).unwrap();
        request
    })
}

#[allow(clippy::result_large_err)] // Tungstenite's handshake callback error type.
pub fn websocket(
    session: impl FnOnce(tokio_tungstenite::tungstenite::WebSocket<TestSocket>) + Send + 'static,
) -> Exchange {
    Box::new(move |socket| {
        let mut received = None;
        let ws = tokio_tungstenite::tungstenite::accept_hdr(
            socket,
            |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                assert_eq!(request.uri().path(), format!("/v1/execs/{KEY}/stream"));
                assert_eq!(request.uri().query(), Some("after=0&readOnly=false"));
                assert_eq!(request.headers()["authorization"], "Bearer test-token");
                assert_eq!(request.headers()["x-organization-id"], ORG);
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
        session(ws);
        received.unwrap()
    })
}

pub struct Server {
    pub url: String,
    handle: JoinHandle<Vec<Request>>,
}

impl Server {
    pub fn start(steps: Vec<Handler>) -> Self {
        Self::script(steps.into_iter().map(http).collect())
    }

    pub fn script(steps: Vec<Exchange>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            let mut requests = vec![];
            for step in steps {
                let start = Instant::now();
                let socket = loop {
                    match listener.accept() {
                        Ok((socket, _)) => break socket,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                && start.elapsed() < Duration::from_secs(30) =>
                        {
                            thread::sleep(Duration::from_millis(5))
                        }
                        other => panic!("Expected another request, got {other:?}"),
                    }
                };
                // macOS inherits O_NONBLOCK from the listening socket.
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                requests.push(step(TestSocket(socket)));
            }
            requests
        });
        Self { url, handle }
    }
    pub fn finish(self) -> Vec<Request> {
        self.handle.join().unwrap()
    }
}

pub fn read_request(socket: &mut TestSocket) -> Request {
    let mut bytes = Vec::new();
    let mut chunk = [0; 4096];
    let boundary = loop {
        let n = socket.read(&mut chunk).unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&chunk[..n]);
        if let Some(index) = bytes.windows(4).position(|s| s == b"\r\n\r\n") {
            break index + 4;
        }
        assert!(bytes.len() < 65536);
    };
    let text = std::str::from_utf8(&bytes[..boundary]).unwrap();
    let mut lines = text.split("\r\n");
    let mut parts = lines.next().unwrap().split_whitespace();
    let method = parts.next().unwrap().to_owned();
    let target = parts.next().unwrap().to_owned();
    let headers: HashMap<_, _> = lines
        .filter_map(|s| s.split_once(':'))
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let length: usize = headers
        .get("content-length")
        .map(|s| s.parse().unwrap())
        .unwrap_or(0);
    while bytes.len() < boundary + length {
        let n = socket.read(&mut chunk).unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&chunk[..n]);
    }
    Request {
        method,
        target,
        headers,
        body: bytes[boundary..].to_vec(),
        at: Instant::now(),
    }
}

pub fn step(method: &'static str, target: String, body: Option<Value>, response: Value) -> Handler {
    Box::new(move |request| {
        assert_eq!(request.method, method);
        assert_eq!(request.target, target);
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some("Bearer test-token")
        );
        assert_eq!(
            request.headers.get("x-organization-id").map(String::as_str),
            Some(ORG)
        );
        if let Some(body) = body {
            assert_eq!(request.json(), body);
        }
        Reply::json(200, response)
    })
}

pub fn command(dir: &Path, url: &str) -> Command {
    command_with_timeout(dir, url, 10)
}

pub fn command_with_timeout(dir: &Path, url: &str, timeout: u64) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_adapt"));
    for name in [
        "ADAPT_TOKEN",
        "ADAPT_PROFILE",
        "ADAPT_API_URL",
        "ADAPT_ORG_ID",
        "ADAPT_CONFIG_DIR",
    ] {
        cmd.env_remove(name);
    }
    cmd.arg("--config-dir")
        .arg(dir)
        .args(["--api-url", url, "--timeout"])
        .arg(timeout.to_string());
    cmd
}

pub fn run(dir: &Path, url: &str, args: &[&str]) -> Output {
    command(dir, url)
        .env("ADAPT_TOKEN", "test-token")
        .env("ADAPT_ORG_ID", ORG)
        .args(args)
        .output()
        .unwrap()
}

pub fn with_stdin(mut cmd: Command, input: &[u8]) -> Output {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

pub fn success(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Writes one private config file with several profiles: (name, organization ID).
pub fn save_profiles(dir: &Path, url: &str, profiles: &[(&str, &str)]) {
    let profiles: serde_json::Map<String, Value> = profiles
        .iter()
        .map(|(name, org)| {
            (
                (*name).to_owned(),
                json!({"api_url":url,"organization_id":org,"credential":{"token":"saved-secret","kind":"session","expires_at":4000000000_u64}}),
            )
        })
        .collect();
    std::fs::write(
        dir.join("config.json"),
        serde_json::to_vec(&json!({ "profiles": profiles })).unwrap(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            dir.join("config.json"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
}

pub fn save_profile(dir: &Path, url: &str) {
    let data = json!({"profiles":{"default":{"api_url":url,"organization_id":ORG,"credential":{"token":"saved-secret","kind":"session","expires_at":4000000000_u64}}}});
    std::fs::write(dir.join("config.json"), serde_json::to_vec(&data).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            dir.join("config.json"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
}
