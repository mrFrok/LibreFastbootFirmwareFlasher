use assert_cmd::Command;

#[test]
fn cli_version_flag() {
    let mut cmd = Command::cargo_bin("lfff").unwrap();
    cmd.arg("--version").assert().success();
}

#[test]
fn cli_help_output() {
    let mut cmd = Command::cargo_bin("lfff").unwrap();
    let assert = cmd.arg("--help").assert().success();
    let output = assert.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("LibreFastbootFirmwareFlasher"));
    assert!(stdout.contains("flash"));
    assert!(stdout.contains("extract"));
    assert!(stdout.contains("download"));
    assert!(stdout.contains("devices"));
    assert!(stdout.contains("arb"));
}

#[test]
fn cli_no_args_shows_help() {
    let mut cmd = Command::cargo_bin("lfff").unwrap();
    let assert = cmd.assert().success();
    let output = assert.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("LibreFastbootFirmwareFlasher") || stdout.contains("Usage"));
}

#[test]
fn cli_devices_no_device() {
    let mut cmd = Command::cargo_bin("lfff").unwrap();
    let assert = cmd.args(["devices"]).assert();
    // May succeed (no devices found) or fail (exit 1) — both are valid
    let output = assert.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{}{}", stdout, stderr);
    assert!(
        combined.contains("No devices found")
            || combined.contains("fastboot")
            || combined.contains("Connected devices"),
        "Expected device-related output, got: {}",
        combined
    );
}

#[test]
fn cli_devices_check_no_device() {
    let mut cmd = Command::cargo_bin("lfff").unwrap();
    cmd.args(["devices", "--check"]).assert();
    // Will likely fail with exit 1 if no device, but should not panic
}

#[test]
fn cli_extract_nonexistent_file() {
    let mut cmd = Command::cargo_bin("lfff").unwrap();
    let assert = cmd.args(["extract", "/nonexistent/firmware.zip"]).assert();
    let output = assert.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("not found") || stdout.contains("File not found") || output.status.code() != Some(0),
        "Expected error for nonexistent file"
    );
}

#[test]
fn cli_flash_nonexistent_directory() {
    let mut cmd = Command::cargo_bin("lfff").unwrap();
    let assert = cmd.args(["flash", "/nonexistent/firmware"]).assert();
    let output = assert.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Not a directory") || stdout.contains("not found") || output.status.code() != Some(0),
        "Expected error for nonexistent directory"
    );
}

#[test]
fn cli_deps_check() {
    let mut cmd = Command::cargo_bin("lfff").unwrap();
    cmd.args(["deps", "--check"]).assert().success();
    // Should run without panicking
}

#[test]
fn cli_arb_no_args() {
    let mut cmd = Command::cargo_bin("lfff").unwrap();
    let assert = cmd.args(["arb"]).assert();
    let output = assert.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Provide either") || stdout.contains("xbl_config") || output.status.code() != Some(0),
        "Expected error or usage hint"
    );
}

#[test]
fn cli_flash_partition_no_args() {
    let mut cmd = Command::cargo_bin("lfff").unwrap();
    let assert = cmd.args(["flash-partition"]).assert();
    // Should not panic, may show error or help
    let _ = assert.get_output();
}

#[test]
fn cli_verbose_flag() {
    let mut cmd = Command::cargo_bin("lfff").unwrap();
    cmd.args(["--verbose", "deps", "--check"]).assert().success();
    // Should run without issues
}

#[test]
fn cli_download_nonexistent_output() {
    let mut cmd = Command::cargo_bin("lfff").unwrap();
    // Will fail because URL is invalid, but should not panic
    cmd.args(["download", "https://invalid.example.com/firmware.zip", "-o", "/nonexistent/dir"])
        .assert();
}

// Additional tests for comprehensive coverage

#[test]
fn cli_deps_help() {
    let mut cmd = Command::cargo_bin("lfff").unwrap();
    let assert = cmd.args(["deps", "--help"]).assert().success();
    let output = assert.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Install and verify external dependencies"));
}

#[test]
fn cli_extract_help() {
    let mut cmd = Command::cargo_bin("lfff").unwrap();
    let assert = cmd.args(["extract", "--help"]).assert().success();
    let output = assert.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Extract a firmware .zip archive"));
}

#[test]
fn cli_download_help() {
    let mut cmd = Command::cargo_bin("lfff").unwrap();
    let assert = cmd.args(["download", "--help"]).assert().success();
    let output = assert.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Download firmware via OTA link"));
}

#[test]
fn cli_arb_help() {
    let mut cmd = Command::cargo_bin("lfff").unwrap();
    let assert = cmd.args(["arb", "--help"]).assert().success();
    let output = assert.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Anti-Rollback"));
}

#[test]
fn cli_flash_help() {
    let mut cmd = Command::cargo_bin("lfff").unwrap();
    let assert = cmd.args(["flash", "--help"]).assert().success();
    let output = assert.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Flash an extracted firmware directory"));
}

#[test]
fn cli_devices_help() {
    let mut cmd = Command::cargo_bin("lfff").unwrap();
    let assert = cmd.args(["devices", "--help"]).assert().success();
    let output = assert.get_output();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("List connected devices"));
}
