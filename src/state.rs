use crate::{
    error::AccountingError,
    event::{Amount, EventKind},
};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StrategyState {
    pub assets: Amount,
    /// No on-chain cap means the effective cap is zero.
    pub cap: Amount,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultState {
    pub decimals: u8,
    pub idle_assets: Amount,
    pub total_shares: Amount,
    pub holders: BTreeMap<String, Amount>,
    pub strategies: BTreeMap<String, StrategyState>,
}

impl VaultState {
    pub fn new(decimals: u8) -> Self {
        Self {
            decimals,
            idle_assets: 0,
            total_shares: 0,
            holders: BTreeMap::new(),
            strategies: BTreeMap::new(),
        }
    }

    pub fn total_assets(&self) -> Option<Amount> {
        self.strategies
            .values()
            .try_fold(self.idle_assets, |total, strategy| total.checked_add(strategy.assets))
    }
}

fn add(value: Amount, amount: Amount) -> Result<Amount, AccountingError> {
    value.checked_add(amount).ok_or(AccountingError::Overflow)
}

fn subtract(
    value: Amount,
    amount: Amount,
    source: &'static str,
) -> Result<Amount, AccountingError> {
    value.checked_sub(amount).ok_or(AccountingError::InsufficientBalance {
        source,
        available: value,
        required: amount,
    })
}

/// Apply one canonical event to an existing vault's candidate state in place.
///
/// The caller must select the correct vault and supply events in blockchain
/// order after resolving duplicates and reorgs. Vault creation is handled by
/// `feed::reconstruct`; receiving `VaultCreated` here means the vault already exists.
///
/// Accounting rules:
/// - Deposits increase idle assets, total shares, and the depositor's shares.
/// - Withdrawals decrease idle assets, total shares, and the holder's shares.
///   Share amounts come directly from the event, including on-chain rounding;
///   this function does not calculate a share price or recalculate shares.
/// - Earnings increase idle assets without issuing shares, even when no shares
///   are outstanding yet.
/// - Allocations and deallocations transfer assets between idle and a strategy,
///   preserving total assets on success.
/// - Cap updates replace the recorded strategy limit. Newly encountered
///   strategies start with zero assets and a zero cap.
///
/// Caps are recorded, not enforced here: replay must preserve observed balances,
/// including positions above their caps, so the later planner can address them.
///
/// # Errors
///
/// Returns `DuplicateVaultCreation` for an existing vault's creation event,
/// `InsufficientBalance` when a debit exceeds its source balance, or `Overflow`
/// when an addition or the final total-assets calculation exceeds `Amount`.
/// The caller adds the vault identifier and event location to these errors.
///
/// # Mutation on failure
///
/// An error does not undo earlier mutations in this function. For example, a
/// deposit can update idle assets before its share addition overflows. `feed::reconstruct`
/// therefore passes a clone of the current state and commits it only on `Ok(())`;
/// a failed candidate must be discarded rather than used as valid vault state.
pub(crate) fn apply(state: &mut VaultState, kind: &EventKind) -> Result<(), AccountingError> {
    match kind {
        EventKind::VaultCreated { .. } => return Err(AccountingError::DuplicateVaultCreation),
        EventKind::Deposit { user, assets, shares } => {
            state.idle_assets = add(state.idle_assets, *assets)?;
            state.total_shares = add(state.total_shares, *shares)?;
            let balance = state.holders.entry(user.clone()).or_default();
            *balance = add(*balance, *shares)?;
        }
        EventKind::Withdraw { user, assets, shares } => {
            state.idle_assets = subtract(state.idle_assets, *assets, "idle assets")?;
            let balance = state.holders.entry(user.clone()).or_default();
            *balance = subtract(*balance, *shares, "holder shares")?;
            state.total_shares = subtract(state.total_shares, *shares, "total shares")?;
        }
        EventKind::Accrue { assets } => state.idle_assets = add(state.idle_assets, *assets)?,
        EventKind::SetCap { strategy, cap } => {
            state.strategies.entry(strategy.clone()).or_default().cap = *cap
        }
        EventKind::Allocate { strategy, assets } => {
            state.idle_assets = subtract(state.idle_assets, *assets, "idle assets")?;
            let target = state.strategies.entry(strategy.clone()).or_default();
            target.assets = add(target.assets, *assets)?;
        }
        EventKind::Deallocate { strategy, assets } => {
            let source = state.strategies.entry(strategy.clone()).or_default();
            source.assets = subtract(source.assets, *assets, "strategy assets")?;
            state.idle_assets = add(state.idle_assets, *assets)?;
        }
    }
    state.total_assets().ok_or(AccountingError::Overflow)?;
    Ok(())
}
