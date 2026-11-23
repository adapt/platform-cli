use super::*;

fn pin() -> Pin {
    Pin {
        format: 1,
        revision: "a".repeat(40),
        targets: ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"]
            .into_iter()
            .map(|target| {
                (
                    target.into(),
                    Artifact {
                        sha256: "b".repeat(64),
                        manifest_sha256: "c".repeat(64),
                        size: 1,
                    },
                )
            })
            .collect(),
    }
}
#[test]
fn pins_require_exact_targets_hashes_and_supported_format() {
    let original = pin();
    original.validate().unwrap();
    let mut bad = pin();
    bad.revision = "../other".into();
    assert!(bad.validate().is_err());
    let mut bad = pin();
    bad.targets.remove("aarch64-apple-darwin");
    assert!(bad.validate().is_err());
    let mut bad = pin();
    bad.targets.values_mut().next().unwrap().size = MAX_ARCHIVE + 1;
    assert!(bad.validate().is_err());
    let mut bad = pin();
    bad.format = 2;
    assert!(bad.validate().is_err());
    let mut bad = pin();
    bad.targets.values_mut().next().unwrap().manifest_sha256 = "B".repeat(64);
    assert!(bad.validate().is_err());
    configured_pin().unwrap();
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::{
        fs,
        io::{Read, Write},
        net::TcpListener,
        os::unix::fs::{PermissionsExt, symlink},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
    };

    fn private_temp() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }
    const TARGET: &str = "aarch64-apple-darwin";
    async fn ensure(
        state: &std::path::Path,
        pin: &Pin,
        target: &str,
        base: &str,
    ) -> anyhow::Result<std::path::PathBuf> {
        install::ensure(state, pin, target, base, std::time::Duration::from_secs(5)).await
    }
    #[tokio::test]
    async fn downloads_honor_the_request_deadline() {
        let dir = private_temp();
        let mut pin = pin();
        let body = bundle::fixture(&mut pin, TARGET, None);
        let server = Server::new(body, 200, None, true);
        assert!(
            install::ensure(
                dir.path(),
                &pin,
                TARGET,
                &server.base,
                std::time::Duration::from_millis(10)
            )
            .await
            .is_err()
        );
        assert!(!installation(dir.path(), &pin, TARGET).exists());
    }

    struct Server {
        base: String,
        requests: Arc<AtomicUsize>,
        done: Arc<std::sync::atomic::AtomicBool>,
        join: Option<thread::JoinHandle<()>>,
    }
    impl Server {
        fn new(body: Vec<u8>, status: u16, declared: Option<usize>, delay: bool) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let base = format!("http://{}/runtime/goblin/", listener.local_addr().unwrap());
            let requests = Arc::new(AtomicUsize::new(0));
            let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let count = requests.clone();
            let stop = done.clone();
            let join = thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let (mut socket, _) = match listener.accept() {
                        Ok(pair) => pair,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(std::time::Duration::from_millis(5));
                            continue;
                        }
                        Err(e) => panic!("{e}"),
                    };
                    // macOS inherits O_NONBLOCK from the listener. These test
                    // connections use blocking reads with a deadline on every OS.
                    socket.set_nonblocking(false).unwrap();
                    socket
                        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                        .unwrap();
                    count.fetch_add(1, Ordering::SeqCst);
                    let mut request = Vec::new();
                    let mut byte = [0];
                    while !request.ends_with(b"\r\n\r\n") {
                        if socket.read_exact(&mut byte).is_err() {
                            break;
                        }
                        request.push(byte[0]);
                    }
                    if !request.ends_with(b"\r\n\r\n") {
                        continue;
                    }
                    let request = String::from_utf8(request).unwrap();
                    assert!(request.starts_with("GET /runtime/goblin/"));
                    assert!(request.contains(&format!("/{TARGET}.tar.gz HTTP/1.1")));
                    assert!(!request.to_ascii_lowercase().contains("authorization:"));
                    if delay {
                        thread::sleep(std::time::Duration::from_millis(100));
                    }
                    let header = format!(
                        "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        declared.unwrap_or(body.len())
                    );
                    let _ = socket.write_all(header.as_bytes());
                    let _ = socket.write_all(&body);
                }
            });
            Self {
                base,
                requests,
                done,
                join: Some(join),
            }
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.done.store(true, Ordering::SeqCst);
            self.join.take().unwrap().join().unwrap();
        }
    }

    #[tokio::test]
    async fn concurrent_installs_download_once_and_cached_reuse_is_offline() {
        let dir = private_temp();
        let mut pin = pin();
        let body = bundle::fixture(&mut pin, TARGET, None);
        let server = Server::new(body, 200, None, true);
        let (first, second) = tokio::join!(
            ensure(dir.path(), &pin, TARGET, &server.base),
            ensure(dir.path(), &pin, TARGET, &server.base)
        );
        let path = first.unwrap();
        assert_eq!(second.unwrap(), path);
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
        assert_eq!(
            fs::metadata(path.join("goblind"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        drop(server);
        assert_eq!(
            ensure(
                dir.path(),
                &pin,
                TARGET,
                "http://127.0.0.1:1/runtime/goblin/"
            )
            .await
            .unwrap(),
            path
        );
        fs::write(path.join("goblind"), b"corrupt").unwrap();
        let error = ensure(
            dir.path(),
            &pin,
            TARGET,
            "http://127.0.0.1:1/runtime/goblin/",
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("corrupt"));
        assert_eq!(fs::read(path.join("goblind")).unwrap(), b"corrupt");
    }

    #[tokio::test]
    async fn corrupt_truncated_wrong_size_or_redirected_downloads_never_install() {
        for case in 0..4 {
            let dir = private_temp();
            let mut pin = pin();
            let mut body = bundle::fixture(&mut pin, TARGET, None);
            let length = body.len();
            let mut declared = None;
            let mut status = 200;
            match case {
                0 => body[10] ^= 1,
                1 => {
                    body.truncate(length / 2);
                    declared = Some(length);
                }
                2 => declared = Some(length + 1),
                _ => status = 302,
            }
            let server = Server::new(body, status, declared, false);
            assert!(
                ensure(dir.path(), &pin, TARGET, &server.base)
                    .await
                    .is_err()
            );
            assert!(!installation(dir.path(), &pin, TARGET).exists());
            assert_eq!(
                fs::read_dir(dir.path().join("runtimes")).unwrap().count(),
                2
            ); // lock and revision; no downloads remain
        }
    }

    #[tokio::test]
    async fn unexpected_duplicate_and_link_entries_fail_without_publishing() {
        for extra in [
            ("unexpected", tar::EntryType::Regular),
            ("bin/goblind", tar::EntryType::Regular),
            ("bin/goblind", tar::EntryType::Symlink),
            ("manifest.json", tar::EntryType::Regular),
        ] {
            let dir = private_temp();
            let mut pin = pin();
            let body = bundle::fixture(&mut pin, TARGET, Some(extra));
            let server = Server::new(body, 200, None, false);
            assert!(
                ensure(dir.path(), &pin, TARGET, &server.base)
                    .await
                    .is_err()
            );
            assert!(!installation(dir.path(), &pin, TARGET).exists());
            assert_eq!(
                fs::read_dir(dir.path().join("runtimes").join(&pin.revision))
                    .unwrap()
                    .count(),
                0
            );
        }
    }

    #[tokio::test]
    async fn manifest_pin_mismatch_and_symlink_caches_are_rejected() {
        let dir = private_temp();
        let mut pin = pin();
        let body = bundle::fixture(&mut pin, TARGET, None);
        pin.targets.get_mut(TARGET).unwrap().manifest_sha256 = "d".repeat(64);
        let server = Server::new(body, 200, None, false);
        assert!(
            ensure(dir.path(), &pin, TARGET, &server.base)
                .await
                .is_err()
        );
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), installation(dir.path(), &pin, TARGET)).unwrap();
        assert!(
            ensure(dir.path(), &pin, TARGET, &server.base)
                .await
                .is_err()
        );
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn separate_releases_preserve_old_executables() {
        let dir = private_temp();
        let mut pin = pin();
        let body = bundle::fixture(&mut pin, TARGET, None);
        let server = Server::new(body, 200, None, false);
        let first = ensure(dir.path(), &pin, TARGET, &server.base)
            .await
            .unwrap();
        let bytes = fs::read(first.join("goblin-worker")).unwrap();
        pin.revision = "e".repeat(40);
        let body = bundle::fixture(&mut pin, TARGET, None);
        let second_server = Server::new(body, 200, None, false);
        let second = ensure(dir.path(), &pin, TARGET, &second_server.base)
            .await
            .unwrap();
        assert_ne!(second, first);
        assert!(second.join("goblind").is_file());
        assert_eq!(fs::read(first.join("goblin-worker")).unwrap(), bytes);
    }
}
