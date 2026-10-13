use crate::event::Amount;
use std::{error::Error, fmt};

/// Accounting failures, before adding the event's input location and vault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountingError {
    Overflow,
    InsufficientBalance { source: &'static str, available: Amount, required: Amount },
    DuplicateVaultCreation,
    MissingVaultCreation,
}

impl fmt::Display for AccountingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Overflow => write!(f, "amount overflow"),
            Self::InsufficientBalance { source, available, required } => {
                write!(f, "insufficient {source}: have {available}, need {required}")
            }
            Self::DuplicateVaultCreation => write!(f, "duplicate vault creation"),
            Self::MissingVaultCreation => {
                write!(f, "incomplete history: event references a vault without VaultCreated")
            }
        }
    }
}

impl Error for AccountingError {}

/// Failures encountered while parsing, resolving chain history, or replaying it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconstructionError {
    InvalidDelivery {
        line: usize,
        message: String,
    },
    ConflictingEvent {
        block: u64,
        log_index: u64,
        first_line: usize,
        incoming_line: usize,
    },
    InvalidVaultHistory {
        line: usize,
        vault: String,
        position: (u64, u64),
        source: AccountingError,
    },
}

impl fmt::Display for ReconstructionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDelivery { line, message } => {
                write!(f, "line {line}: invalid delivery: {message}")
            }
            Self::ConflictingEvent { block, log_index, first_line, incoming_line } => {
                write!(
                    f,
                    "Conflicting event at block {block}, log_index {log_index}: \
                    delivery on line {incoming_line} differs from the event first received on line {first_line}. \
                    No intervening reorg removed the stored event."
                )
            }
            Self::InvalidVaultHistory { line, vault, position: (block, log_index), source } => {
                write!(
                    f,
                    "line {line}, vault {vault}, block {block}, log_index {log_index}: {source}"
                )
            }
        }
    }
}

impl Error for ReconstructionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidVaultHistory { source, .. } => Some(source),
            _ => None,
        }
    }
}
