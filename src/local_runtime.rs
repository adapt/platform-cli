//! Per-user Goblin discovery and bootstrap. No Platform credentials are loaded here.
use crate::backend::local::Goblin;
use anyhow::{Result, bail};
use std::{path::Path, time::Duration};

pub async fn connect(state: &Path, runtime: Option<&Path>, timeout: Duration) -> Result<Goblin> {
    if !cfg!(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64")
    )) {
        bail!("Local sandboxes currently require Apple Silicon macOS or x86_64 Linux with KVM");
    }
    #[cfg(unix)]
    {
        unix::connect(state, runtime, timeout).await
    }
    #[cfg(not(unix))]
    {
        let _ = (state, runtime, timeout);
        unreachable!()
    }
}

#[cfg(unix)]
mod unix {
    use super::*;
    use anyhow::Context;
    use reqwest::Method;
    use serde::Deserialize;
    use std::{
        fs::{self, File, OpenOptions},
        io::{Read, Write},
        os::{
            fd::AsRawFd,
            unix::{
                fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt},
                process::CommandExt,
            },
        },
        process::{Command, Stdio},
    };
    use tokio::time::{Instant, sleep};
    use uuid::Uuid;

    #[derive(Deserialize)]
    struct Ready {
        version: u32,
        socket_path: std::path::PathBuf,
        instance_id: Uuid,
        runtime_version: String,
        #[serde(default)]
        release_revision: Option<String>,
        #[serde(default)]
        runtime_abi: Option<u32>,
    }

    fn private_metadata(path: &Path, directory: bool) -> Result<()> {
        let meta = fs::symlink_metadata(path)?;
        // SAFETY: geteuid has no arguments or memory effects.
        if meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o077 != 0
            || (directory && !meta.is_dir())
            || (!directory && !meta.is_file())
        {
            bail!(
                "{} must be a private, user-owned {}",
                path.display(),
                if directory {
                    "directory"
                } else {
                    "regular file"
                }
            );
        }
        Ok(())
    }

    fn open(path: &Path, create: bool) -> Result<File> {
        let file = OpenOptions::new()
            .read(true)
            .write(create)
            .create(create)
            .truncate(false)
            .append(create)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        private_metadata(path, false)?;
        Ok(file)
    }

    fn read(path: &Path) -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        open(path, false)?.take(65537).read_to_end(&mut bytes)?;
        if bytes.len() > 65536 {
            bail!("Local runtime metadata is too large");
        }
        Ok(bytes)
    }

    async fn discover(
        state: &Path,
        token: &str,
        timeout: Duration,
        pin: Option<&crate::runtime::Pin>,
    ) -> Result<Option<Goblin>> {
        let path = state.join("ready.json");
        let bytes = match read(&path) {
            Ok(bytes) => bytes,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let ready: Ready =
            serde_json::from_slice(&bytes).context("Invalid local runtime discovery file")?;
        if ready.version != 1 || ready.socket_path != state.join("goblind.sock") {
            bail!("Unsupported or invalid local runtime discovery metadata");
        }
        let meta = match fs::symlink_metadata(&ready.socket_path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        // The private directory prevents other users from replacing this endpoint.
        // Reject symlinks and foreign sockets before transmitting the bearer.
        // SAFETY: geteuid has no arguments or memory effects.
        if !meta.file_type().is_socket()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o077 != 0
        {
            bail!("Local runtime socket must be private and user-owned");
        }
        let probe = Goblin::new(
            ready.socket_path.clone(),
            token,
            state.to_owned(),
            Duration::from_secs(2),
        )?;
        let spec = match probe.request(Method::GET, "/v1/spec", None).await {
            Ok(spec) => spec,
            Err(error)
                if error
                    .downcast_ref::<reqwest::Error>()
                    .is_some_and(|e| e.is_connect() || e.is_timeout()) =>
            {
                return Ok(None);
            }
            Err(error) => {
                return Err(error).context("Cannot authenticate the discovered local runtime");
            }
        };
        let instance_id: Uuid = serde_json::from_value(spec["instance_id"].clone())
            .context("Local runtime returned an invalid instance ID")?;
        if instance_id != ready.instance_id {
            return Ok(None);
        }
        if spec["version"] != 1
            || spec["runtime_version"].as_str() != Some(&ready.runtime_version)
            || spec["features"]["sessions"] != true
            || spec["features"]["image_pull"] != true
        {
            bail!(
                "Local runtime is incompatible or has no embedded image firmware; use a compatible Goblin bundle"
            );
        }
        if let Some(pin) = pin {
            compatible_identity(&ready, &spec, pin.runtime_abi)?;
        }
        Ok(Some(Goblin::new(
            ready.socket_path,
            token,
            state.to_owned(),
            timeout,
        )?))
    }

    fn compatible_identity(ready: &Ready, spec: &serde_json::Value, abi: u32) -> Result<()> {
        if ready.runtime_abi != Some(abi)
            || spec["runtime_abi"].as_u64() != Some(u64::from(abi))
            || !ready
                .release_revision
                .as_ref()
                .is_some_and(|r| crate::runtime::hex(r, 40))
            || spec["release_revision"].as_str() != ready.release_revision.as_deref()
        {
            bail!(
                "The running local runtime is incompatible with this CLI's pin. It was left running; use its original CLI/runtime to stop and migrate or recreate sandboxes before changing runtimes"
            );
        }
        Ok(())
    }

    fn compatible_state(state: &Path, abi: u32) -> Result<()> {
        if state.join("ready.json").try_exists()? {
            let previous: Ready = serde_json::from_slice(&read(&state.join("ready.json"))?)?;
            if previous.runtime_abi == Some(abi) {
                return Ok(());
            }
        } else if !state.join("vms").try_exists()? && !state.join("sandboxes").try_exists()? {
            return Ok(());
        }
        bail!(
            "Existing local state belongs to a different or unknown runtime ABI; use the original runtime to migrate or recreate its sandboxes, or choose a fresh --local-state-dir"
        )
    }

    pub async fn connect(
        state: &Path,
        runtime: Option<&Path>,
        timeout: Duration,
    ) -> Result<Goblin> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(state)?;
        private_metadata(state, true)?;
        let state = state.canonicalize()?;
        let lock = open(&state.join("bootstrap.lock"), true)?;
        let deadline = Instant::now() + Duration::from_secs(300);
        loop {
            // SAFETY: the descriptor is live and exclusively owned by this scope.
            if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                break;
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::WouldBlock {
                return Err(error.into());
            }
            if Instant::now() >= deadline {
                bail!("Timed out waiting for local runtime startup");
            }
            sleep(Duration::from_millis(50)).await;
        }
        let token_path = state.join("token");
        if !token_path.try_exists()? {
            let mut file = tempfile::NamedTempFile::new_in(&state)?;
            write!(
                file,
                "{}{}",
                Uuid::new_v4().simple(),
                Uuid::new_v4().simple()
            )?;
            file.as_file().sync_all()?;
            file.persist_noclobber(&token_path)?;
        }
        let token = String::from_utf8(read(&token_path)?)?;
        let token = token.trim();
        if token.len() < 32 || token.len() > 4096 || token.chars().any(char::is_whitespace) {
            bail!("Invalid local runtime token file");
        }
        let pin = if runtime.is_none() {
            crate::runtime::configured_pin()?
        } else {
            None
        };
        if let Some(local) = discover(&state, token, timeout, pin.as_ref()).await? {
            return Ok(local);
        }
        if let Some(pin) = &pin {
            // Stopped workers and persisted VM records can still reference old binaries.
            compatible_state(&state, pin.runtime_abi)?;
        }
        let runtime = crate::runtime::resolve(&state, runtime, timeout).await?;
        let deadline = Instant::now() + Duration::from_secs(30);
        let manager = runtime
            .join("goblind")
            .canonicalize()
            .context("Runtime directory is missing goblind")?;
        let worker = runtime
            .join("goblin-worker")
            .canonicalize()
            .context("Runtime directory is missing goblin-worker")?;
        let log_path = state.join("goblind.log");
        let log = open(&log_path, true)?;
        let mut command = Command::new(manager);
        command
            .args([
                "--state-dir",
                state.to_str().context("Local runtime path must be UTF-8")?,
            ])
            .arg("--unix-socket")
            .arg(state.join("goblind.sock"))
            .arg("--worker")
            .arg(worker)
            .arg("--token-file")
            .arg(token_path)
            .arg("--ready-file")
            .arg(state.join("ready.json"))
            .current_dir(&state)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        // These support system helper lookup and temporary files, without forwarding
        // ADAPT_TOKEN or the user's other credentials to the daemon.
        for key in ["PATH", "TMPDIR"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
        // SAFETY: setsid is an async-signal-safe syscall and the closure has no locks.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        eprintln!("Starting local runtime; log: {}", log_path.display());
        let mut child = command.spawn().context("Cannot start goblind")?;
        loop {
            if let Some(local) = discover(&state, token, timeout, pin.as_ref()).await? {
                return Ok(local);
            }
            if let Some(status) = child.try_wait()? {
                bail!(
                    "Local runtime exited with {status}; inspect {}",
                    log_path.display()
                );
            }
            if Instant::now() >= deadline {
                // Startup is independent of the CLI. A later command can discover
                // a slow recovery; do not kill a daemon that may own live workers.
                bail!(
                    "Local runtime is still starting; inspect {} and retry",
                    log_path.display()
                );
            }
            sleep(Duration::from_millis(50)).await;
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::json;
        use std::os::unix::fs::PermissionsExt;

        fn ready() -> serde_json::Value {
            json!({"version":1,"socket_path":"/unused/goblind.sock","instance_id":Uuid::new_v4(),
                "runtime_version":"0.1.0","runtime_abi":7,"release_revision":"a".repeat(40)})
        }
        #[test]
        fn runtime_abi_and_exact_discovery_identity_are_required_for_managed_reuse() {
            let metadata = ready();
            let ready: Ready = serde_json::from_value(metadata.clone()).unwrap();
            compatible_identity(&ready, &metadata, 7).unwrap();
            assert!(compatible_identity(&ready, &metadata, 8).is_err());
            let mut other = metadata.clone();
            other["release_revision"] = json!("b".repeat(40));
            assert!(compatible_identity(&ready, &other, 7).is_err());
            let mut old = metadata.clone();
            old.as_object_mut().unwrap().remove("runtime_abi");
            assert!(
                compatible_identity(&serde_json::from_value(old.clone()).unwrap(), &old, 7)
                    .is_err()
            );
        }
        #[test]
        fn unknown_or_incompatible_persisted_state_is_preserved() {
            let dir = tempfile::tempdir().unwrap();
            compatible_state(dir.path(), 7).unwrap();
            fs::create_dir(dir.path().join("vms")).unwrap();
            assert!(compatible_state(dir.path(), 7).is_err());
            let path = dir.path().join("ready.json");
            let bytes = serde_json::to_vec(&ready()).unwrap();
            fs::write(&path, &bytes).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            compatible_state(dir.path(), 7).unwrap();
            assert!(compatible_state(dir.path(), 8).is_err());
            assert_eq!(fs::read(path).unwrap(), bytes);
            assert!(dir.path().join("vms").is_dir());
        }
    }
}
