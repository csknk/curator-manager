use curator_manager::{
    plan::{self, Move, PlanError, SkipReason, VaultPlan},
    policy::Policy,
    reconstruct,
    state::{StrategyState, VaultState},
};
use std::collections::BTreeMap;

fn state(idle: u64, strategies: &[(&str, u64, u64)]) -> VaultState {
    let mut state = VaultState::new(0);
    state.idle_assets = idle;
    for (name, assets, cap) in strategies {
        state.strategies.insert(name.to_string(), StrategyState { assets: *assets, cap: *cap });
    }
    state
}

// Independent execution of the public moves checks that a consumer can follow
// the exact order without overdrafts, lost assets, or overfunding destinations.
fn verify(initial: &VaultState, plan: &VaultPlan) {
    let mut simulated = initial.clone();
    let total = initial.total_assets().unwrap();
    let mut allocating = false;
    for movement in plan.moves() {
        match movement {
            Move::Deallocate { strategy, assets } => {
                assert!(!allocating);
                assert!(*assets > 0);
                let source = simulated.strategies.get_mut(strategy).unwrap();
                source.assets = source.assets.checked_sub(*assets).unwrap();
                simulated.idle_assets = simulated.idle_assets.checked_add(*assets).unwrap();
            }
            Move::Allocate { strategy, assets } => {
                allocating = true;
                assert!(*assets > 0);
                simulated.idle_assets = simulated.idle_assets.checked_sub(*assets).unwrap();
                let destination = simulated.strategies.entry(strategy.clone()).or_default();
                destination.assets = destination.assets.checked_add(*assets).unwrap();
                assert!(destination.assets <= destination.cap);
            }
        }
        assert_eq!(simulated.total_assets(), Some(total));
    }
    assert_eq!(&simulated, plan.projected());
    assert!(simulated.strategies.values().all(|s| s.assets <= s.cap));
    assert_eq!(simulated.holders, initial.holders);
    assert_eq!(simulated.total_shares, initial.total_shares);
}

#[test]
fn fixture_has_expected_order_balances_and_shortfalls() {
    let reconstruction = reconstruct(include_str!("../data/events.jsonl")).unwrap();
    let policy = Policy::parse(include_str!("../data/policy.json")).unwrap();
    let result = plan::build(&reconstruction.vaults, &policy);
    assert!(result.skipped.is_empty());
    let core = &result.plans["vault-core"];
    assert_eq!(
        core.moves(),
        &[
            Move::Deallocate { strategy: "aave".into(), assets: 19_600_000_000 },
            Move::Allocate { strategy: "spark".into(), assets: 30_000_000_000 },
        ]
    );
    assert_eq!(core.projected().idle_assets, 43_600_000_000);
    assert_eq!(
        core.targets()["spark"].requested_amount - core.targets()["spark"].feasible_amount,
        16_000_000_000
    );
    let yield_plan = &result.plans["vault-yield"];
    assert_eq!(
        yield_plan.moves(),
        &[
            Move::Deallocate { strategy: "morpho".into(), assets: 78_800_000_000 },
            Move::Allocate { strategy: "aave".into(), assets: 121_500_000_000 },
        ]
    );
    assert_eq!(yield_plan.projected().idle_assets, 40_300_000_000);
    assert_eq!(yield_plan.targets()["ether-fi"].feasible_amount, 0);
    for (name, vault_plan) in &result.plans {
        verify(&reconstruction.vaults[name], vault_plan);
    }
    let output = curator_manager::report::render_plans(&result);
    assert!(output.contains("UNMET: 16000 tokens"));
    assert!(output.contains("UNMET: 40300 tokens"));
    assert!(output.contains("not executed"));
}

#[test]
fn rejects_bad_policy() {
    for input in [
        r#"{"v":{"s":10001}}"#,
        r#"{"v":{"s":6000,"t":5000}}"#,
        r#"{"v":{"s":-1}}"#,
        r#"{"v":{"s":1.5}}"#,
        r#"{"":{"s":1}}"#,
        r#"{"v":{"":1}}"#,
        "garbage",
    ] {
        assert!(Policy::parse(input).is_err(), "accepted {input}");
    }
}

#[test]
fn missing_policy_and_missing_state_are_explicit_skips() {
    let states = BTreeMap::from([("unconfigured".into(), state(10, &[]))]);
    let result = plan::build(&states, &Policy::parse(r#"{"unknown":{"s":5000}}"#).unwrap());
    assert!(result.plans.is_empty());
    assert_eq!(result.skipped.len(), 2);
    assert_eq!(result.skipped["unknown"], SkipReason::MissingState);
    assert_eq!(result.skipped["unconfigured"], SkipReason::MissingPolicy);
}

#[test]
fn empty_policy_withdraws_everything_including_uncapped_positions() {
    let initial = state(5, &[("s", 10, 0), ("t", 20, 100)]);
    let states = BTreeMap::from([("v".into(), initial.clone())]);
    let result = plan::build(&states, &Policy::parse(r#"{"v":{}}"#).unwrap());
    verify(&initial, &result.plans["v"]);
    assert_eq!(result.plans["v"].projected().idle_assets, 35);
}

#[test]
fn rounding_and_large_amounts_leave_correct_idle() {
    for total in [0, 1, 7, u64::MAX] {
        let initial = state(total, &[("s", 0, u64::MAX)]);
        let states = BTreeMap::from([("v".into(), initial.clone())]);
        let result = plan::build(&states, &Policy::parse(r#"{"v":{"s":3333}}"#).unwrap());
        let vault_plan = &result.plans["v"];
        let expected = (u128::from(total) * 3333 / 10000) as u64;
        assert_eq!(vault_plan.projected().idle_assets, total - expected);
        verify(&initial, vault_plan);
    }
}

#[test]
fn deterministic_safe_plans_and_replanning_is_noop() {
    let policy = Policy::parse(r#"{"v":{"a":4000,"b":6000}}"#).unwrap();
    for idle in [0, 1, 10, 100] {
        for balance in [0, 5, 50] {
            for cap in [0, 3, 100] {
                let initial = state(idle, &[("a", balance, cap), ("b", 4, 50), ("removed", 2, 0)]);
                let states = BTreeMap::from([("v".into(), initial.clone())]);
                let result = plan::build(&states, &policy);
                let vault_plan = &result.plans["v"];
                verify(&initial, vault_plan);
                assert_eq!(plan::build(&states, &policy).plans, result.plans);
                assert_eq!(states["v"], initial);
                let projected = BTreeMap::from([("v".into(), vault_plan.projected().clone())]);
                assert!(plan::build(&projected, &policy).plans["v"].moves().is_empty());
            }
        }
    }
}

#[test]
fn invalid_total_is_skipped_without_partial_plan() {
    let states = BTreeMap::from([("v".into(), state(u64::MAX, &[("s", 1, 1)]))]);
    let result = plan::build(&states, &Policy::parse(r#"{"v":{}}"#).unwrap());
    assert!(result.plans.is_empty());
    assert_eq!(result.skipped["v"], SkipReason::PlanningFailed(PlanError::Overflow));
}

#[test]
fn duplicate_policy_keys_use_last_value() {
    let policy = Policy::parse(r#"{"v":{"s":1000,"s":2500}}"#).unwrap();
    assert_eq!(policy.vaults()["v"]["s"], 2500);
    let policy = Policy::parse(r#"{"v":{"s":1000},"v":{"t":5000}}"#).unwrap();
    assert_eq!(policy.vaults()["v"], BTreeMap::from([("t".into(), 5000)]));
}
