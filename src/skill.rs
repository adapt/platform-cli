use crate::{
    cli::{SkillCommand, SkillTarget},
    output,
};
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
};

const GUIDE: &str = include_str!("../skills/adapt/SKILL.md");

pub fn run(command: &SkillCommand, json_output: bool) -> Result<()> {
    let (target, install) = match command {
        SkillCommand::Show => {
            io::stdout().lock().write_all(GUIDE.as_bytes())?;
            return Ok(());
        }
        SkillCommand::Install { target } => (*target, true),
        SkillCommand::Uninstall { target } => (*target, false),
    };
    let agents: &[SkillTarget] = match target {
        SkillTarget::All => &[
            SkillTarget::Codex,
            SkillTarget::Claude,
            SkillTarget::Pi,
            SkillTarget::Opencode,
        ],
        _ => std::slice::from_ref(&target),
    };
    // Resolve every override before creating directories or changing any guide.
    let targets = agents
        .iter()
        .map(|agent| Ok((*agent, destination(*agent)?)))
        .collect::<Result<Vec<_>>>()?;
    let mut paths = BTreeMap::new();
    for (_, path) in &targets {
        let parent = path.parent().unwrap();
        if install {
            fs::create_dir_all(parent)
                .with_context(|| format!("Cannot create {}", parent.display()))?;
        }
        let resolved = match fs::canonicalize(parent) {
            Ok(parent) => Some(parent.join("SKILL.md")),
            Err(error) if !install && error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error).with_context(|| format!("Cannot resolve {}", parent.display()));
            }
        };
        paths.insert(path.clone(), resolved);
    }

    // Agents can share a directory, including through symlinks. Lock each real
    // destination once, in sorted order, to serialize concurrent invocations.
    let mut changes = BTreeMap::new();
    for path in paths.values().flatten() {
        changes.entry(path.clone()).or_insert(false);
    }
    let _locks = changes
        .keys()
        .map(|path| lock(path))
        .collect::<Result<Vec<_>>>()?;
    // Check all destinations before replacing or deleting any skill file.
    for (path, changed) in &mut changes {
        let exists = regular_file(path)?;
        *changed = if install {
            !exists
                || fs::read(path).with_context(|| format!("Cannot read {}", path.display()))?
                    != GUIDE.as_bytes()
        } else {
            exists
        };
    }
    for (path, changed) in &changes {
        if *changed {
            if install {
                let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
                file.write_all(GUIDE.as_bytes())?;
                file.as_file().sync_all()?;
                file.persist(path)
                    .with_context(|| format!("Cannot install {}", path.display()))?;
            } else {
                fs::remove_file(path)
                    .with_context(|| format!("Cannot uninstall {}", path.display()))?;
            }
        }
    }

    let changed = |path: &Path| paths[path].as_ref().is_some_and(|path| changes[path]);
    if json_output || !io::stdout().is_terminal() {
        let entries: Vec<_> = targets
            .iter()
            .map(|(agent, path)| {
                json!({"agent": agent_name(*agent), "path": path, "changed": changed(path)})
            })
            .collect();
        let key = if install { "installed" } else { "uninstalled" };
        return output::print(&json!({key: entries}), true);
    }
    let mut out = io::stdout().lock();
    for (agent, path) in &targets {
        let action = match (install, changed(path)) {
            (true, true) => "Installed",
            (true, false) => "Already installed",
            (false, true) => "Uninstalled",
            (false, false) => "Not installed",
        };
        writeln!(
            out,
            "{action} {} skill: {}",
            agent_name(*agent),
            output::safe(&path.display().to_string())
        )?;
        if install {
            writeln!(out, "  {}", usage_hint(*agent))?;
        }
    }
    writeln!(out, "Restart the agent if the change does not appear.")?;
    Ok(())
}

fn regular_file(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => {
            bail!(
                "{} is a symlink; manage its target directly",
                path.display()
            );
        }
        Ok(meta) if meta.is_file() => Ok(true),
        Ok(_) => bail!("{} is not a regular file", path.display()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("Cannot inspect {}", path.display())),
    }
}

fn lock(path: &Path) -> Result<File> {
    let path = path.with_file_name(".SKILL.md.lock");
    regular_file(&path)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("Cannot open skill lock {}", path.display()))?;
    loop {
        match file.lock() {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => {
                result.with_context(|| format!("Cannot lock {}", path.display()))?;
                return Ok(file);
            }
        }
    }
}

fn agent_name(agent: SkillTarget) -> &'static str {
    match agent {
        SkillTarget::Codex => "codex",
        SkillTarget::Claude => "claude",
        SkillTarget::Pi => "pi",
        SkillTarget::Opencode => "opencode",
        SkillTarget::All => unreachable!(),
    }
}

fn usage_hint(agent: SkillTarget) -> &'static str {
    match agent {
        SkillTarget::Codex => "Use $adapt in Codex.",
        SkillTarget::Claude => "Use /adapt in Claude Code.",
        SkillTarget::Pi => "Use /skill:adapt in Pi.",
        SkillTarget::Opencode => "Ask OpenCode to use the adapt skill.",
        SkillTarget::All => unreachable!(),
    }
}

fn destination(agent: SkillTarget) -> Result<PathBuf> {
    let root = match agent {
        SkillTarget::Codex => user_root("CODEX_HOME", ".agents")?,
        SkillTarget::Claude => user_root("CLAUDE_CONFIG_DIR", ".claude")?,
        SkillTarget::Pi | SkillTarget::Opencode => home_root(".agents")?,
        SkillTarget::All => unreachable!(),
    };
    Ok(root.join("skills/adapt/SKILL.md"))
}

fn user_root(variable: &str, default: &str) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(variable).filter(|value| !value.is_empty()) {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            bail!("{variable} must be an absolute path");
        }
        return Ok(path);
    }
    home_root(default)
}

fn home_root(default: &str) -> Result<PathBuf> {
    let home = std::env::home_dir()
        .filter(|path| path.is_absolute())
        .context("Cannot determine your home directory")?;
    Ok(home.join(default))
}
