use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use url::Url;

pub const DEFAULT_URL: &str = "https://platform.adaptstaging.com";

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Profile {
    pub api_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<Credential>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Credential {
    pub token: String,
    pub kind: CredentialKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    Session,
    ApiKey,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            api_url: DEFAULT_URL.into(),
            organization_id: None,
            credential: None,
        }
    }
}

pub fn directory(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path.to_owned());
    }
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(path).join("plat"));
    }
    #[cfg(windows)]
    if let Some(path) = std::env::var_os("APPDATA") {
        return Ok(PathBuf::from(path).join("plat"));
    }
    Ok(PathBuf::from(
        std::env::var_os("HOME").context("Set PLAT_CONFIG_DIR; no home directory is available")?,
    )
    .join(".config/plat"))
}

impl Config {
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join("config.json");
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).context("Cannot read CLI configuration"),
        };
        if !meta.is_file() {
            bail!("{} must be a regular file", path.display());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o077 != 0 {
                bail!(
                    "{} contains credentials and must be private; run chmod 600 on this file",
                    path.display()
                );
            }
        }
        serde_json::from_slice(&fs::read(&path)?)
            .with_context(|| format!("Invalid CLI configuration at {}", path.display()))
    }

    pub fn save(&self, dir: &Path) -> Result<()> {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(dir)
            .context("Cannot create CLI configuration directory")?;
        let mut file = tempfile::NamedTempFile::new_in(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        serde_json::to_writer_pretty(&mut file, self)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(dir.join("config.json"))
            .context("Cannot save CLI configuration")?;
        Ok(())
    }
}

/// Only origins are accepted: credentials must never follow an implicit redirect or URL override.
pub fn origin(input: &str) -> Result<Url> {
    let url = Url::parse(input).context("API URL must be an absolute HTTP(S) origin")?;
    validate_http_url(&url)?;
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        bail!("API URL must be an origin, e.g. https://platform.example.com (without /v1)");
    }
    Ok(url)
}

pub fn validate_http_url(url: &Url) -> Result<()> {
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        bail!("Use an HTTP(S) URL without embedded credentials");
    }
    let loopback = match url.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        _ => false,
    };
    if url.scheme() != "https" && !loopback {
        bail!("HTTPS is required except for a loopback development server");
    }
    Ok(())
}
