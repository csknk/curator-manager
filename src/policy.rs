//! Validated off-chain allocation preferences. One basis point is 1/10,000.
use std::{collections::BTreeMap, error::Error, fmt};

/// Private storage prevents callers from constructing an unvalidated policy.
#[derive(Debug)]
pub struct Policy(BTreeMap<String, BTreeMap<String, u16>>);

#[derive(Debug)]
pub enum PolicyError {
    InvalidJson(serde_json::Error),
    InvalidAllocation { vault: String, strategy: String, basis_points: u16 },
    ExcessTotal { vault: String },
    EmptyName,
}
impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyName => write!(f, "policy vault and strategy names must not be empty"),
            Self::InvalidJson(error) => write!(f, "invalid policy: {error}"),
            Self::InvalidAllocation { vault, strategy, basis_points } => write!(
                f,
                "vault {vault}, strategy {strategy}: {basis_points} basis points exceeds 10000"
            ),
            Self::ExcessTotal { vault } => {
                write!(f, "vault {vault}: total allocation exceeds 10000 basis points")
            }
        }
    }
}
impl Error for PolicyError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidJson(error) => Some(error),
            _ => None,
        }
    }
}

impl Policy {
    /// Parse the complete JSON policy and validate every allocation before planning.
    /// Each value must be an integer in 0..=10000 and each vault's sum must be at
    /// most 10000. Empty names and malformed JSON are rejected.
    /// Standard map deserialization keeps the last value for duplicate JSON keys.
    /// Unassigned percentages, capped allocations, and integer rounding stay idle.
    /// An empty per-vault policy deliberately targets all its strategies at zero.
    pub fn parse(input: &str) -> Result<Self, PolicyError> {
        let vaults: BTreeMap<String, BTreeMap<String, u16>> =
            serde_json::from_str(input).map_err(PolicyError::InvalidJson)?;
        for (vault, strategies) in &vaults {
            if vault.trim().is_empty() {
                return Err(PolicyError::EmptyName);
            }
            let mut total = 0_u32;
            for (strategy, basis_points) in strategies {
                if strategy.trim().is_empty() {
                    return Err(PolicyError::EmptyName);
                }
                if *basis_points > 10000 {
                    return Err(PolicyError::InvalidAllocation {
                        vault: vault.clone(),
                        strategy: strategy.clone(),
                        basis_points: *basis_points,
                    });
                }
                total += u32::from(*basis_points);
                if total > 10000 {
                    return Err(PolicyError::ExcessTotal { vault: vault.clone() });
                }
            }
        }
        Ok(Self(vaults))
    }

    /// Borrow validated allocations in deterministic vault/strategy name order.
    pub fn vaults(&self) -> &BTreeMap<String, BTreeMap<String, u16>> {
        &self.0
    }
}
