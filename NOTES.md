# Design notes and next steps

## Current scope

The programme reads saved events, works out what each vault owns, and computes moves based on the curator's policy. It does not yet receive live events or move funds on-chain.

The prototype assumes that funds can be withdrawn from strategies immediately and without fees. Its number data types are suitable for the provided fixture (rather than actual Blockchain data). Tests cover event ordering, duplicates, reorgs, accounting, and safe plans.

There is no persistence, and state does not survive a restart.

Live ingestion, persistence, and on-chain execution were deliberately left out to keep this implementation focused on accounting correctness and safe planning from the supplied fixtures. Fees, withdrawal liquidity, and wider integer types would be addressed when integrating real contracts.

## Part 2: Handling live data and dozens of vaults

Before scaling the service up, there should be a clear picture of the purpose of the service. This might include:

- Giving curators a live view of their vault: total funds and how they are allocated
- Automatic rebalancing: implement plans when vault state changes e.g. a deposit increases idle funds, and the service might automatically allocate according to the defined plan
- Monitoring, policy comparison and rebalancing as a component part of a wider financial service managing allocations
- Providing an audit trail

For the purposes of this exercise, we'll assume that the intended use is:

> A continuously running service for vault operators, initially providing monitoring and proposed rebalancing, which could be expanded or incorporated to support automatic controlled rebalancing

Keep the existing accounting and planning code. Start with one application, a database, and a connection to the indexer's event queue. Add complexity only when there is a clear need.

The main challenges will be continual scanning of a stream of data, and accounting for possible reorgs.

### 1. Receive events continuously

Instead of reading a file once, the application would keep receiving events as they arrive. It must know when enough data has arrived to update balances safely - there is likely a balance between waiting for finality to reduce reorg exposure and unwanted delay.

For the purpose of this exercise, we'll assume the indexer delivers events through a durable message queue or replayable event log. This delivery system retains events so our service can recover after downtime. Our service connects to it and records which events it has safely received. If the indexer does not provide this capability, we would need to add it.

Our service might:

- Connect to the message queue and scan inputs in an infinite loop
- Ask the indexer for block identities and a signal that a block's events are complete, so out-of-order delivery does not produce incorrect balances
- Keep invalid events for investigation and pause affected processing rather than silently skipping missing or bad data
- Recognise duplicate deliveries without applying the same event twice
- Stored events should support rollback and be marked as provisional/finalised to support block finality & reorg management

Different vaults can be processed concurrently, but events within each vault must be applied in chain order.

### 2. Save progress and recover

The application should survive a restart without losing events or counting them twice. It must also be able to rebuild balances when a reorg replaces recent chain history. A database provides the durable record; memory holds only the state currently being worked on.

#### What to save

Keep the original events so balances can be rebuilt, alongside the latest calculated state so every restart does not require processing the entire history again.

- **Received events:** the event data, its chain and block identity (height, hash), and its position in the input stream. Block hashes distinguish competing blocks at the same height
- **Vault state:** balances, shares, strategy allocations, and caps, tied to the block hash through which they have been calculated
- **Processing progress:** which deliveries have been saved and which complete canonical blocks have been applied. These are different: receiving an event does not mean it has updated balances yet
- **Periodic snapshots:** saved copies of vault state at known blocks, used as starting points when rebuilding history

#### How to receive and apply an event safely

Save events before confirming receipt, so a restart cannot cause them to be lost.

1. Store each event using a unique chain, block-hash, and log-index key; ignore identical duplicates and flag conflicting data
2. Confirm receipt to the delivery system only after the save succeeds
3. When events are ready for processing, apply them in chain order
4. Save updated balances and processing progress together, so they cannot get out of step

After a restart, continue from the saved progress. If an event is delivered again, duplicate detection prevents it being counted twice.

A DB uniqueness constraint should be applied, e.g. an event key `(chain_id, block_hash, log_index)` to prevent duplicate storage.

#### Processing Saved Data

Incoming events are stored immediately, but we update the accounting state only from complete finalized blocks. We could provide a provisional view if freshness is important.

A background loop checks the latest finalized block reported by the Ethereum node. Before processing stored events up to that point, confirm that their block hashes belong to the finalized chain and that the indexer has delivered all relevant events. Apply those events in chain order and save the updated balances and processing position together. Block height alone is not enough: an abandoned block may have the same height as its replacement.

#### Handling reorgs

Because we only update accounting from finalized blocks, ordinary reorgs should affect stored provisional events rather than calculated balances.

- Mark replaced blocks and their events as no longer canonical, and accept their replacements
- If we later maintain provisional balances, rebuild those balances from a snapshot before the replaced blocks and discard affected plans
- If a change affects history we considered finalized, pause processing and investigate before rebuilding from a trusted starting point

#### What to test first

Recovery tests should deliberately stop the service at awkward moments: after saving an event but before acknowledging it, during a state update, and during a reorg rebuild. After restarting, the resulting balances should match a clean replay of the same canonical history, with no missing or duplicated funds.

### 3. Handle several vaults at once

Different vaults can be processed independently, but changes within one vault must stay in order.

- Give each vault one active worker at a time; let a small number of workers process different vaults concurrently
- Coordinate reorgs across all affected vaults
- Limit how much work is held in memory and slow intake when processing falls behind. Measure delays before adding more workers

### 4. Keep plans up to date

Balances, caps, and policies can change after a plan is created. A plan must be checked against the latest information before anyone acts on it.

- Record the state and policy version used to create each plan
- Rebuild stale plans and avoid generating tiny moves for insignificant changes
- Check actual withdrawal availability, fees, and contract-sized amounts. Continue reporting targets that cannot be met

### 5. Execute and check transfers

Printing a move is different from sending money. Execution needs its own checks and a record of what was submitted and what actually happened.

- Start with reviewed plans and test-network execution before automating real funds
- Simulate transactions, protect signing keys, and enforce caps when moves execute
- Track submitted transactions. After a timeout, check their status before retrying so the same intended transfer is not sent twice
- Confirm withdrawals before spending their proceeds, then check the resulting on-chain balances. Stop and replan if a move fails or the state changes

### 6. Show when something needs attention

Operators need to know whether the application is keeping up and whether its balances agree with the chain.

- Keep structured logs linked to vaults, plans, and transactions
- Track processing delay, failed events, reorgs, and pending transactions
- Compare calculated balances with contract balances at the same block. Alert on unexplained differences and pause execution until they are understood

## Suggested order of work

Build confidence in the live balances before allowing the system to move funds.

1. Add database storage and restart/reorg recovery tests
2. Consume live events in report-only mode and compare balances with the chain
3. Add saved, versioned plans and transaction simulation
4. Test execution and failure recovery, then introduce limited, reviewed transfers
