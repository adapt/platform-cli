use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const GUIDE: &str = include_str!("../skills/adapt/SKILL.md");

fn offline(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_adapt"));
    for name in [
        "ADAPT_TOKEN",
        "ADAPT_PROFILE",
        "ADAPT_ORG_ID",
        "ADAPT_API_URL",
        "ADAPT_CONFIG_DIR",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
    ] {
        command.env_remove(name);
    }
    command
        .env("HOME", root.join("user"))
        .env("USERPROFILE", root.join("user"))
        .args(["--api-url", "not-an-origin", "--config-dir"])
        .arg(root.join("invalid-config"))
        .arg("skill");
    command
}

fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn paths(root: &Path) -> [PathBuf; 2] {
    [
        root.join("user")
            .join(".agents")
            .join("skills/adapt/SKILL.md"),
        root.join("user")
            .join(".claude")
            .join("skills/adapt/SKILL.md"),
    ]
}

#[test]
fn installs_updates_and_uninstalls_all_agents_offline() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir(root.join("invalid-config")).unwrap();
    fs::write(root.join("invalid-config/config.json"), "invalid JSON").unwrap();
    let [shared, claude] = paths(root);
    let installed = success(offline(root).arg("install").output().unwrap());
    let entries = installed["installed"].as_array().unwrap();
    assert_eq!(entries.len(), 4);
    for (entry, (agent, path)) in entries.iter().zip([
        ("codex", &shared),
        ("claude", &claude),
        ("pi", &shared),
        ("opencode", &shared),
    ]) {
        assert_eq!(entry["agent"], agent);
        assert_eq!(entry["path"], path.to_str().unwrap());
        assert_eq!(entry["changed"], true);
        assert_eq!(fs::read_to_string(path).unwrap(), GUIDE);
    }
    let modified = fs::metadata(&shared).unwrap().modified().unwrap();
    let again = success(
        offline(root)
            .args(["install", "all", "--json"])
            .output()
            .unwrap(),
    );
    assert!(
        again["installed"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["changed"] == false)
    );
    assert_eq!(fs::metadata(&shared).unwrap().modified().unwrap(), modified);

    fs::write(&shared, "older guide with local edits").unwrap();
    let notes = shared.with_file_name("notes.md");
    fs::write(&notes, "keep notes").unwrap();
    let updated = success(offline(root).args(["install", "codex"]).output().unwrap());
    assert_eq!(updated["installed"].as_array().unwrap().len(), 1);
    assert_eq!(updated["installed"][0]["changed"], true);
    assert_eq!(fs::read_to_string(&shared).unwrap(), GUIDE);

    let removed = success(offline(root).arg("uninstall").output().unwrap());
    assert_eq!(removed["uninstalled"].as_array().unwrap().len(), 4);
    assert!(
        removed["uninstalled"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["changed"] == true)
    );
    assert!(!shared.exists());
    assert!(!claude.exists());
    assert_eq!(fs::read_to_string(notes).unwrap(), "keep notes");
    let again = success(offline(root).arg("uninstall").output().unwrap());
    assert!(
        again["uninstalled"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["changed"] == false)
    );
}

#[test]
fn show_and_missing_uninstall_do_not_create_directories() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let shown = offline(root).args(["show", "--json"]).output().unwrap();
    assert!(shown.status.success());
    assert_eq!(shown.stdout, GUIDE.as_bytes());
    let removed = success(offline(root).arg("uninstall").output().unwrap());
    assert!(
        removed["uninstalled"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["changed"] == false)
    );
    for args in [["install", "unknown"], ["uninstall", "unknown"]] {
        assert_eq!(
            offline(root).args(args).output().unwrap().status.code(),
            Some(2)
        );
    }
    assert_eq!(fs::read_dir(root).unwrap().count(), 0);
}

#[test]
fn custom_agent_roots_and_aliases_apply_to_install_and_uninstall() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (agent, reported, variable) in [
        ("codex", "codex", "CODEX_HOME"),
        ("claude-code", "claude", "CLAUDE_CONFIG_DIR"),
    ] {
        let profile = root.join(format!("{reported}-profile"));
        let guide = profile.join("skills/adapt/SKILL.md");
        for (action, key) in [("install", "installed"), ("uninstall", "uninstalled")] {
            let result = success(
                offline(root)
                    .env(variable, &profile)
                    .args([action, agent, "--json"])
                    .output()
                    .unwrap(),
            );
            assert_eq!(result[key].as_array().unwrap().len(), 1);
            assert_eq!(result[key][0]["agent"], reported);
            assert_eq!(result[key][0]["path"], guide.to_str().unwrap());
            assert_eq!(result[key][0]["changed"], true);
            assert_eq!(guide.exists(), action == "install");
        }
    }
    assert!(!root.join("user").exists());
}

