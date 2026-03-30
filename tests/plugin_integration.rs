use std::path::PathBuf;
use std::process::Command;

fn asobi_bin() -> PathBuf {
    let mut path = PathBuf::from(env!("CARGO_BIN_EXE_asobi"));
    if !path.exists() {
        path = PathBuf::from("target/debug/asobi");
    }
    path
}

fn plugin_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("plugins/{name}.wasm"))
}

fn has_plugins() -> bool {
    plugin_path("edit_file").exists() && plugin_path("patch_file").exists()
}

fn clean_workdir() -> PathBuf {
    let dir = std::env::temp_dir().join("asobi_integ_workdir");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).ok();
    dir
}

#[test]
fn test_tool_loading_via_cli() {
    if !has_plugins() {
        eprintln!("skipping: plugin wasm not found");
        return;
    }

    let workdir = clean_workdir();
    let output = Command::new(asobi_bin())
        .current_dir(&workdir)
        .args([
            "--tool",
            &format!("edit_file:{}", plugin_path("edit_file").display()),
            "--tool",
            &format!("patch_file:{}", plugin_path("patch_file").display()),
            "--prompt",
            "/tools",
        ])
        .env("ASOBI_MODEL", "test")
        .output()
        .expect("failed to run asobi");

    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("stderr: {stderr}");
    assert!(
        stderr.contains("[plugin] loaded: edit_file"),
        "edit_file should be loaded"
    );
    assert!(
        stderr.contains("[plugin] loaded: patch_file"),
        "patch_file should be loaded"
    );
}

#[test]
fn test_edit_file_via_non_interactive() {
    if !has_plugins() {
        eprintln!("skipping: plugin wasm not found");
        return;
    }

    let workdir = clean_workdir();
    let edit_path = plugin_path("edit_file");

    let output = Command::new(asobi_bin())
        .current_dir(&workdir)
        .args([
            "--tool",
            &format!("edit_file:{}", edit_path.display()),
            "--prompt",
            "Use the edit_file tool to replace 'foo' with 'bar' in /tmp/asobi_test_target.txt",
        ])
        .env("AWS_REGION", "us-west-2")
        .output()
        .expect("failed to run asobi");

    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("stderr: {stderr}");
    assert!(stderr.contains("[plugin] loaded: edit_file"));
}

#[test]
fn test_invalid_tool_spec_ignored() {
    let workdir = clean_workdir();
    let output = Command::new(asobi_bin())
        .current_dir(&workdir)
        .args(["--tool", "badformat", "--prompt", "hello"])
        .env("ASOBI_MODEL", "test")
        .output()
        .expect("failed to run asobi");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid --tool format"));
}

#[test]
fn test_config_error_exits() {
    let workdir = std::env::temp_dir().join("asobi_integ_bad_config");
    let _ = std::fs::remove_dir_all(&workdir);
    std::fs::create_dir_all(workdir.join(".asobi")).ok();
    std::fs::write(
        workdir.join(".asobi/config.toml"),
        "permissions = \"invalid\"",
    )
    .ok();

    let output = Command::new(asobi_bin())
        .current_dir(&workdir)
        .args(["--prompt", "hello"])
        .output()
        .expect("failed to run asobi");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("config error"));

    let _ = std::fs::remove_dir_all(&workdir);
}
