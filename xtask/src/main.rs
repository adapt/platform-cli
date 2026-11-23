//! Repository release tasks, sharing the runtime pin contract with adapt.
use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use reqwest::{Url, blocking::Client};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
#[path = "../../src/runtime/pin.rs"]
mod pin;
use pin::Pin;

#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    task: Task,
}
#[derive(Subcommand)]
enum Task {
    /// Verify both published Goblin archives, then atomically update the CLI pin.
    PinRuntime {
        #[arg(required_unless_present = "check", conflicts_with = "check")]
        revision: Option<String>,
        /// Validate the checked-in lock without downloading.
        #[arg(long)]
        check: bool,
        /// Reject an empty development pin (required for CLI publication).
        #[arg(long, requires = "check")]
        require: bool,
        #[arg(long, default_value = "runtime-lock.json")]
        lock: PathBuf,
        #[arg(long, default_value = "https://platform.adapt.com/runtime/goblin/")]
        base_url: String,
    },
}
fn parse_pin(encoded: &[u8], required: bool) -> Result<Option<Pin>> {
    let pin: Option<Pin> = serde_json::from_slice(encoded)?;
    if let Some(pin) = &pin {
        pin.validate()?;
    } else if required {
        bail!(
            "Pin a published runtime before releasing this CLI: cargo xtask pin-runtime REVISION"
        );
    }
    Ok(pin)
}
fn base_url(base: &str) -> Result<Url> {
    let base = Url::parse(base)?;
    if base.scheme() != "https"
        || base.host_str().is_none()
        || !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
        || !base.path().ends_with('/')
    {
        bail!("Expected an HTTPS directory URL without credentials");
    }
    Ok(base)
}
fn update_pin(
    revision: &str,
    destination: &Path,
    mut get: impl FnMut(&str) -> Result<Box<dyn Read>>,
) -> Result<()> {
    if !pin::hex(revision, 40) {
        bail!("Expected a full Goblin commit SHA");
    }
    let mut encoded = Vec::new();
    get(&format!("{revision}/runtime-lock.json"))?
        .take(65537)
        .read_to_end(&mut encoded)?;
    if encoded.len() > 65536 {
        bail!("Oversized runtime lock");
    }
    let pin = parse_pin(&encoded, true)?.context("Missing runtime pin")?;
    if pin.revision != revision {
        bail!("Published runtime revision mismatch");
    }
    for (target, artifact) in &pin.targets {
        let mut source = get(&format!("{revision}/{target}.tar.gz"))?;
        let mut hash = Sha256::new();
        let mut size = 0u64;
        let mut buffer = [0; 65536];
        loop {
            let read = source.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            size += read as u64;
            if size > artifact.size {
                bail!("Runtime download exceeds pinned size");
            }
            hash.update(&buffer[..read]);
        }
        if size != artifact.size || hex::encode(hash.finalize()) != artifact.sha256 {
            bail!("Published runtime archive checksum or size mismatch");
        }
    }
    // Close before atomic replacement, including on Windows. An interrupted or
    // failed download leaves the old pin untouched; TempPath cleans up failures.
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut file, &pin)?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.into_temp_path().persist(destination)?;
    Ok(())
}
fn main() -> Result<()> {
    match Args::parse().task {
        Task::PinRuntime {
            revision,
            check,
            require,
            lock,
            base_url: base,
        } => {
            if check {
                parse_pin(&fs::read(lock)?, require)?;
            } else {
                let base = base_url(&base)?;
                let client = Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(Duration::from_secs(300))
                    .build()?;
                update_pin(
                    revision
                        .as_deref()
                        .context("Supply a revision or --check")?,
                    &lock,
                    |name| {
                        let response = client
                            .get(base.join(name)?)
                            .header("Accept-Encoding", "identity")
                            .send()?;
                        if response.status() != reqwest::StatusCode::OK {
                            bail!("Runtime download returned HTTP {}", response.status());
                        }
                        Ok(Box::new(response))
                    },
                )?;
                println!("Pinned Goblin runtime in {}", lock.display());
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const BODY: &[u8] = b"fixture archive bytes";
    fn encoded() -> Vec<u8> {
        let targets = ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"]
            .into_iter()
            .map(|t| {
                (
                    t,
                    serde_json::json!({
                        "size": BODY.len(), "sha256": hex::encode(Sha256::digest(BODY)),
                        "manifest_sha256": "b".repeat(64)
                    }),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        serde_json::to_vec(&serde_json::json!({"format":1,"revision":REVISION,
            "targets":targets}))
        .unwrap()
    }
    fn fetch(bodies: Vec<Vec<u8>>) -> impl FnMut(&str) -> Result<Box<dyn Read>> {
        let mut bodies = bodies.into_iter();
        move |name| {
            assert!(name.starts_with(&format!("{REVISION}/")));
            Ok(Box::new(Cursor::new(
                bodies.next().expect("unexpected download"),
            )))
        }
    }
    #[test]
    fn release_requires_a_valid_pin() {
        assert!(parse_pin(b"null", false).unwrap().is_none());
        assert!(parse_pin(b"null", true).is_err());
        assert!(parse_pin(&encoded(), true).unwrap().is_some());
        let mut pin: serde_json::Value = serde_json::from_slice(&encoded()).unwrap();
        pin["targets"]
            .as_object_mut()
            .unwrap()
            .remove("aarch64-apple-darwin");
        assert!(parse_pin(&serde_json::to_vec(&pin).unwrap(), true).is_err());
    }
    #[test]
    fn both_targets_verify_before_replacing_existing_pin() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime-lock.json");
        fs::write(&path, b"null\n").unwrap();
        for bodies in [
            vec![encoded(), BODY.to_vec(), b"bad".to_vec()],
            vec![encoded(), b"bad".to_vec(), BODY.to_vec()],
        ] {
            assert!(update_pin(REVISION, &path, fetch(bodies)).is_err());
            assert_eq!(fs::read(&path).unwrap(), b"null\n");
        }
        update_pin(
            REVISION,
            &path,
            fetch(vec![encoded(), BODY.to_vec(), BODY.to_vec()]),
        )
        .unwrap();
        assert_eq!(
            parse_pin(&fs::read(path).unwrap(), true)
                .unwrap()
                .unwrap()
                .revision,
            REVISION
        );
    }
    #[test]
    fn bounds_and_identity_errors_preserve_the_old_pin() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime-lock.json");
        fs::write(&path, b"null\n").unwrap();
        let mut wrong: serde_json::Value = serde_json::from_slice(&encoded()).unwrap();
        wrong["revision"] = "b".repeat(40).into();
        for lock in [vec![b' '; 65537], serde_json::to_vec(&wrong).unwrap()] {
            assert!(update_pin(REVISION, &path, fetch(vec![lock])).is_err());
        }
        assert!(
            update_pin("../other", &path, |_| panic!(
                "invalid revision must not download"
            ))
            .is_err()
        );
        assert_eq!(fs::read(path).unwrap(), b"null\n");
    }
    #[test]
    fn only_https_directory_origins_are_allowed() {
        for base in [
            "http://localhost/",
            "https://user:password@example.com/",
            "https://example.com/?x=1",
            "https://example.com/#x",
            "https://example.com/file",
        ] {
            assert!(base_url(base).is_err());
        }
        assert!(base_url("https://platform.adapt.com/runtime/goblin/").is_ok());
    }
}
