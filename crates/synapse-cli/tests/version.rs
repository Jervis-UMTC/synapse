use std::process::Command;

#[test]
fn version_flag_reports_package_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_synapse"))
        .arg("--version")
        .output()
        .expect("synapse binary should run");

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout)
            .expect("version output should be UTF-8")
            .trim(),
        format!("synapse {}", env!("CARGO_PKG_VERSION"))
    );
}