#[test]
fn selected_shared_agents_ignore_other_agent_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let [shared, claude] = paths(root);
    for (agent, changed) in [("pi", true), ("opencode", false)] {
        let result = success(
            offline(root)
                .env("CODEX_HOME", "unused-relative-path")
                .env("CLAUDE_CONFIG_DIR", "unused-relative-path")
                .args(["install", agent])
                .output()
                .unwrap(),
        );
        assert_eq!(result["installed"][0]["path"], shared.to_str().unwrap());
        assert_eq!(result["installed"][0]["changed"], changed);
    }
    let removed = success(
        offline(root)
            .args(["uninstall", "opencode"])
            .output()
            .unwrap(),
    );
    assert_eq!(removed["uninstalled"][0]["changed"], true);
    assert!(!shared.exists());
    assert!(!claude.exists());
    let result = success(
        offline(root)
            .env("CODEX_HOME", "")
            .args(["install", "codex"])
            .output()
            .unwrap(),
    );
    assert_eq!(result["installed"][0]["path"], shared.to_str().unwrap());
}

#[test]
fn invalid_overrides_are_rejected_before_any_installation() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for variable in ["CODEX_HOME", "CLAUDE_CONFIG_DIR"] {
        for action in ["install", "uninstall"] {
            let result = offline(root)
                .env(variable, "relative")
                .arg(action)
                .output()
                .unwrap();
            assert!(!result.status.success());
            assert!(
                String::from_utf8_lossy(&result.stderr)
                    .contains(&format!("{variable} must be an absolute path"))
            );
        }
    }
    assert!(!root.join("user").exists());
}

#[test]
fn nonfiles_are_rejected_before_changing_any_guide() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let [shared, claude] = paths(root);
    fs::create_dir_all(shared.parent().unwrap()).unwrap();
    fs::write(&shared, "existing guide").unwrap();
    fs::create_dir_all(&claude).unwrap();
    for action in ["install", "uninstall"] {
        let result = offline(root).arg(action).output().unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("not a regular file"));
        assert_eq!(fs::read_to_string(&shared).unwrap(), "existing guide");
        assert!(claude.is_dir());
    }
}

#[test]
fn concurrent_installs_deduplicate_shared_agent_roots() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let common = root.join("common");
    fs::create_dir(&common).unwrap();
    // On Unix also exercise canonicalization through a directory symlink.
    #[cfg(unix)]
    let other = {
        let linked = root.join("linked");
        std::os::unix::fs::symlink(&common, &linked).unwrap();
        linked
    };
    #[cfg(not(unix))]
    let other = common.clone();
    let mut children: Vec<_> = (0..4)
        .map(|_| {
            offline(root)
                .env("CODEX_HOME", &common)
                .env("CLAUDE_CONFIG_DIR", &other)
                .arg("install")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let deadline = Instant::now() + Duration::from_secs(15);
    for child in &mut children {
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                for child in &mut children {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                panic!("Skill installation deadlocked on shared directories");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    let results: Vec<_> = children
        .into_iter()
        .map(|child| success(child.wait_with_output().unwrap()))
        .collect();
    assert_eq!(
        results
            .iter()
            .filter(|result| result["installed"][0]["changed"] == true)
            .count(),
        1
    );
    for result in results {
        assert_eq!(
            result["installed"][0]["changed"],
            result["installed"][1]["changed"]
        );
    }
    assert_eq!(
        fs::read_to_string(common.join("skills/adapt/SKILL.md")).unwrap(),
        GUIDE
    );
    let removed = success(
        offline(root)
            .env("CODEX_HOME", &common)
            .env("CLAUDE_CONFIG_DIR", &other)
            .arg("uninstall")
            .output()
            .unwrap(),
    );
    assert!(
        removed["uninstalled"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["changed"] == true)
    );
}

#[cfg(unix)]
#[test]
fn skill_file_and_lock_symlinks_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let [shared, claude] = paths(root);
    fs::create_dir_all(shared.parent().unwrap()).unwrap();
    fs::create_dir_all(claude.parent().unwrap()).unwrap();
    fs::write(&shared, "existing guide").unwrap();
    for link in [claude.clone(), claude.with_file_name(".SKILL.md.lock")] {
        // Remove the lock left by the previous iteration before linking it.
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(&shared, &link).unwrap();
        for action in ["install", "uninstall"] {
            let result = offline(root).arg(action).output().unwrap();
            assert!(!result.status.success());
            assert!(String::from_utf8_lossy(&result.stderr).contains("is a symlink"));
            assert_eq!(fs::read_to_string(&shared).unwrap(), "existing guide");
            assert!(
                fs::symlink_metadata(&link)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
        }
        fs::remove_file(link).unwrap();
    }
}
