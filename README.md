# Curator vault manager

This version starts with one job: read the supplied event feed, determine which
events still count, and reconstruct a readable report of each vault's funds.
A vault pools deposits; shares represent ownership of the pool. Strategies hold
some of its assets, and the rest is idle. Earnings increase assets, not shares.
It also reads the curator policy and proposes safe fund movements. No transfers
are executed.

## Run

Requires a current stable Rust toolchain with Cargo. Run commands from the repository root, which contains `Cargo.toml`. The application reads `data/events.jsonl` and `data/policy.json` relative to the working directory. No credentials, live blockchain connection, or additional configuration are required. Cargo downloads dependencies on the first build.

```sh
cargo run
```

The supplied feed references `vault-legacy` without creating it. The program prints the two valid vaults and their plans, identifies the excluded vault, and exits with status **1** to signal incomplete history. This is expected fixture behavior, not a crash.

## Verify

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

See [NOTES.md](NOTES.md) for limitations and the proposed live-service design.

## Programme flow

```text
main::run
  read data/events.jsonl
  feed::reconstruct
    canonical_events: parse deliveries and resolve history
    walk surviving events in blockchain order
      create the vault, or call state::apply on a copy
      accept the updated state, or exclude the invalid vault
  log errors, cap excesses, and summary
  read data/policy.json
  Policy::parse: deserialize ordinary maps and validate percentages
  plan::build: select vaults with both usable state and policy
    plan_vault
      calculate_targets
      withdraw_surpluses
      fund_deficits
      validate_final_state
  report::render + report::render_plans
  write report to stdout and choose exit status
```

1. **Read deliveries in file order.** Each JSON line is an `Event` or a `Reorg`.
   Malformed lines return a custom error with the input line number.
2. **Resolve history before accounting.** Store events in a `BTreeMap` keyed by
   `(block, log_index)`. Identical duplicates count once. Different contents at
   the same position produce `ConflictingEvent`. A reorg removes its boundary
   block and all later events, allowing replacement events at those positions.
3. **Replay in chain order.** The map supplies sorted events regardless of their
   arrival order. A vault must first have a `VaultCreated` event. Its subsequent
   events update a candidate copy through `state::apply`.
4. **Accept only successful updates.** All arithmetic is checked. Deposits add
   idle assets and shares; withdrawals remove them; earnings add idle assets;
   allocations and deallocations transfer between idle and strategies. A failed
   candidate is discarded. Its entire vault is excluded, and later events for
   that vault are skipped. Other vaults continue.
5. **Read and validate policy.** `Policy::parse` uses ordinary nested BTreeMaps:
   vault name -> strategy name -> basis points. Names must be nonempty, individual
   values must be at most 10,000, and each vault's total must be at most 10,000.
   Parse and validation failures return `PolicyError`. Duplicate JSON keys use
   the last value, deliberately avoiding a custom deserializer.
6. **Calculate targets.** For each strategy, requested assets are
   `total assets * basis points / 10,000`, rounded down using a u128 intermediate.
   The feasible amount is the smaller of the request and the on-chain cap.
   A missing cap means zero; a strategy omitted from a configured policy targets
   zero. A missing vault policy causes a skip, not an implicit liquidation.
7. **Simulate the plan.** `withdraw_surpluses` moves every surplus to idle first.
   `fund_deficits` then fills shortages. `apply_move` has explicit allocation and
   withdrawal branches, checking source funds and arithmetic before assigning
   balances. Allocations also check the destination cap. `validate_final_state`
   checks conservation, unchanged ownership, all caps, and feasible targets.
   The original state never changes; only the candidate copy is updated.
8. **Report the result.** Show total and idle assets, strategies and caps, raw
   shares, holder values, and rounding remainders. An incomplete report clearly
   identifies excluded vaults; details are logged to stderr. The plan then shows
   starting allocations and policy targets, ordered moves, projected end balances,
   and unmet allocations. Skipped plans include their reasons.

## Files and reading order

