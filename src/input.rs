use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};
use std::{
    fmt,
    fs::File,
    io::{self, Read},
    path::Path,
    str::FromStr,
};
use uuid::Uuid;

/// A full resource ID or a prefix whose uniqueness is checked by Platform.
#[derive(Clone, Debug)]
pub struct ResourceRef(String);

impl ResourceRef {
    pub fn uuid(&self) -> Option<Uuid> {
        self.0.parse().ok()
    }
}

impl From<Uuid> for ResourceRef {
    fn from(id: Uuid) -> Self {
        Self(id.to_string())
    }
}

impl fmt::Display for ResourceRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for ResourceRef {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Ok(id) = value.parse::<Uuid>() {
            return Ok(id.into());
        }
        let compact = value.len() <= 32 && value.bytes().all(|c| c.is_ascii_hexdigit());
        let hyphenated = value.len() <= 36
            && value.bytes().enumerate().all(|(i, c)| {
                if [8, 13, 18, 23].contains(&i) {
                    c == b'-'
                } else {
                    c.is_ascii_hexdigit()
                }
            });
        if value.is_empty() || !(compact || hyphenated) {
            return Err("Expected a UUID or hexadecimal ID prefix".into());
        }
        Ok(Self(value.to_ascii_lowercase()))
    }
}

pub const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

pub fn bytes(path: &Path, max: u64) -> Result<Vec<u8>> {
    let mut reader: Box<dyn Read> = if path == Path::new("-") {
        Box::new(io::stdin())
    } else {
        Box::new(File::open(path).with_context(|| format!("Cannot read {}", path.display()))?)
    };
    let mut result = Vec::new();
    reader.by_ref().take(max + 1).read_to_end(&mut result)?;
    if result.len() as u64 > max {
        bail!("Input exceeds the {max}-byte limit");
    }
    Ok(result)
}

/// JSON is accepted inline, from @file, or from stdin with '-'.
pub fn json(source: &str) -> Result<Value> {
    let bytes = if source == "-" {
        bytes(Path::new("-"), MAX_FILE_BYTES)?
    } else if let Some(path) = source.strip_prefix('@') {
        bytes(Path::new(path), MAX_FILE_BYTES)?
    } else {
        source.as_bytes().to_vec()
    };
    serde_json::from_slice(&bytes)
        .context("Expected valid JSON (inline, @file.json, or - for stdin)")
}

pub fn object(source: Option<&str>) -> Result<Value> {
    let value = source
        .map(json)
        .transpose()?
        .unwrap_or_else(|| Value::Object(Map::new()));
    if !value.is_object() {
        bail!("Request data must be a JSON object");
    }
    Ok(value)
}

pub fn pair(value: &str) -> Result<(String, String), String> {
    let (key, value) = value.split_once('=').ok_or("Use NAME=VALUE")?;
    if key.is_empty() {
        return Err("Name cannot be empty".into());
    }
    Ok((key.into(), value.into()))
}

pub fn pairs(values: &[(String, String)]) -> Value {
    Value::Object(
        values
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect(),
    )
}

pub fn put<T: serde::Serialize>(body: &mut Value, name: &str, value: Option<T>) -> Result<()> {
    if let Some(value) = value {
        body[name] = serde_json::to_value(value)?;
    }
    Ok(())
}
