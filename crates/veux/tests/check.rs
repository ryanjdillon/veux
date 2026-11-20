use std::path::PathBuf;

use assert_cmd::Command;

fn workspace_file(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

#[test]
fn example_config_passes() {
    Command::cargo_bin("veux")
        .unwrap()
        .arg("check")
        .arg("--config")
        .arg(workspace_file("examples/veux.toml"))
        .assert()
        .success();
}

#[test]
fn invalid_config_fails() {
    let dir = std::env::temp_dir().join(format!("veux-check-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("veux.toml");
    std::fs::write(&path, "[devices.a]\nkind = \"xvf3800\"\nlink = \"alsa\"\n").unwrap();
    let output = Command::cargo_bin("veux")
        .unwrap()
        .args(["check", "--config"])
        .arg(&path)
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    assert!(
        stdout.contains("does not support the alsa link"),
        "{stdout}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
