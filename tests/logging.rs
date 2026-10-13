use serde_json::Value;
use std::process::Command;

#[test]
fn json_logs_and_report_are_separate_and_incomplete_exit_is_explicit() {
    let output = Command::new(env!("CARGO_BIN_EXE_curator-manager"))
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("LOG_FORMAT", "json")
        .env("RUST_LOG", "debug")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report = String::from_utf8(output.stdout).unwrap();
    assert!(report.contains("Total assets: 184000 tokens"));
    assert!(report.contains("Total assets: 403000 tokens"));
    assert!(report.contains("INCOMPLETE"));
    assert!(report.contains("Excluded vault vault-legacy"));
    assert!(report.contains("1. Move 78800 tokens from morpho to idle."));
    assert!(report.contains("Projected end state (if all moves are applied):"));
    assert!(report.contains("UNMET: 40300 tokens for ether-fi"));
    let logs: Vec<Value> = String::from_utf8(output.stderr)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(logs.iter().filter(|log| log["level"] == "ERROR").count(), 1);
    assert!(logs.iter().any(|log| log["level"] == "DEBUG"));
    assert!(logs.iter().any(|log| log["level"] == "WARN"));
    assert!(logs.iter().any(
        |log| log["fields"]["block"] == 5 && log["fields"]["idle_after"] == 150_500_000_000_u64
    ));
    assert!(!logs.iter().any(|log| log["fields"]["vault"] == "vault-legacy"
        && log["fields"]["message"] == "Event applied"));
}
