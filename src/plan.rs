//! Pure planning over a reconstructed snapshot; nothing here submits transactions.
use crate::{event::Amount, policy::Policy, state::VaultState};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};

/// Transfers always pass through idle, matching the chain's supported operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Move {
    Deallocate { strategy: String, assets: Amount },
    Allocate { strategy: String, assets: Amount },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// Snapshot balance before any proposed moves.
    pub current_balance: Amount,
    /// Original policy allocation; zero for strategies omitted from the policy.
    pub basis_points: u16,
    pub requested_amount: Amount,
    pub feasible_amount: Amount,
    pub cap: Amount,
}

/// Fields are private so callers cannot alter a validated plan or its projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultPlan {
    total_assets: Amount,
    moves: Vec<Move>,
    projected: VaultState,
    targets: BTreeMap<String, Target>,
}
impl VaultPlan {
    /// Original total assets, also the validated projected total.
    pub fn total_assets(&self) -> Amount {
        self.total_assets
    }
    /// Ordered transfers validated against successive simulated balances.
    pub fn moves(&self) -> &[Move] {
        &self.moves
    }
    /// Expected balances after all transfers, without changing the input snapshot.
    pub fn projected(&self) -> &VaultState {
        &self.projected
    }
    /// Requested and cap-limited targets; their difference is an unmet allocation.
    pub fn targets(&self) -> &BTreeMap<String, Target> {
        &self.targets
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    Overflow,
    InsufficientAssets { source: String, available: Amount, required: Amount },
    CapExceeded { strategy: String },
    ConservationViolation,
    TargetMismatch { strategy: String },
}
impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Overflow => write!(f, "planning arithmetic overflow"),
            Self::InsufficientAssets { source, available, required } => {
                write!(f, "insufficient assets in {source}: have {available}, need {required}")
            }
            Self::CapExceeded { strategy } => write!(f, "strategy {strategy} exceeds its cap"),
            Self::ConservationViolation => write!(f, "plan does not conserve assets or shares"),
            Self::TargetMismatch { strategy } => {
                write!(f, "strategy {strategy} did not reach its feasible target")
            }
        }
    }
}
impl Error for PlanError {}

/// Reasons a vault has no usable plan; preserve validation failures as typed errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    MissingState,
    MissingPolicy,
    PlanningFailed(PlanError),
}
impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingState => write!(f, "no usable reconstructed state"),
            Self::MissingPolicy => write!(f, "no policy supplied"),
            Self::PlanningFailed(error) => write!(f, "{error}"),
        }
    }
}

#[derive(Debug, Default)]
pub struct PlanningResult {
    pub plans: BTreeMap<String, VaultPlan>,
    /// No plan is produced when policy/state is missing or validation fails.
    pub skipped: BTreeMap<String, SkipReason>,
}

/// Apply one proposed transfer to a simulation, checking available source funds,
/// destination arithmetic, destination cap on allocations, and asset conservation.
/// Calculate balances before assignment so failed checks cannot partially commit.
/// Existing excesses in other strategies are permitted during withdrawals; all
/// caps are checked again after the entire sequence. Holders/shares never change.
fn apply_move(state: &mut VaultState, movement: &Move) -> Result<(), PlanError> {
    let total_before = state.total_assets().ok_or(PlanError::Overflow)?;
    match movement {
        Move::Allocate { strategy, assets } => {
            let destination = state.strategies.get(strategy).cloned().unwrap_or_default();
            let idle_after =
                state.idle_assets.checked_sub(*assets).ok_or(PlanError::InsufficientAssets {
                    source: "idle".into(),
                    available: state.idle_assets,
                    required: *assets,
                })?;
            let strategy_after =
                destination.assets.checked_add(*assets).ok_or(PlanError::Overflow)?;
            if strategy_after > destination.cap {
                return Err(PlanError::CapExceeded { strategy: strategy.clone() });
            }
            state.idle_assets = idle_after;
            state.strategies.entry(strategy.clone()).or_default().assets = strategy_after;
        }
        Move::Deallocate { strategy, assets } => {
            let available = match state.strategies.get(strategy) {
                Some(balance) => balance.assets,
                None => 0,
            };
            let strategy_after =
                available.checked_sub(*assets).ok_or(PlanError::InsufficientAssets {
                    source: strategy.clone(),
                    available,
                    required: *assets,
                })?;
            let idle_after = state.idle_assets.checked_add(*assets).ok_or(PlanError::Overflow)?;
            state.idle_assets = idle_after;
            state.strategies.entry(strategy.clone()).or_default().assets = strategy_after;
        }
    }
    if state.total_assets() != Some(total_before) {
        return Err(PlanError::ConservationViolation);
    }
    Ok(())
}

