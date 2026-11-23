use super::{Pin, bundle, installation};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::Write,
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::time::{Instant, sleep};

pub(super) fn private(path: &Path, directory: bool) -> Result<()> {
    let m = fs::symlink_metadata(path)?;
    // SAFETY: geteuid has no arguments or memory effects.
    if m.uid() != unsafe { libc::geteuid() }
        || m.mode() & 0o077 != 0
        || (directory && !m.is_dir())
        || (!directory && !m.is_file())
    {
        bail!(
            "{} must be private, user-owned, and not a symlink",
            path.display()
        );
    }
    Ok(())
}
fn directory(path: &Path) -> Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    private(path, true)
}
pub(super) async fn ensure(
    state: &Path,
    pin: &Pin,
    target: &str,
    base: &str,
    timeout: Duration,
) -> Result<PathBuf> {
    pin.validate()?;
    let artifact = pin
        .targets
        .get(target)
        .context("No runtime bundle for this host")?
        .clone();
    let base = url::Url::parse(base)?;
    if (base.scheme() != "https"
        && !(cfg!(test) && base.scheme() == "http" && base.host_str() == Some("127.0.0.1")))
        || !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
        || !base.path().ends_with('/')
    {
        bail!("Runtime downloads require an HTTPS directory URL without credentials");
    }
    directory(state)?;
    let root = state.join("runtimes");
    directory(&root)?;
    let lock_path = root.join("install.lock");
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&lock_path)?;
    private(&lock_path, false)?;
    let deadline = Instant::now() + timeout;
    loop {
        // SAFETY: lock owns the live descriptor throughout installation.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            break;
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::WouldBlock {
            return Err(error.into());
        }
        if Instant::now() >= deadline {
            bail!("Timed out waiting for another runtime installation");
        }
        sleep(Duration::from_millis(50)).await;
    }
    let installed = installation(state, pin, target);
    let parent = installed.parent().unwrap();
    directory(parent)?;
    if installed.try_exists()? || fs::symlink_metadata(&installed).is_ok() {
        private(&installed, true)?;
        bundle::verify(&installed, pin, target).context("Installed runtime is corrupt; its files were preserved because sandboxes may reference them")?;
        return Ok(installed.join("bin"));
    }
    let url = base.join(&format!("{}/{target}.tar.gz", pin.revision))?;
    eprintln!(
        "Downloading local runtime {} ({target}, {} bytes)…",
        &pin.revision[..12],
        artifact.size
    );
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()?;
    let mut response = client
        .get(url)
        .send()
        .await
        .context("Cannot download the pinned local runtime")?;
    if response.status() != reqwest::StatusCode::OK {
        bail!("Runtime download returned HTTP {}", response.status());
    }
    if response
        .content_length()
        .is_some_and(|size| size != artifact.size)
    {
        bail!("Runtime download size differs from its pin");
    }
    let mut archive = tempfile::NamedTempFile::new_in(&root)?;
    let mut hash = Sha256::new();
    let mut received = 0u64;
    while let Some(chunk) = response.chunk().await? {
        received += chunk.len() as u64;
        if received > artifact.size {
            bail!("Runtime download exceeds its pinned size");
        }
        hash.update(&chunk);
        archive.write_all(&chunk)?;
    }
    if received != artifact.size || hex::encode(hash.finalize()) != artifact.sha256 {
        bail!("Runtime download checksum or size mismatch");
    }
    archive.as_file().sync_all()?;
    let pin = pin.clone();
    let target = target.to_owned();
    // Keep the lock and temporary files in the blocking task, even if the user
    // interrupts the async caller. Incomplete directories are never published.
    tokio::task::spawn_blocking(move || -> Result<PathBuf> {
        let _lock = lock;
        let parent = installed.parent().unwrap();
        let staging = tempfile::tempdir_in(parent)?;
        let unpacked = staging.path().join("bundle");
        bundle::extract(archive.path(), &unpacked, &pin, &target)?;
        fs::rename(&unpacked, &installed)?;
        File::open(parent)?.sync_all()?;
        Ok(installed.join("bin"))
    })
    .await?
}
