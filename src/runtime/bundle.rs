use super::{Artifact, Pin, hex};
use anyhow::{Context, Result, bail};
use flate2::bufread::GzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

const MAX_CONTENT: u64 = 1024 * 1024 * 1024;
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: u32,
    release: String,
    revision: String,
    dirty: bool,
    target: String,
    // Existing Goblin bundle metadata; local installation does not gate on it.
    runtime_abi: u32,
    guest_protocol: u32,
    management_protocol: u32,
    files: BTreeMap<String, Record>,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    sha256: String,
    size: u64,
    mode: u32,
}

fn names(target: &str) -> BTreeSet<String> {
    [
        "bin/goblin",
        "bin/goblind",
        "bin/goblin-worker",
        "bin/goblin-node",
        "docs/DEPLOYMENT.md",
        "docs/FIRMWARE.md",
        "guest/kernel.config",
        "guest/kernel-source.json",
        "guest/kernel-manifest.json",
        "guest/KERNEL-COPYING",
        "guest/KERNEL-GPL-2.0",
        "guest/KERNEL-Linux-syscall-note",
        "guest/initramfs-manifest.json",
        "guest/image-builder-manifest.json",
        "guest/hello.json",
        "guest/BUSYBOX-COPYRIGHT",
        "guest/IMAGE-TOOLS-NOTICES",
        if target == "aarch64-apple-darwin" {
            "packaging/goblin-worker.entitlements"
        } else {
            "packaging/goblind.service"
        },
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}
fn manifest(bytes: &[u8], pin: &Pin, target: &str, artifact: &Artifact) -> Result<Manifest> {
    if bytes.len() > 65536 || hex::encode(Sha256::digest(bytes)) != artifact.manifest_sha256 {
        bail!("Local runtime manifest checksum mismatch");
    }
    let m: Manifest = serde_json::from_slice(bytes)?;
    if m.format != 2
        || m.dirty
        || m.revision != pin.revision
        || m.release != pin.revision
        || m.target != target
        || m.files.keys().cloned().collect::<BTreeSet<_>>() != names(target)
    {
        bail!("Local runtime bundle identity or contents do not match the pinned release");
    }
    let mut total = 0u64;
    for (name, r) in &m.files {
        total = total.checked_add(r.size).context("Runtime size overflow")?;
        if r.size == 0
            || total > MAX_CONTENT
            || !hex(&r.sha256, 64)
            || r.mode
                != if name.starts_with("bin/") {
                    0o755
                } else {
                    0o644
                }
        {
            bail!("Invalid runtime file metadata");
        }
    }
    Ok(m)
}
fn copy(mut source: impl Read, mut output: Option<&mut File>, record: &Record) -> Result<()> {
    let mut hash = Sha256::new();
    let mut size = 0;
    let mut buffer = [0; 65536];
    loop {
        let n = source.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        if size > record.size {
            bail!("Runtime file exceeds its expected size");
        }
        hash.update(&buffer[..n]);
        if let Some(file) = output.as_mut() {
            file.write_all(&buffer[..n])?;
        }
    }
    if size != record.size || hex::encode(hash.finalize()) != record.sha256 {
        bail!("Runtime file checksum mismatch");
    }
    Ok(())
}
pub(super) fn extract(archive: &Path, destination: &Path, pin: &Pin, target: &str) -> Result<()> {
    let artifact = &pin.targets[target];
    // The downloader verifies the compressed bytes before this function runs.
    fs::create_dir(destination)?;
    fs::set_permissions(destination, fs::Permissions::from_mode(0o700))?;
    let source = std::io::BufReader::new(File::open(archive)?);
    let mut tar = tar::Archive::new(GzDecoder::new(source).take(MAX_CONTENT + 131072));
    let mut entries = tar.entries()?.raw(true);
    let mut first = entries.next().context("Missing runtime manifest")??;
    if first.path_bytes().as_ref() != b"manifest.json"
        || !first.header().entry_type().is_file()
        || first.size() > 65536
    {
        bail!("Expected a bounded regular manifest.json first");
    }
    let mut bytes = Vec::new();
    first.read_to_end(&mut bytes)?;
    let m = manifest(&bytes, pin, target, artifact)?;
    fs::write(destination.join("manifest.json"), bytes)?;
    let mut seen = BTreeSet::new();
    for entry in entries {
        let mut entry = entry?;
        let name = String::from_utf8(entry.path_bytes().into_owned())?;
        let record = m
            .files
            .get(&name)
            .context("Unexpected runtime archive path")?;
        if !seen.insert(name.clone())
            || !entry.header().entry_type().is_file()
            || entry.size() != record.size
            || entry.header().mode()? != record.mode
        {
            bail!("Invalid or duplicate runtime archive entry");
        }
        let path = destination.join(name);
        fs::create_dir_all(path.parent().unwrap())?;
        let mut file = File::options()
            .write(true)
            .create_new(true)
            .mode(record.mode)
            .open(&path)?;
        copy(&mut entry, Some(&mut file), record)?;
        file.set_permissions(fs::Permissions::from_mode(record.mode))?;
        file.sync_all()?;
    }
    if seen.len() != m.files.len() {
        bail!("Missing runtime archive files");
    }
    let mut decoder = tar.into_inner();
    let mut tail = Vec::new();
    decoder.by_ref().take(65537).read_to_end(&mut tail)?;
    let mut compressed = decoder.into_inner().into_inner();
    let mut extra = [0; 1];
    if tail.len() > 65536 || tail.iter().any(|b| *b != 0) || compressed.read(&mut extra)? != 0 {
        bail!("Unexpected runtime archive trailer");
    }
    File::open(destination.join("manifest.json"))?.sync_all()?;
    Ok(())
}
pub(super) fn verify(path: &Path, pin: &Pin, target: &str) -> Result<()> {
    for directory in [
        path.to_owned(),
        path.join("bin"),
        path.join("guest"),
        path.join("docs"),
        path.join("packaging"),
    ] {
        if !fs::symlink_metadata(directory)?.is_dir() {
            bail!("Runtime directories must not be symlinks");
        }
    }
    let mut source = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path.join("manifest.json"))?;
    if !source.metadata()?.is_file() {
        bail!("Runtime manifest is not a regular file");
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut source)
        .take(65537)
        .read_to_end(&mut bytes)?;
    let m = manifest(&bytes, pin, target, &pin.targets[target])?;
    for (name, record) in m.files {
        let file = File::options()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path.join(name))?;
        let meta = file.metadata()?;
        if !meta.is_file()
            || meta.len() != record.size
            || meta.permissions().mode() & 0o7777 != record.mode
        {
            bail!("Installed runtime file metadata changed");
        }
        copy(file, None, &record)?;
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn fixture(
    pin: &mut Pin,
    target: &str,
    extra: Option<(&str, tar::EntryType)>,
) -> Vec<u8> {
    let payload = b"test executable\n";
    let files = names(target)
        .into_iter()
        .map(|name| {
            let record = Record {
                sha256: hex::encode(Sha256::digest(payload)),
                size: payload.len() as u64,
                mode: if name.starts_with("bin/") {
                    0o755
                } else {
                    0o644
                },
            };
            (name, record)
        })
        .collect();
    let m = Manifest {
        format: 2,
        release: pin.revision.clone(),
        revision: pin.revision.clone(),
        dirty: false,
        target: target.into(),
        runtime_abi: 7,
        guest_protocol: 10,
        management_protocol: 1,
        files,
    };
    let bytes = serde_json::to_vec(&m).unwrap();
    let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::default(),
    ));
    fn add<W: Write>(
        tar: &mut tar::Builder<W>,
        name: &str,
        mode: u32,
        bytes: &[u8],
        kind: tar::EntryType,
    ) {
        let mut h = tar::Header::new_ustar();
        h.set_size(bytes.len() as u64);
        h.set_mode(mode);
        h.set_entry_type(kind);
        h.set_cksum();
        tar.append_data(&mut h, name, bytes).unwrap();
    }
    add(
        &mut tar,
        "manifest.json",
        0o644,
        &bytes,
        tar::EntryType::Regular,
    );
    for (name, record) in &m.files {
        add(
            &mut tar,
            name,
            record.mode,
            payload,
            tar::EntryType::Regular,
        );
    }
    if let Some((name, kind)) = extra {
        add(&mut tar, name, 0o755, payload, kind);
    }
    let data = tar.into_inner().unwrap().finish().unwrap();
    pin.targets.insert(
        target.into(),
        Artifact {
            sha256: hex::encode(Sha256::digest(&data)),
            manifest_sha256: hex::encode(Sha256::digest(bytes)),
            size: data.len() as u64,
        },
    );
    data
}