/// Calculate targets for the union of on-chain and policy strategies. Missing
/// policy entries target zero; missing caps allow zero. Integer division rounds
/// down to smallest asset units, with u128 multiplication to prevent overflow.
fn calculate_targets(
    state: &VaultState,
    allocations: &BTreeMap<String, u16>,
    total_assets: Amount,
) -> BTreeMap<String, Target> {
    let mut strategy_names = BTreeSet::new();
    strategy_names.extend(state.strategies.keys());
    strategy_names.extend(allocations.keys());
    let mut targets = BTreeMap::new();
    for strategy in strategy_names {
        let basis_points = allocations.get(strategy).copied().unwrap_or(0);
        let requested_amount =
            (u128::from(total_assets) * u128::from(basis_points) / 10000) as Amount;
        let (current_balance, cap) = match state.strategies.get(strategy) {
            Some(balance) => (balance.assets, balance.cap),
            None => (0, 0),
        };
        targets.insert(
            strategy.clone(),
            Target {
                current_balance,
                basis_points,
                requested_amount,
                feasible_amount: requested_amount.min(cap),
                cap,
            },
        );
    }
    targets
}

/// Release every surplus to idle before any allocations. A move is recorded only
/// after simulation succeeds. Name ordering makes the sequence reproducible.
fn withdraw_surpluses(plan: &mut VaultPlan) -> Result<(), PlanError> {
    for (strategy, target) in &plan.targets {
        if target.current_balance > target.feasible_amount {
            let movement = Move::Deallocate {
                strategy: strategy.clone(),
                assets: target.current_balance - target.feasible_amount,
            };
            apply_move(&mut plan.projected, &movement)?;
            plan.moves.push(movement);
        }
    }
    Ok(())
}

/// Fund deficits from the idle balance after withdrawals. Capped shortfalls are
/// not redistributed; unassigned percentages and rounding also remain idle.
fn fund_deficits(plan: &mut VaultPlan) -> Result<(), PlanError> {
    for (strategy, target) in &plan.targets {
        let current_balance = match plan.projected.strategies.get(strategy) {
            Some(balance) => balance.assets,
            None => 0,
        };
        if current_balance < target.feasible_amount {
            let movement = Move::Allocate {
                strategy: strategy.clone(),
                assets: target.feasible_amount - current_balance,
            };
            apply_move(&mut plan.projected, &movement)?;
            plan.moves.push(movement);
        }
    }
    Ok(())
}

/// Reject a plan unless it conserves assets and ownership, respects all caps,
/// and reaches every feasible target. The caller discards any failed plan.
fn validate_final_state(original: &VaultState, plan: &VaultPlan) -> Result<(), PlanError> {
    if plan.projected.total_assets() != Some(plan.total_assets)
        || plan.projected.holders != original.holders
        || plan.projected.total_shares != original.total_shares
    {
        return Err(PlanError::ConservationViolation);
    }
    for (strategy, target) in &plan.targets {
        let final_balance = match plan.projected.strategies.get(strategy) {
            Some(balance) => balance.assets,
            None => 0,
        };
        if final_balance > target.cap {
            return Err(PlanError::CapExceeded { strategy: strategy.clone() });
        }
        if final_balance != target.feasible_amount {
            return Err(PlanError::TargetMismatch { strategy: strategy.clone() });
        }
    }
    Ok(())
}

/// Plan against a copy: the original state never changes. Full strategy liquidity,
/// no fees, and an unchanged execution snapshot are assumed.
///
/// Example: with 403,000 total tokens, Morpho's 40% target is 161,200. Starting
/// from 240,000, withdraw 78,800 to idle before funding another strategy.
fn plan_vault(
    state: &VaultState,
    allocations: &BTreeMap<String, u16>,
) -> Result<VaultPlan, PlanError> {
    let total_assets = state.total_assets().ok_or(PlanError::Overflow)?;
    let mut plan = VaultPlan {
        total_assets,
        targets: calculate_targets(state, allocations, total_assets),
        projected: state.clone(),
        moves: Vec::new(),
    };
    withdraw_surpluses(&mut plan)?;
    fund_deficits(&mut plan)?;
    validate_final_state(state, &plan)?;
    Ok(plan)
}

/// Plan only vaults having both validated policy and usable reconstructed state.
/// Missing policy does not implicitly liquidate a vault; it produces a visible
/// skip. Policy for an unknown/excluded vault also produces a skip. An explicit
/// empty policy, by contrast, deliberately moves all strategy funds to idle.
/// One vault's planning failure does not suppress valid plans for other vaults.
pub fn build(states: &BTreeMap<String, VaultState>, policy: &Policy) -> PlanningResult {
    let mut result = PlanningResult::default();
    let mut vault_names = BTreeSet::new();
    vault_names.extend(states.keys());
    vault_names.extend(policy.vaults().keys());
    for vault in vault_names {
        match (states.get(vault), policy.vaults().get(vault)) {
            (Some(state), Some(allocations)) => match plan_vault(state, allocations) {
                Ok(plan) => {
                    result.plans.insert(vault.clone(), plan);
                }
                Err(error) => {
                    result.skipped.insert(vault.clone(), SkipReason::PlanningFailed(error));
                }
            },
            (None, _) => {
                result.skipped.insert(vault.clone(), SkipReason::MissingState);
            }
            (_, None) => {
                result.skipped.insert(vault.clone(), SkipReason::MissingPolicy);
            }
        }
    }
    result
}
