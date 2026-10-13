use curator_manager::{
    error::{AccountingError, ReconstructionError},
    reconstruct,
};
use serde_json::{Value, json};

fn event(block: u64, kind: &str, data: Value) -> Value {
    json!({"event": {"block": block, "log_index": 0, "vault": "v", "kind": kind, "data": data}})
}
fn create() -> Value {
    event(1, "VaultCreated", json!({"decimals": 6}))
}
fn deposit(block: u64, assets: u64) -> Value {
    event(block, "Deposit", json!({"user": "alice", "assets": assets, "shares": assets}))
}
fn feed(events: &[Value]) -> String {
    events.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")
}

#[test]
fn duplicates_order_and_reruns() {
    let ordered = reconstruct(&feed(&[create(), deposit(2, 100)])).unwrap();
    let input = feed(&[deposit(2, 100), create(), deposit(2, 100)]);
    let actual = reconstruct(&input).unwrap();
    assert_eq!(ordered.vaults, actual.vaults);
    assert_eq!(actual.stats.duplicates, 1);
    assert_eq!(actual, reconstruct(&input).unwrap());
}

#[test]
fn reorg_removes_boundary_and_later_events() {
    let input = feed(&[
        create(),
        deposit(2, 100),
        deposit(3, 200),
        json!({"reorg": {"from_block": 2}}),
        deposit(2, 50),
    ]);
    let result = reconstruct(&input).unwrap();
    assert_eq!(result.vaults["v"].idle_assets, 50);
    assert_eq!(result.stats.removed_events, 2);
    assert_eq!(result.stats.reorgs, 1);
}

#[test]
fn conflicting_duplicate_is_fatal() {
    let error = reconstruct(&feed(&[create(), deposit(2, 100), deposit(2, 50)])).unwrap_err();
    assert_eq!(
        error,
        ReconstructionError::ConflictingEvent {
            block: 2,
            log_index: 0,
            first_line: 2,
            incoming_line: 3,
        }
    );
    assert_eq!(
        error.to_string(),
        "Conflicting event at block 2, log_index 0: delivery on line 3 differs from the event first received on line 2. No intervening reorg removed the stored event."
    );
}

#[test]
fn accounting_and_cap_reduction() {
    let input = feed(&[
        create(),
        deposit(2, 100),
        event(3, "Accrue", json!({"assets": 20})),
        event(4, "SetCap", json!({"strategy": "s", "cap": 100})),
        event(5, "Allocate", json!({"strategy": "s", "assets": 80})),
        event(6, "SetCap", json!({"strategy": "s", "cap": 50})),
        event(7, "Deallocate", json!({"strategy": "s", "assets": 10})),
        event(8, "Withdraw", json!({"user": "alice", "assets": 12, "shares": 10})),
    ]);
    let result = reconstruct(&input).unwrap();
    assert!(result.diagnostics.is_empty());
    let state = &result.vaults["v"];
    assert_eq!(state.total_assets(), Some(108));
    assert_eq!(state.idle_assets, 38);
    assert_eq!(state.total_shares, 90);
    assert_eq!(state.holders["alice"], 90);
    assert_eq!(state.strategies["s"].assets, 70);
    assert_eq!(state.strategies["s"].cap, 50);
}

#[test]
fn invalid_accounting_quarantines_vault() {
    for bad in [
        event(3, "Withdraw", json!({"user": "alice", "assets": 101, "shares": 1})),
        event(3, "Withdraw", json!({"user": "alice", "assets": 1, "shares": 101})),
        event(3, "Allocate", json!({"strategy": "s", "assets": 101})),
        event(3, "Deallocate", json!({"strategy": "s", "assets": 1})),
        event(3, "VaultCreated", json!({"decimals": 6})),
    ] {
        let result = reconstruct(&feed(&[create(), deposit(2, 100), bad])).unwrap();
        assert!(result.vaults.is_empty());
        assert_eq!(result.diagnostics.len(), 1);
    }
}

