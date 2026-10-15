use std::sync::Arc;
use std::time::{Duration, Instant};

use rgit_config::{Error, RepoDirs, edit_toml, set_private};
use tempfile::TempDir;

fn repo(dir: &TempDir) -> RepoDirs {
    RepoDirs {
        worktree_root: Some(dir.path().join("work")),
        common_dir: dir.path().join("git"),
    }
}

#[test]
fn edit_keeps_comments_and_existing_keys() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("sub").join("c.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "# mine\n[output]\ncompact = true\n").unwrap();
    edit_toml(&path, |doc| {
        doc["commit"]["change_id"] = toml_edit::value(true);
        Ok::<(), Error>(())
    })
    .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("# mine"), "{text}");
    assert!(text.contains("compact = true"), "{text}");
    assert!(text.contains("change_id = true"), "{text}");
    assert!(!path.with_extension("toml.lock").exists());
}

#[test]
fn edit_creates_parent_dirs_and_a_missing_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("a").join("b").join("c.toml");
    edit_toml(&path, |_| Ok::<(), Error>(())).unwrap();
    assert!(path.exists());
}

#[test]
fn failed_edit_writes_nothing_and_drops_the_lock() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("c.toml");
    std::fs::write(&path, "a = 1\n").unwrap();
    let result = edit_toml(&path, |doc| {
        doc["a"] = toml_edit::value(2);
        Err(Error::BadKey("x".into()))
    });
    assert!(result.is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "a = 1\n");
    assert!(!dir.path().join("c.toml.lock").exists());
}

#[test]
fn set_private_writes_a_table() {
    let dir = TempDir::new().unwrap();
    let repo = repo(&dir);
    set_private(&repo, "workflow.preset", "trunk").unwrap();
    let text = std::fs::read_to_string(repo.private_file()).unwrap();
    assert_eq!(text, "[workflow]\npreset = \"trunk\"\n");
}

#[test]
fn set_private_nested_key_uses_a_nested_table() {
    let dir = TempDir::new().unwrap();
    let repo = repo(&dir);
    set_private(&repo, "keys.vim.stage", "s").unwrap();
    let text = std::fs::read_to_string(repo.private_file()).unwrap();
    assert_eq!(text, "[keys.vim]\nstage = \"s\"\n");
}

#[test]
fn concurrent_edits_all_land() {
    let dir = TempDir::new().unwrap();
    let path = Arc::new(dir.path().join("stacks.toml"));
    let threads: Vec<_> = (0..2)
        .map(|t| {
            let path = Arc::clone(&path);
            std::thread::spawn(move || {
                for i in 0..20 {
                    edit_toml(&path, |doc| {
                        doc[&format!("t{t}_{i}")] = toml_edit::value(i);
                        Ok::<(), Error>(())
                    })
                    .unwrap();
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    let doc: toml::Table = std::fs::read_to_string(&*path).unwrap().parse().unwrap();
    assert_eq!(doc.len(), 40);
}

#[test]
fn held_lock_makes_an_edit_wait_then_succeed() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("c.toml");
    let lock = dir.path().join("c.toml.lock");
    std::fs::write(&lock, "").unwrap();
    let release = lock.clone();
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        std::fs::remove_file(release).unwrap();
    });
    let start = Instant::now();
    edit_toml(&path, |doc| {
        doc["a"] = toml_edit::value(1);
        Ok::<(), Error>(())
    })
    .unwrap();
    releaser.join().unwrap();
    assert!(start.elapsed() >= Duration::from_millis(250));
    assert!(std::fs::read_to_string(&path).unwrap().contains("a = 1"));
}
