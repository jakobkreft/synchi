use assert_cmd::cargo::cargo_bin_cmd;
use predicates::str::contains;
use std::fs;

#[test]
fn help_includes_name() {
    let mut cmd = cargo_bin_cmd!("synchi");
    cmd.arg("--help")
        .assert()
        .success()
        .stdout(contains("synchi"));
}

#[test]
fn missing_roots_exit_with_error() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    fs::write(&config_path, "").unwrap();

    let mut cmd = cargo_bin_cmd!("synchi");
    cmd.arg("--config")
        .arg(&config_path)
        .arg("status")
        .assert()
        .failure()
        .stderr(contains("Root A not defined"));
}

#[test]
fn scp_style_root_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let root_a = dir.path().join("root_a");
    fs::create_dir_all(&root_a).unwrap();

    let config_path = dir.path().join("config.toml");
    fs::write(
        &config_path,
        "root_a = \"./root_a\"\nroot_b = \"user@host:/data\"\n",
    )
    .unwrap();

    let mut cmd = cargo_bin_cmd!("synchi");
    cmd.current_dir(dir.path())
        .arg("--config")
        .arg(&config_path)
        .arg("status")
        .assert()
        .failure()
        .stderr(contains("ssh://"));
}

#[test]
fn status_without_init_reports_message() {
    let dir = tempfile::tempdir().unwrap();
    let root_a = dir.path().join("root_a");
    let root_b = dir.path().join("root_b");
    fs::create_dir_all(&root_a).unwrap();
    fs::create_dir_all(&root_b).unwrap();

    let config_path = dir.path().join("config.toml");
    fs::write(
        &config_path,
        "root_a = \"./root_a\"\nroot_b = \"./root_b\"\n",
    )
    .unwrap();

    let mut cmd = cargo_bin_cmd!("synchi");
    cmd.current_dir(dir.path())
        .arg("--config")
        .arg(&config_path)
        .arg("status")
        .assert()
        .success()
        .stdout(contains("Not initialized"));

    assert!(!root_a.join(".synchi").exists());
    assert!(!root_b.join(".synchi").exists());
}

#[test]
fn sync_with_empty_include_performs_no_ops() {
    let dir = tempfile::tempdir().unwrap();
    let root_a = dir.path().join("root_a");
    let root_b = dir.path().join("root_b");
    fs::create_dir_all(&root_a).unwrap();
    fs::create_dir_all(&root_b).unwrap();

    fs::write(root_a.join("file_a.txt"), "data").unwrap();
    fs::write(root_b.join("file_b.txt"), "data").unwrap();

    let config_path = dir.path().join("config.toml");
    fs::write(
        &config_path,
        "root_a = \"./root_a\"\nroot_b = \"./root_b\"\ninclude = []\n",
    )
    .unwrap();

    let mut cmd = cargo_bin_cmd!("synchi");
    cmd.current_dir(dir.path())
        .arg("--config")
        .arg(&config_path)
        .arg("sync")
        .arg("--yes")
        .assert()
        .success()
        .stdout(contains("Executing 0 operations"))
        .stderr(contains("Include patterns are empty"));
}

#[test]
fn sync_dry_run_does_not_create_synchi_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let root_a = dir.path().join("root_a");
    let root_b = dir.path().join("root_b");
    fs::create_dir_all(&root_a).unwrap();
    fs::create_dir_all(&root_b).unwrap();

    fs::write(root_a.join("file_a.txt"), "data").unwrap();
    fs::write(root_b.join("file_b.txt"), "data").unwrap();

    let config_path = dir.path().join("config.toml");
    fs::write(
        &config_path,
        "root_a = \"./root_a\"\nroot_b = \"./root_b\"\n",
    )
    .unwrap();

    let mut cmd = cargo_bin_cmd!("synchi");
    cmd.current_dir(dir.path())
        .arg("--config")
        .arg(&config_path)
        .arg("sync")
        .arg("--dry-run")
        .assert()
        .success();

    assert!(!root_a.join(".synchi").exists());
    assert!(!root_b.join(".synchi").exists());
}
