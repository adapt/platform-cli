use crate::support::*;
use serde_json::json;
use std::{
    io::Write,
    time::{Duration, Instant, SystemTime},
};

fn disconnect() -> Exchange {
    Box::new(|mut socket| read_request(&mut socket))
}

#[test]
fn sandbox_creation_wait_recovers_from_redeploy_without_creating_twice() {
    let server = Server::script(vec![
        http(step(
            "POST",
            "/v1/sandboxes".into(),
            Some(json!({"image":"alpine"})),
            json!({"id":ID,"status":"provisioning"}),
        )),
        http(step(
            "GET",
            format!("/v1/sandboxes/{ID}"),
            None,
            json!({"id":ID,"status":"provisioning"}),
        )),
        disconnect(),
        Box::new(|mut socket| {
            let request = read_request(&mut socket);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{\"status\":")
                .unwrap();
            request
        }),
        http(Box::new(|_| {
            let mut reply = Reply::json(503, json!({"message":"Redeploying"}));
            reply.headers.push(("Retry-After".into(), "1".into()));
            reply
        })),
        http(step(
            "GET",
            format!("/v1/sandboxes/{ID}"),
            None,
            json!({"id":ID,"status":"running"}),
        )),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let output = run(
        dir.path(),
        &server.url,
        &[
            "sandbox",
            "create",
            "--image",
            "alpine",
            "--wait",
            "--idempotency-key",
            KEY,
        ],
    );
    assert_eq!(success(&output)["status"], "running");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("retrying"), "{stderr}");
    assert!(!stderr.contains("test-token"));
    let requests = server.finish();
    assert_eq!(requests[0].headers["idempotency-key"], KEY);
    for request in &requests[1..] {
        assert_eq!(request.method, "GET");
        assert_eq!(request.target, format!("/v1/sandboxes/{ID}"));
        assert_eq!(request.headers["authorization"], "Bearer test-token");
        assert_eq!(request.headers["x-organization-id"], ORG);
    }
    assert!(requests[5].at.duration_since(requests[4].at) >= Duration::from_millis(950));
}

#[test]
fn transient_http_errors_retry_the_same_read() {
    for status in [408, 429, 500, 502, 503, 504] {
        let server = Server::start(vec![
            Box::new(move |_| Reply {
                status,
                content_type: "text/html",
                headers: vec![],
                body: b"<html>Gateway temporarily unavailable</html>".to_vec(),
            }),
            step(
                "GET",
                "/v1/sandboxes?limit=2".into(),
                None,
                json!({"sandboxes":[],"nextCursor":null}),
            ),
        ]);
        let dir = tempfile::tempdir().unwrap();
        let output = run(
            dir.path(),
            &server.url,
            &["sandbox", "list", "--limit", "2"],
        );
        assert_eq!(success(&output)["sandboxes"], json!([]));
        let requests = server.finish();
        assert_eq!(requests[0].target, requests[1].target);
    }
}

#[test]
fn permanent_errors_and_invalid_json_fail_without_retrying() {
    for reply in [
        Reply::json(401, json!({"message":"Expired credential"})),
        Reply::json(403, json!({"message":"Forbidden"})),
        Reply::json(404, json!({"message":"Missing resource"})),
        Reply::json(409, json!({"message":"Conflict"})),
        Reply::json(501, json!({"message":"Not implemented"})),
        Reply::bytes("application/json", b"{invalid".to_vec()),
    ] {
        let server = Server::start(vec![Box::new(move |_| reply)]);
        let dir = tempfile::tempdir().unwrap();
        let output = command_with_timeout(dir.path(), &server.url, 1)
            .env("ADAPT_TOKEN", "test-token")
            .args(["sandbox", "get", ID])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("retrying"));
        assert_eq!(server.finish().len(), 1);
    }
}

