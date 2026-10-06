use std::process::Command;

#[test]
fn operational_cli_validates_checkpoints_backup_and_compatibility() {
    // Loom's substituted queues can only execute inside a Loom model.
    // The actual queue algorithm is checked separately in hft-spsc.
    if hft_spsc::IS_LOOM_BUILD {
        return;
    }
    let root = std::env::temp_dir().join(format!(
        "hft-cli-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir(&root).expect("root");
    let bundle = root.join("checkpoint");
    let backup = root.join("backup");
    let config = bundle.join("config.v1");
    let binary = env!("CARGO_BIN_EXE_hft-cli");
    let run = |args: &[&std::ffi::OsStr]| {
        let output = Command::new(binary).args(args).output().expect("execute");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    run(&["checkpoint-demo".as_ref(), bundle.as_os_str()]);
    run(&["config-validate".as_ref(), config.as_os_str()]);
    let health = run(&["health".as_ref(), bundle.as_os_str(), config.as_os_str()]);
    assert!(String::from_utf8_lossy(&health.stdout).contains("next_sequence=2"));
    run(&[
        "restore-check".as_ref(),
        bundle.as_os_str(),
        config.as_os_str(),
    ]);
    run(&[
        "backup".as_ref(),
        bundle.as_os_str(),
        backup.as_os_str(),
        config.as_os_str(),
    ]);
    run(&[
        "compatibility-check".as_ref(),
        config.as_os_str(),
        backup.join("config.v1").as_os_str(),
    ]);
    assert!(
        !Command::new(binary)
            .args(["checkpoint-demo".as_ref(), bundle.as_os_str()])
            .output()
            .expect("refuse overwrite")
            .status
            .success()
    );
    std::fs::remove_dir_all(root).expect("cleanup");
}