#[test]
fn malformed_input_is_not_dropped() {
    for input in ["garbage", "{}", "{\"reorg\":{\"from_block\":-1}}"] {
        assert!(matches!(
            reconstruct(input).unwrap_err(),
            ReconstructionError::InvalidDelivery { line: 1, .. }
        ));
    }
}

#[test]
fn fixture_reconstructs_valid_vaults_and_surfaces_missing_history() {
    let result = reconstruct(include_str!("../data/events.jsonl")).unwrap();
    assert_eq!(result.vaults.len(), 2);
    assert_eq!(result.diagnostics.len(), 1);
    assert!(
        matches!(&result.diagnostics[0], ReconstructionError::InvalidVaultHistory { vault, source: AccountingError::MissingVaultCreation, .. } if vault == "vault-legacy")
    );
    let core = &result.vaults["vault-core"];
    assert_eq!(core.total_assets(), Some(184_000_000_000));
    assert_eq!(core.holders["grace"], 49_260_277_559);
    assert_eq!(core.total_shares, 179_307_410_316);
    assert_eq!(result.vaults["vault-yield"].total_assets(), Some(403_000_000_000));
}

#[test]
fn earnings_before_shares_and_missing_cap_are_preserved() {
    let result = reconstruct(&feed(&[
        create(),
        event(2, "Accrue", json!({"assets": 10})),
        event(3, "Allocate", json!({"strategy": "s", "assets": 5})),
    ]))
    .unwrap();
    assert_eq!(result.vaults["v"].total_shares, 0);
    assert_eq!(result.vaults["v"].total_assets(), Some(10));
    assert_eq!(result.vaults["v"].strategies["s"].cap, 0);
}

#[test]
fn overflow_is_reported_without_wrapping() {
    let result = reconstruct(&feed(&[
        create(),
        deposit(2, u64::MAX),
        event(3, "Accrue", json!({"assets": 1})),
    ]))
    .unwrap();
    assert!(result.vaults.is_empty());
    assert!(matches!(
        &result.diagnostics[0],
        ReconstructionError::InvalidVaultHistory { source: AccountingError::Overflow, .. }
    ));
}

#[test]
fn reorg_can_remove_invalid_accounting_before_replay() {
    let result = reconstruct(&feed(&[
        create(),
        event(2, "Withdraw", json!({"user": "alice", "assets": 100, "shares": 100})),
        json!({"reorg": {"from_block": 2}}),
    ]))
    .unwrap();
    assert!(result.diagnostics.is_empty());
    assert_eq!(result.vaults["v"].total_assets(), Some(0));
}

#[test]
fn insufficient_balance_preserves_details_and_error_source() {
    use std::error::Error;
    let result = reconstruct(&feed(&[
        create(),
        deposit(2, 100),
        event(3, "Allocate", json!({"strategy": "s", "assets": 101})),
    ]))
    .unwrap();
    let expected = AccountingError::InsufficientBalance {
        source: "idle assets",
        available: 100,
        required: 101,
    };
    let error = &result.diagnostics[0];
    assert_eq!(
        error,
        &ReconstructionError::InvalidVaultHistory {
            line: 3,
            vault: "v".into(),
            position: (3, 0),
            source: expected.clone(),
        }
    );
    assert_eq!(error.source().unwrap().downcast_ref::<AccountingError>(), Some(&expected));
    assert_eq!(
        error.to_string(),
        "line 3, vault v, block 3, log_index 0: insufficient idle assets: have 100, need 101"
    );
}

#[test]
fn duplicate_creation_has_typed_error() {
    let result =
        reconstruct(&feed(&[create(), event(2, "VaultCreated", json!({"decimals": 6}))])).unwrap();
    assert!(matches!(
        &result.diagnostics[0],
        ReconstructionError::InvalidVaultHistory {
            source: AccountingError::DuplicateVaultCreation,
            ..
        }
    ));
}