| File            | Responsibility                                              |
| --------------- | ----------------------------------------------------------- |
| `src/main.rs`   | File access, logging setup, output, exit status             |
| `src/event.rs`  | JSON event types; no custom deserializer                    |
| `src/feed.rs`   | Canonical history and reconstruction in two explicit passes |
| `src/state.rs`  | Balance types and event accounting rules                    |
| `src/error.rs`  | Named error variants and readable descriptions              |
| `src/policy.rs` | Ordinary JSON parsing and policy validation                 |
| `src/plan.rs`   | Targets, simulated transfers, and final safety checks       |
| `src/report.rs` | Integer-based amounts, state report, and plan presentation  |
| `src/lib.rs`    | Module declarations and the reconstruction entry point      |

Start with `feed::reconstruct`, then `canonical_events`, then `state::apply`.
There is no queue framework, generic processing pipeline, or live chain client.

## Logging

Structured `tracing` events go to stderr; the report goes to stdout.

```sh
# Show detailed logs
RUST_LOG=debug cargo run

# Send the report and JSON logs to separate files
LOG_FORMAT=json RUST_LOG=info cargo run --quiet > /tmp/report.txt 2> /tmp/events.log
```

INFO logs accepted replay events with vault, block, log index, line, event data,
and before/after idle and share balances. Reorgs and the summary also use INFO.
Duplicates use DEBUG, cap excesses and policy shortfalls WARN, and errors ERROR.
Validated plans log their move count at INFO. Planning skips log their typed reason
at ERROR, without suppressing valid plans for other vaults. Text logs use
color only when stderr is a terminal; JSON and redirected text remain uncolored.
The default level is INFO, and the default format is text.

Updates log only after successful accounting. A later error may exclude a vault
whose earlier events were logged, so these are replay diagnostics, not durable
transaction records. Errors are logged once at the CLI boundary.

## Accounting choices and limits

- Amounts are `u64` smallest asset units; overflow and insufficient balances
  return `AccountingError` variants. Input share amounts are used as supplied.
- Caps are reconstructed, not enforced during replay. Existing over-cap balances
  must be visible for the planner. Missing caps have effective value zero.
- Holder value is `assets * holder shares / total shares`, with a `u128`
  intermediate. Integer division rounds down to the smallest asset unit. The
  difference from summed rounded values is reported, not treated as lost funds.
- No shares means no defined holder valuation. Share decimals are unspecified,
  so shares are displayed in raw units. Asset amounts omit trailing decimal zeros.
- Reorg handling depends on the indexer's delivery contract. Without block hashes
  or history IDs, a delayed abandoned event after a reorg cannot be distinguished
  from a replacement. This is a batch snapshot, not a streaming consumer.
- Cloning vault state before each event keeps failure handling easy to follow.
  It is acceptable for this fixture but is not optimized for large holder sets.

Tests cover duplicate/order handling, reorgs, conflicting events, incomplete
history, checked arithmetic, cap changes, holder values, formatting, and CLI logs.

## Planning example and limits

For `vault-yield`, Morpho holds 240,000 tokens. Its 40% policy target is 161,200
of the vault's 403,000 tokens, below its 200,000 cap. Withdraw 78,800 to idle,
then move 121,500 from idle to Aave. Ether.fi's missing cap blocks its 40,300
allocation; that amount stays idle and is reported as unmet.

- Capped shortfalls are not redirected to other strategies. Unassigned policy
  percentages and target-rounding remainders also stay idle.
- An explicit empty policy withdraws all strategy balances to idle. Missing
  policy or missing usable state instead produces a typed `SkipReason`.
- `PlanError` identifies arithmetic, source-balance, cap, conservation, and target
  failures. A failed candidate is discarded; valid plans for other vaults survive.
- Cap-limited targets are expected outcomes, not fatal errors. Incomplete history
  or skipped plans produce a nonzero exit status. Invalid policy aborts planning.
- Plans assume full withdrawal liquidity, no fees, and unchanged chain state.
  A future execution service must revalidate them before submitting transactions.

Planner tests independently replay moves and check ordering, conservation, caps,
rounding, large amounts, determinism, missing configuration, and no-op replanning.
