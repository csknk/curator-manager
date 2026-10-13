use curator_manager::{
    feed::Reconstruction,
    reconstruct,
    report::{ReportError, format_assets, render},
    state::{StrategyState, VaultState},
};

fn sample(assets: u64, holders: &[(&str, u64)]) -> Reconstruction {
    let mut state = VaultState::new(0);
    state.idle_assets = assets;
    state.holders = holders.iter().map(|(name, shares)| (name.to_string(), *shares)).collect();
    state.total_shares = holders.iter().map(|(_, shares)| shares).sum();
    let mut result = Reconstruction::default();
    result.vaults.insert("test".into(), state);
    result
}

#[test]
fn exact_ownership_and_read_only_deterministic_rendering() {
    let result = sample(1000, &[("bob", 75), ("alice", 25)]);
    let before = format!("{result:?}");
    let text = render(&result).unwrap();
    assert!(text.contains("alice: 25 shares (raw units); calculated value: 250 tokens"));
    assert!(text.contains("bob: 75 shares (raw units); calculated value: 750 tokens"));
    assert!(text.find("alice:").unwrap() < text.find("bob:").unwrap());
    assert!(text.contains("Valuation rounding remainder: 0 tokens"));
    assert_eq!(text, render(&result).unwrap());
    assert_eq!(before, format!("{result:?}"));
}

#[test]
fn floors_fractional_values_and_explains_remainder() {
    let text = render(&sample(10, &[("alice", 1), ("bob", 2)])).unwrap();
    assert!(text.contains("calculated value: 3 tokens"));
    assert!(text.contains("calculated value: 6 tokens"));
    assert!(text.contains("Valuation rounding remainder: 1 tokens (1 smallest asset units)"));
}

#[test]
fn no_share_supply_has_no_valuation() {
    let text = render(&sample(10, &[("alice", 0)])).unwrap();
    assert!(text.contains("Total assets: 10 tokens"));
    assert!(text.contains("calculated value: unavailable (no shares outstanding)"));
    assert!(text.contains("Valuation rounding remainder: unavailable"));
}

#[test]
fn multiplication_uses_wide_intermediate() {
    let text = render(&sample(u64::MAX, &[("alice", u64::MAX)])).unwrap();
    assert!(text.contains(&format!("calculated value: {} tokens", u64::MAX)));
    assert!(text.contains("Valuation rounding remainder: 0 tokens"));
}

#[test]
fn formats_assets_exactly_across_decimal_range() {
    assert_eq!(format_assets(1_234_567, 6), "1.234567");
    assert_eq!(format_assets(1, 6), "0.000001");
    assert_eq!(format_assets(42, 0), "42");
    assert_eq!(format_assets(130_000_000_000, 6), "130000");
    assert_eq!(format_assets(1_230_000, 6), "1.23");
    assert_eq!(format_assets(0, 6), "0");
    assert_eq!(format_assets(1, 255), format!("0.{}1", "0".repeat(254)));
}

#[test]
fn reports_cap_excess_and_default_cap() {
    let mut result = sample(0, &[]);
    let state = result.vaults.get_mut("test").unwrap();
    state.strategies.insert("uncapped".into(), StrategyState { assets: 5, ..Default::default() });
    state.strategies.insert("limited".into(), StrategyState { assets: 70, cap: 50 });
    let text = render(&result).unwrap();
    assert!(text.contains("uncapped: 5 tokens; effective cap: 0 tokens; ABOVE CAP by 5 tokens"));
    assert!(text.contains("limited: 70 tokens; effective cap: 50 tokens; ABOVE CAP by 20 tokens"));
}

#[test]
fn fixture_reports_valid_vaults_and_excluded_history() {
    let text = render(&reconstruct(include_str!("../data/events.jsonl")).unwrap()).unwrap();
    assert!(text.contains("Vault state report — INCOMPLETE"));
    assert!(text.contains("Vault: vault-core"));
    assert!(text.contains("Total assets: 184000 tokens"));
    assert!(text.contains("Vault: vault-yield"));
    assert!(text.contains("Total assets: 403000 tokens"));
    assert!(text.contains("ABOVE CAP by 40000 tokens"));
    assert!(text.contains("Excluded vaults"));
    assert!(text.contains("vault vault-legacy"));
    assert!(!text.contains("Vault: vault-legacy"));
}

#[test]
fn malformed_state_is_not_reported_as_valid() {
    let mut result = sample(10, &[("alice", 1)]);
    result.vaults.get_mut("test").unwrap().total_shares = 0;
    assert!(matches!(render(&result), Err(ReportError::InconsistentShares { .. })));
    let state = result.vaults.get_mut("test").unwrap();
    state.idle_assets = u64::MAX;
    state.strategies.insert("s".into(), StrategyState { assets: 1, cap: 1 });
    assert!(matches!(render(&result), Err(ReportError::AssetsOverflow { .. })));
}
