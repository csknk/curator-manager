//! Two passes: resolve chain history, then replay the surviving events in order.
use crate::{
    error::{AccountingError, ReconstructionError},
    event::{Delivery, Event, EventKey, EventKind},
    state::{self, VaultState},
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct FeedStats {
    pub deliveries: usize,
    pub duplicates: usize,
    pub reorgs: usize,
    pub removed_events: usize,
    pub canonical_events: usize,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reconstruction {
    pub vaults: BTreeMap<String, VaultState>,
    pub diagnostics: Vec<ReconstructionError>,
    pub stats: FeedStats,
}

/// Keep the original input line alongside an event for useful error messages.
struct LocatedEvent {
    line: usize,
    event: Event,
}

/// Read deliveries in file order, because reorg signals affect previously received
/// events. A BTreeMap both detects repeated (block, log_index) keys and orders the
/// surviving events for replay. Identical repeats are ignored; conflicting repeats
/// fail unless a reorg first removed the old event. A reorg removes its boundary
/// block and everything after it. Malformed input aborts rather than disappearing.
///
/// The feed has no block hashes: after a reorg, we must trust that subsequent
/// deliveries describe replacement history, not delayed abandoned events.
fn canonical_events(
    feed: &str,
) -> Result<(BTreeMap<EventKey, LocatedEvent>, FeedStats), ReconstructionError> {
    let mut events: BTreeMap<EventKey, LocatedEvent> = BTreeMap::new();
    let mut stats = FeedStats::default();
    for (index, text) in feed.lines().enumerate() {
        let line = index + 1;
        let delivery: Delivery = serde_json::from_str(text).map_err(|error| {
            ReconstructionError::InvalidDelivery { line, message: error.to_string() }
        })?;
        stats.deliveries += 1;
        match delivery {
            Delivery::Reorg(reorg) => {
                let removed = events.split_off(&(reorg.from_block, 0)).len();
                stats.reorgs += 1;
                stats.removed_events += removed;
                tracing::info!(line, from_block = reorg.from_block, removed, "Reorg applied");
            }
            Delivery::Event(event) => {
                if let Some(stored) = events.get(&event.key()) {
                    if stored.event != event {
                        return Err(ReconstructionError::ConflictingEvent {
                            block: event.block,
                            log_index: event.log_index,
                            first_line: stored.line,
                            incoming_line: line,
                        });
                    }
                    stats.duplicates += 1;
                    tracing::debug!(
                        line,
                        block = event.block,
                        log_index = event.log_index,
                        "Duplicate ignored"
                    );
                } else {
                    events.insert(event.key(), LocatedEvent { line, event });
                }
            }
        }
    }
    stats.canonical_events = events.len();
    Ok((events, stats))
}

/// Reconstruct complete vault histories from canonical events. Creation must come
/// first for each vault. Apply later events to a copy, committing only successful
/// updates: arithmetic failure can leave that copy partially modified.
///
/// The first accounting error excludes the entire affected vault and skips its
/// remaining events; other vaults continue. Callers must inspect diagnostics before
/// claiming the result is complete. Caps are observed history, not replay limits.
/// Logs describe accepted replay steps, not actual transactions or durable commits.
pub fn reconstruct(feed: &str) -> Result<Reconstruction, ReconstructionError> {
    let (events, stats) = canonical_events(feed)?;
    let mut result = Reconstruction { stats, ..Default::default() };
    let mut excluded = BTreeSet::new();
    for located in events.into_values() {
        let event = located.event;
        if excluded.contains(&event.vault) {
            continue;
        }
        let current_state = result.vaults.get(&event.vault);
        let updated_state = match current_state {
            Some(current) => {
                let mut candidate = current.clone();
                state::apply(&mut candidate, &event.kind).map(|()| candidate)
            }
            None => match event.kind {
                EventKind::VaultCreated { decimals } => Ok(VaultState::new(decimals)),
                _ => Err(AccountingError::MissingVaultCreation),
            },
        };
        match updated_state {
            Ok(updated) => {
                tracing::info!(vault = %event.vault, block = event.block, log_index = event.log_index,
                    line = located.line, event = ?event.kind,
                    idle_before = current_state.map_or(0, |s| s.idle_assets), idle_after = updated.idle_assets,
                    shares_before = current_state.map_or(0, |s| s.total_shares), shares_after = updated.total_shares,
                    "Event applied");
                result.vaults.insert(event.vault, updated);
            }
            Err(source) => {
                result.diagnostics.push(ReconstructionError::InvalidVaultHistory {
                    line: located.line,
                    vault: event.vault.clone(),
                    position: event.key(),
                    source,
                });
                result.vaults.remove(&event.vault);
                excluded.insert(event.vault);
            }
        }
    }
    Ok(result)
}
