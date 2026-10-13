use serde::Deserialize;

/// Amounts are bounded to u64 smallest units; larger input is rejected.
pub type Amount = u64;
pub type EventKey = (u64, u64);

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    Event(Event),
    Reorg(Reorg),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reorg {
    pub from_block: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Event {
    pub block: u64,
    pub log_index: u64,
    pub vault: String,
    #[serde(flatten)]
    pub kind: EventKind,
}

impl Event {
    pub fn key(&self) -> EventKey {
        (self.block, self.log_index)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", content = "data", deny_unknown_fields)]
pub enum EventKind {
    VaultCreated { decimals: u8 },
    Deposit { user: String, assets: Amount, shares: Amount },
    Withdraw { user: String, assets: Amount, shares: Amount },
    Accrue { assets: Amount },
    SetCap { strategy: String, cap: Amount },
    Allocate { strategy: String, assets: Amount },
    Deallocate { strategy: String, assets: Amount },
}
