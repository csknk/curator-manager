//! Read-only presentation of reconstructed balances. Asset values are proportional
//! accounting values, not promises of immediately available withdrawals.
use crate::plan::{Move, PlanningResult, VaultPlan};
use crate::{event::Amount, feed::Reconstruction, state::VaultState};
use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportError {
    AssetsOverflow { vault: String },
    InconsistentShares { vault: String },
}

impl fmt::Display for ReportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AssetsOverflow { vault } => write!(f, "vault {vault}: total assets overflow"),
            Self::InconsistentShares { vault } => {
                write!(f, "vault {vault}: holder shares do not match total shares")
            }
        }
    }
}
impl Error for ReportError {}

/// Format smallest-unit assets without floats or powers of ten that could overflow.
/// Trailing fractional zeros and an unnecessary decimal point are omitted.
/// Padding supports the entire u8 decimals range supplied by the input schema.
pub fn format_assets(amount: Amount, decimals: u8) -> String {
    if decimals == 0 {
        return amount.to_string();
    }
    let mut digits = format!("{:0width$}", amount, width = usize::from(decimals) + 1);
    digits.insert(digits.len() - usize::from(decimals), '.');
    digits.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// Floor a holder's proportional asset value to the smallest asset unit.
/// Two u64 inputs multiply exactly in u128. A zero share supply has no valuation.
fn holder_value(
    total_vault_assets: Amount,
    holder_shares: Amount,
    total_shares_outstanding: Amount,
) -> Option<u128> {
    if total_shares_outstanding == 0 {
        return None;
    }
    Some(
        u128::from(total_vault_assets) * u128::from(holder_shares)
            / u128::from(total_shares_outstanding),
    )
}

fn vault_report(name: &str, state: &VaultState) -> Result<String, ReportError> {
    let total =
        state.total_assets().ok_or_else(|| ReportError::AssetsOverflow { vault: name.into() })?;
    let share_sum = state.holders.values().try_fold(0_u64, |sum, shares| sum.checked_add(*shares));
    if share_sum != Some(state.total_shares) {
        return Err(ReportError::InconsistentShares { vault: name.into() });
    }
    let assets = |amount| format_assets(amount, state.decimals);
    let mut text = format!(
        "\nVault: {name}\n  Asset decimals: {}\n  Total assets: {} tokens\n  Idle assets: {} tokens\n  Shares outstanding (raw units): {}\n  Strategies:\n",
        state.decimals,
        assets(total),
        assets(state.idle_assets),
        state.total_shares
    );
    if state.strategies.is_empty() {
        text.push_str("    None\n");
    }
    for (strategy, balance) in &state.strategies {
        text.push_str(&format!(
            "    {strategy}: {} tokens; effective cap: {} tokens",
            assets(balance.assets),
            assets(balance.cap)
        ));
        if balance.assets > balance.cap {
            text.push_str(&format!(
                "; ABOVE CAP by {} tokens",
                assets(balance.assets - balance.cap)
            ));
        }
        text.push('\n');
    }
    text.push_str("  Holders (calculated asset values, rounded down):\n");
    if state.holders.is_empty() {
        text.push_str("    None\n");
    }
    let mut valued_total = 0_u128;
    for (holder, shares) in &state.holders {
        let value = match holder_value(total, *shares, state.total_shares) {
            Some(value) => {
                valued_total += value;
                // Validated holder shares sum to supply, so each value <= total (u64).
                format!("{} tokens", assets(value as Amount))
            }
            None => "unavailable (no shares outstanding)".into(),
        };
        text.push_str(&format!(
            "    {holder}: {shares} shares (raw units); calculated value: {value}\n"
        ));
    }
    if state.total_shares == 0 {
        text.push_str("  Holder valuation: unavailable (no shares outstanding)\n  Valuation rounding remainder: unavailable\n");
    } else {
        // Sum of individually floored values cannot exceed total assets.
        let remainder = u128::from(total) - valued_total;
        text.push_str(&format!(
            "  Valuation rounding remainder: {} tokens ({remainder} smallest asset units)\n",
            assets(remainder as Amount)
        ));
    }
    Ok(text)
}

/// Render deterministic output without changing reconstructed state. Diagnostics
/// remain visible alongside valid vaults; callers retain responsibility for exit status.
/// Invalid manually constructed states return a report error instead of misleading values.
pub fn render(result: &Reconstruction) -> Result<String, ReportError> {
    let status = if result.diagnostics.is_empty() { "COMPLETE" } else { "INCOMPLETE" };
    let mut text = format!(
        "Vault state report — {status}\nAsset amounts are tokens; shares are raw units.\nHolder values are proportional ownership, not immediately withdrawable balances.\nAn unset strategy cap has an effective value of zero.\n"
    );
    if result.vaults.is_empty() {
        text.push_str("\nNo valid vaults to report.\n");
    }
    for (name, state) in &result.vaults {
        text.push_str(&vault_report(name, state)?);
    }
    if !result.diagnostics.is_empty() {
        text.push_str("\nExcluded vaults — incomplete or invalid history:\n");
        for diagnostic in &result.diagnostics {
            if let crate::error::ReconstructionError::InvalidVaultHistory { vault, .. } = diagnostic
            {
                text.push_str(&format!("  Excluded vault {vault} (see stderr logs for details)\n"));
            } else {
                text.push_str("  Reconstruction error (see stderr logs for details)\n");
            }
        }
    }
    Ok(text)
}

/// Render policy targets, ordered moves, and projected balances. This only formats
/// the validated plan; it never executes transfers or changes state.
pub fn render_plans(result: &PlanningResult) -> String {
    let mut text = String::from("\nProposed fund movements (not executed)\n");
    for (vault, plan) in &result.plans {
        render_starting_position(&mut text, vault, plan);
        render_moves(&mut text, plan);
        render_projected_state(&mut text, plan);
    }
    for (vault, reason) in &result.skipped {
        text.push_str(&format!("\nSkipped vault {vault}: {reason}\n"));
    }
    text
}

/// Explain current allocations and cap-limited targets before showing transfers.
fn render_starting_position(text: &mut String, vault: &str, plan: &VaultPlan) {
    let amount = |value| format_assets(value, plan.projected().decimals);
    text.push_str(&format!("\nVault: {vault}\n"));
    text.push_str(&format!("  Total assets: {} tokens\n", amount(plan.total_assets())));
    text.push_str("  Allocation targets (token amounts):\n");
    for (strategy, target) in plan.targets() {
        text.push_str(&format!(
            "    {strategy}: current {}; policy {}% = {}; cap {}; feasible target {}\n",
            amount(target.current_balance),
            format_assets(Amount::from(target.basis_points), 2),
            amount(target.requested_amount),
            amount(target.cap),
            amount(target.feasible_amount)
        ));
    }
}

/// Show proposed actions in their validated execution order.
fn render_moves(text: &mut String, plan: &VaultPlan) {
    let amount = |value| format_assets(value, plan.projected().decimals);
    text.push_str("  Proposed moves:\n");
    if plan.moves().is_empty() {
        text.push_str("  No moves required for feasible targets.\n");
    }
    for (index, movement) in plan.moves().iter().enumerate() {
        let (from, to, assets) = match movement {
            Move::Allocate { strategy, assets } => ("idle", strategy.as_str(), assets),
            Move::Deallocate { strategy, assets } => (strategy.as_str(), "idle", assets),
        };
        text.push_str(&format!(
            "  {}. Move {} tokens from {from} to {to}.\n",
            index + 1,
            amount(*assets)
        ));
    }
}

/// Show the simulated end state and policy shortfalls, including zero-cap targets.
fn render_projected_state(text: &mut String, plan: &VaultPlan) {
    let amount = |value| format_assets(value, plan.projected().decimals);
    text.push_str("  Projected end state (if all moves are applied):\n");
    text.push_str(&format!("    Total assets: {} tokens\n", amount(plan.total_assets())));
    text.push_str(&format!("    Idle assets: {} tokens\n", amount(plan.projected().idle_assets)));
    for (strategy, target) in plan.targets() {
        let projected_balance =
            plan.projected().strategies.get(strategy).map_or(0, |state| state.assets);
        text.push_str(&format!(
            "    {strategy}: projected {}; policy {}% = {}; cap {}; feasible target {}\n",
            amount(projected_balance),
            format_assets(Amount::from(target.basis_points), 2),
            amount(target.requested_amount),
            amount(target.cap),
            amount(target.feasible_amount)
        ));
    }
    text.push_str("    Shares and holder ownership unchanged.\n");
    for (strategy, target) in plan.targets() {
        if target.requested_amount > target.feasible_amount {
            text.push_str(&format!(
                    "    UNMET: {} tokens for {strategy} blocked by effective cap; funds remain idle.\n",
                    amount(target.requested_amount - target.feasible_amount)
                ));
        }
    }
}
