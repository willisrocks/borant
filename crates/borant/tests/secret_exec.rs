//! The `borant secret-exec` shim hands one secret directory to one program.
//! The shim execs the program, so each test runs the real binary.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

fn shim(dir: &Path, argv: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_borant"))
        .arg("secret-exec")
        .arg(dir)
        .arg("--")
        .args(argv)
        .output()
        .unwrap()
}

/// A secret directory with an `env` file, and `pre.json` if given.
fn fixture(env: &str, pre: Option<&str>) -> TempDir {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("env"), env).unwrap();
    if let Some(pre) = pre {
        fs::write(tmp.path().join("pre.json"), pre).unwrap();
    }
    tmp
}

#[test]
fn secret_exec_passes_the_env_and_runs_the_pre_commands() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let pre = format!(
        r#"[["/bin/sh","-c","printf %s \"$ALPHA\" > {dir}/marker"]]"#,
        dir = dir.display()
    );
    fs::write(dir.join("env"), "ALPHA=one\nBETA=two=three\n").unwrap();
    fs::write(dir.join("pre.json"), pre).unwrap();

    let out = shim(
        dir,
        &["/bin/sh", "-c", "printf '%s|%s' \"$ALPHA\" \"$BETA\""],
    );

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "one|two=three");
    assert_eq!(
        fs::read_to_string(dir.join("marker")).unwrap(),
        "one",
        "the pre command must run with the vars and {{dir}} replaced"
    );
    assert!(!dir.join("env").exists(), "the env file must be deleted");
}

#[test]
fn secret_exec_missing_env_file_fails_without_running_the_program() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let ran = dir.join("ran");

    let out = shim(dir, &["/bin/sh", "-c", &format!(": > {}", ran.display())]);

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("env"), "{stderr}");
    assert!(!ran.exists(), "the program must not run");
}

#[test]
fn secret_exec_failed_pre_command_stops_before_the_program() {
    let tmp = fixture(
        "TOKEN=sup3rs3cret\n",
        Some(r#"[["/bin/sh","-c","exit 3"]]"#),
    );
    let dir = tmp.path();
    let ran = dir.join("ran");

    let out = shim(dir, &["/bin/sh", "-c", &format!(": > {}", ran.display())]);

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("/bin/sh"), "{stderr}");
    assert!(!stderr.contains("sup3rs3cret"), "{stderr}");
    assert!(!ran.exists(), "the program must not run");
    assert!(!dir.join("env").exists(), "the env file must be deleted");
}

#[test]
fn secret_exec_without_pre_commands_keeps_the_arguments() {
    let tmp = fixture("TOKEN=sup3rs3cret\n", None);
    let dir = tmp.path();

    let out = shim(
        dir,
        &[
            "/bin/sh",
            "-c",
            "printf '%s\\n' \"$@\"; printf 'token=%s' \"$TOKEN\"",
            "sh",
            "alpha",
            "beta",
        ],
    );

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "alpha\nbeta\ntoken=sup3rs3cret"
    );
    assert!(!dir.join("env").exists(), "the env file must be deleted");
}

/// The value of a bundle wins over the same name in the environment of the
/// process that starts the shim: a laptop daemon exports its own token, and
/// the harness must still get the value of the launch.
#[test]
fn secret_exec_overrides_a_variable_of_its_own_environment() {
    let tmp = fixture("TOKEN=bundle-value-123456\n", None);
    let dir = tmp.path();

    let out = Command::new(env!("CARGO_BIN_EXE_borant"))
        .arg("secret-exec")
        .arg(dir)
        .arg("--")
        .args(["/bin/sh", "-c", "printf '%s' \"$TOKEN\""])
        .env("TOKEN", "daemon-value-123456")
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "bundle-value-123456");
}
