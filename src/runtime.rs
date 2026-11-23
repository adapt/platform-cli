//! Immutable Goblin releases selected by this CLI build, independently of cloud profiles.
use crate::{
    cli::{Cli, RuntimeCommand},
    config, output,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(unix)]
mod bundle;
#[cfg(unix)]
mod install;
#[cfg(test)]
mod tests;

pub const MAX_ARCHIVE: u64 = 512 * 1024 * 1024;
#[cfg(unix)]
const DOWNLOAD_BASE: &str = "https://platform.adapt.com/runtime/goblin/";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    pub format: u32,
    pub revision: String,
    pub api_version: u32,
    pub runtime_abi: u32,
    pub guest_protocol: u32,
    pub targets: BTreeMap<String, Artifact>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub sha256: String,
    pub manifest_sha256: String,
    pub size: u64,
}
pub fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
impl Pin {
    pub fn validate(&self) -> Result<()> {
        if self.format != 1
            || self.api_version != 1
            || !hex(&self.revision, 40)
            || self.runtime_abi == 0
            || self.guest_protocol == 0
            || self.targets.keys().map(String::as_str).collect::<Vec<_>>()
                != ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"]
            || self.targets.values().any(|a| {
                !hex(&a.sha256, 64)
                    || !hex(&a.manifest_sha256, 64)
                    || a.size == 0
                    || a.size > MAX_ARCHIVE
            })
        {
            bail!("Invalid or unsupported local runtime pin");
        }
        Ok(())
    }
}
pub fn configured_pin() -> Result<Option<Pin>> {
    let pin: Option<Pin> = serde_json::from_str(include_str!("../runtime-lock.json"))?;
    if let Some(pin) = &pin {
        pin.validate()?;
    }
    Ok(pin)
}
#[cfg(unix)]
pub fn required_pin() -> Result<Pin> {
    configured_pin()?.context("This development build has no published runtime pin. Set --runtime-dir or ADAPT_LOCAL_RUNTIME to a compatible Goblin bundle")
}
pub fn target() -> Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        _ => {
            bail!("Local sandboxes currently require Apple Silicon macOS or x86_64 Linux with KVM")
        }
    }
}
fn state_dir(cli: &Cli) -> Result<PathBuf> {
    cli.local_state_dir
        .clone()
        .map(Ok)
        .unwrap_or_else(|| config::directory(cli.config_dir.as_deref()).map(|p| p.join("local")))
}
pub async fn run(command: &RuntimeCommand, cli: &Cli) -> Result<u8> {
    if cli.cloud_target {
        bail!("Runtime commands manage the local runtime; --cloud is not supported");
    }
    let target = target()?;
    let state = state_dir(cli)?;
    match command {
        RuntimeCommand::Install => {
            let path = resolve(
                &state,
                cli.runtime_dir.as_deref(),
                Duration::from_secs(cli.timeout),
            )
            .await?;
            output::print(&json!({"path":path,"target":target}), cli.json)?;
        }
        RuntimeCommand::Status => {
            let pin = configured_pin()?;
            let path = cli.runtime_dir.clone().or_else(|| {
                pin.as_ref()
                    .map(|p| installation(&state, p, target).join("bin"))
            });
            let installed = path
                .as_ref()
                .is_some_and(|p| p.join("goblind").is_file() && p.join("goblin-worker").is_file());
            // Status is read-only and never downloads or starts a manager.
            output::print(
                &json!({"target":target,"pin":pin,"path":path,"installed":installed,"override":cli.runtime_dir.is_some()}),
                cli.json,
            )?;
        }
    }
    Ok(0)
}
fn installation(state: &Path, pin: &Pin, target: &str) -> PathBuf {
    state.join("runtimes").join(&pin.revision).join(target)
}
pub async fn resolve(state: &Path, explicit: Option<&Path>, timeout: Duration) -> Result<PathBuf> {
    target()?;
    if let Some(path) = explicit {
        let path = path
            .canonicalize()
            .context("Cannot open the local runtime directory")?;
        for name in ["goblind", "goblin-worker"] {
            if !path.join(name).is_file() {
                bail!("Runtime directory is missing {name}");
            }
        }
        return Ok(path);
    }
    #[cfg(unix)]
    {
        let pin = required_pin()?;
        install::ensure(state, &pin, target()?, DOWNLOAD_BASE, timeout).await
    }
    #[cfg(not(unix))]
    {
        let _ = (state, timeout);
        unreachable!()
    }
}
