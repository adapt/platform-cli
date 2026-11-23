//! Shared runtime lock contract for the CLI and its release tooling.
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_ARCHIVE: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    pub format: u32,
    pub revision: String,
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
            || !hex(&self.revision, 40)
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