#[test]
fn mutations_are_not_replayed_after_a_lost_response_even_with_a_key() {
    for method in ["POST", "PUT", "DELETE"] {
        let server = Server::script(vec![disconnect()]);
        let dir = tempfile::tempdir().unwrap();
        let output = command_with_timeout(dir.path(), &server.url, 1)
            .env("ADAPT_TOKEN", "test-token")
            .args([
                "api",
                method,
                "/v1/example",
                "--data",
                "{}",
                "--idempotency-key",
                KEY,
            ])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("retrying"));
        let requests = server.finish();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, method);
        assert_eq!(requests[0].headers["idempotency-key"], KEY);
    }
}

#[test]
fn retry_after_cannot_extend_the_read_timeout() {
    let server = Server::start(vec![Box::new(|_| {
        let mut reply = Reply::json(503, json!({"message":"Redeploying"}));
        reply.headers.push(("Retry-After".into(), "120".into()));
        reply
    })]);
    let dir = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let output = command_with_timeout(dir.path(), &server.url, 1)
        .env("ADAPT_TOKEN", "test-token")
        .args(["sandbox", "get", ID])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("503"), "{stderr}");
    assert!(stderr.contains("retry after 120"), "{stderr}");
    assert!(started.elapsed() < Duration::from_secs(5));
    server.finish();
}

#[test]
fn transient_failures_do_not_extend_the_wait_deadline() {
    let server = Server::start(vec![Box::new(|_| {
        let mut reply = Reply::json(503, json!({"message":"Redeploying"}));
        reply.headers.push(("Retry-After".into(), "2".into()));
        reply
    })]);
    let dir = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let output = run(
        dir.path(),
        &server.url,
        &["images", "wait", ID, "--wait-timeout", "1"],
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Timed out waiting"), "{stderr}");
    assert!(started.elapsed() < Duration::from_secs(5));
    server.finish();
}

#[test]
fn head_reads_honor_retry_after_http_dates() {
    let server = Server::start(vec![
        Box::new(|_| {
            let mut reply = Reply::bytes("application/json", vec![]);
            reply.status = 503;
            reply.headers.push((
                "Retry-After".into(),
                httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(2)),
            ));
            reply
        }),
        Box::new(|request| {
            assert_eq!(request.method, "HEAD");
            Reply::bytes("application/json", vec![])
        }),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let output = run(dir.path(), &server.url, &["api", "HEAD", "/v1/example"]);
    assert_eq!(success(&output)["status"], 200);
    let requests = server.finish();
    assert!(requests[1].at.duration_since(requests[0].at) >= Duration::from_millis(950));
}

#[test]
fn interrupted_authentication_error_bodies_are_not_retried() {
    for status in [401, 403] {
        let server = Server::script(vec![Box::new(move |mut socket| {
            let request = read_request(&mut socket);
            write!(
                socket,
                "HTTP/1.1 {status} Error\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{{"
            )
            .unwrap();
            request
        })]);
        let dir = tempfile::tempdir().unwrap();
        let output = command_with_timeout(dir.path(), &server.url, 1)
            .env("ADAPT_TOKEN", "test-token")
            .args(["sandbox", "get", ID])
            .output()
            .unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(&status.to_string()), "{stderr}");
        assert!(!stderr.contains("retrying"), "{stderr}");
        assert_eq!(server.finish().len(), 1);
    }
}

#[test]
fn read_timeout_covers_backoff_and_the_retried_response_body() {
    let server = Server::script(vec![
        http(Box::new(|_| {
            let mut reply = Reply::json(503, json!({}));
            reply.headers.push(("Retry-After".into(), "2".into()));
            reply
        })),
        Box::new(|mut socket| {
            let request = read_request(&mut socket);
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{")
                .unwrap();
            std::thread::sleep(Duration::from_secs(5));
            request
        }),
    ]);
    let dir = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let output = command_with_timeout(dir.path(), &server.url, 3)
        .env("ADAPT_TOKEN", "test-token")
        .args(["sandbox", "get", ID])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("timed out"), "{stderr}");
    assert!(started.elapsed() < Duration::from_millis(4500));
    assert_eq!(server.finish().len(), 2);
}
