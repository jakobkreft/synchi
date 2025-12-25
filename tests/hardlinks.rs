#![cfg(unix)]

use assert_cmd::cargo::cargo_bin_cmd;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

fn write_config(dir: &Path) -> PathBuf {
    let config_path = dir.join("config.toml");
    fs::write(
        &config_path,
        "root_a = \"./root_a\"\nroot_b = \"./root_b\"\n",
    )
    .unwrap();
    config_path
}

fn run_init(dir: &Path, config_path: &Path) {
    let mut cmd = cargo_bin_cmd!("synchi");
    cmd.current_dir(dir)
        .arg("--config")
        .arg(config_path)
        .arg("init")
        .assert()
        .success();
}

fn run_sync(dir: &Path, config_path: &Path, mode: &str) {
    let mut cmd = cargo_bin_cmd!("synchi");
    cmd.current_dir(dir)
        .arg("--config")
        .arg(config_path)
        .arg("--hardlinks")
        .arg(mode)
        .arg("sync")
        .arg("--yes")
        .assert()
        .success();
}

#[test]
fn hardlinks_preserve_creates_links_on_dest() {
    let dir = tempfile::tempdir().unwrap();
    let root_a = dir.path().join("root_a");
    let root_b = dir.path().join("root_b");
    fs::create_dir_all(&root_a).unwrap();
    fs::create_dir_all(&root_b).unwrap();

    let src_primary = root_a.join("file1.txt");
    let src_link = root_a.join("file2.txt");
    fs::write(&src_primary, "data").unwrap();
    fs::hard_link(&src_primary, &src_link).unwrap();

    let config_path = write_config(dir.path());
    run_init(dir.path(), &config_path);
    run_sync(dir.path(), &config_path, "preserve");

    let dst_primary = root_b.join("file1.txt");
    let dst_link = root_b.join("file2.txt");
    assert!(dst_primary.exists());
    assert!(dst_link.exists());

    let meta_primary = fs::metadata(&dst_primary).unwrap();
    let meta_link = fs::metadata(&dst_link).unwrap();
    assert_eq!(meta_primary.ino(), meta_link.ino());
    assert!(meta_primary.nlink() >= 2);
}

#[test]
fn hardlinks_skip_ignores_hardlinked_files() {
    let dir = tempfile::tempdir().unwrap();
    let root_a = dir.path().join("root_a");
    let root_b = dir.path().join("root_b");
    fs::create_dir_all(&root_a).unwrap();
    fs::create_dir_all(&root_b).unwrap();

    let src_primary = root_a.join("skip1.txt");
    let src_link = root_a.join("skip2.txt");
    fs::write(&src_primary, "data").unwrap();
    fs::hard_link(&src_primary, &src_link).unwrap();

    let config_path = write_config(dir.path());
    run_init(dir.path(), &config_path);
    run_sync(dir.path(), &config_path, "skip");

    assert!(!root_b.join("skip1.txt").exists());
    assert!(!root_b.join("skip2.txt").exists());
}

#[test]
fn hardlinks_copy_from_b_to_a_copies_files() {
    let dir = tempfile::tempdir().unwrap();
    let root_a = dir.path().join("root_a");
    let root_b = dir.path().join("root_b");
    fs::create_dir_all(&root_a).unwrap();
    fs::create_dir_all(&root_b).unwrap();

    let src_primary = root_b.join("b1.txt");
    let src_link = root_b.join("b2.txt");
    fs::write(&src_primary, "data").unwrap();
    fs::hard_link(&src_primary, &src_link).unwrap();

    let config_path = write_config(dir.path());
    run_init(dir.path(), &config_path);
    run_sync(dir.path(), &config_path, "copy");

    let dst_primary = root_a.join("b1.txt");
    let dst_link = root_a.join("b2.txt");
    assert!(dst_primary.exists());
    assert!(dst_link.exists());
}

#[test]
fn hardlinks_skip_from_b_to_a_leaves_a_empty() {
    let dir = tempfile::tempdir().unwrap();
    let root_a = dir.path().join("root_a");
    let root_b = dir.path().join("root_b");
    fs::create_dir_all(&root_a).unwrap();
    fs::create_dir_all(&root_b).unwrap();

    let src_primary = root_b.join("bskip1.txt");
    let src_link = root_b.join("bskip2.txt");
    fs::write(&src_primary, "data").unwrap();
    fs::hard_link(&src_primary, &src_link).unwrap();

    let config_path = write_config(dir.path());
    run_init(dir.path(), &config_path);
    run_sync(dir.path(), &config_path, "skip");

    assert!(!root_a.join("bskip1.txt").exists());
    assert!(!root_a.join("bskip2.txt").exists());
}
