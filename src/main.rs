use std::{
    error::Error,
    fs,
    io::{self, IsTerminal, Write},
    process::ExitCode,
};
use tracing_subscriber::EnvFilter;

/// Read both fixtures, reconstruct state, build plans, and write only the report
/// to stdout. Incomplete history produces a report but a failing exit status.
fn run() -> Result<bool, Box<dyn Error>> {
    let feed = fs::read_to_string("data/events.jsonl")?;
    let result = curator_manager::reconstruct(&feed)?;
    for diagnostic in &result.diagnostics {
        log_reconstruction_error(diagnostic);
    }
    for (vault, state) in &result.vaults {
        for (strategy, balance) in &state.strategies {
            if balance.assets > balance.cap {
                tracing::warn!(
                    vault,
                    strategy,
                    assets = balance.assets,
                    cap = balance.cap,
                    excess = balance.assets - balance.cap,
                    "Final strategy balance exceeds cap"
                );
            }
        }
    }
    let stats = &result.stats;
    tracing::info!(
        deliveries = stats.deliveries,
        duplicates = stats.duplicates,
        reorgs = stats.reorgs,
        removed_events = stats.removed_events,
        canonical_events = stats.canonical_events,
        valid_vaults = result.vaults.len(),
        excluded_vaults = result.diagnostics.len(),
        complete = result.diagnostics.is_empty(),
        "Reconstruction finished"
    );
    let policy = curator_manager::policy::Policy::parse(&fs::read_to_string("data/policy.json")?)?;
    let plans = curator_manager::plan::build(&result.vaults, &policy);
    for (vault, reason) in &plans.skipped {
        tracing::error!(vault, reason = %reason, "Vault planning skipped");
    }
    for (vault, plan) in &plans.plans {
        tracing::info!(
            vault,
            moves = plan.moves().len(),
            "Fund movement plan validated (not executed)"
        );
        for (strategy, target) in plan.targets() {
            if target.requested_amount > target.feasible_amount {
                tracing::warn!(
                    vault,
                    strategy,
                    requested = target.requested_amount,
                    feasible = target.feasible_amount,
                    shortfall = target.requested_amount - target.feasible_amount,
                    "Policy target limited by cap"
                );
            }
        }
    }
    let report =
        curator_manager::report::render(&result)? + &curator_manager::report::render_plans(&plans);
    let mut stdout = io::stdout().lock();
    stdout.write_all(report.as_bytes())?;
    stdout.flush()?;
    Ok(result.diagnostics.is_empty() && plans.skipped.is_empty())
}

fn main() -> ExitCode {
    let filter = match EnvFilter::try_from_default_env() {
        Ok(filter) => filter,
        Err(error) if std::env::var_os("RUST_LOG").is_some() => {
            eprintln!("Invalid RUST_LOG: {error}");
            return ExitCode::FAILURE;
        }
        Err(_) => EnvFilter::new("info"),
    };
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(io::stderr)
        .with_ansi(io::stderr().is_terminal());
    match std::env::var("LOG_FORMAT").as_deref() {
        Ok("json") => subscriber.json().init(),
        Ok("text") | Err(std::env::VarError::NotPresent) => subscriber.init(),
        _ => {
            eprintln!("Invalid LOG_FORMAT: expected text or json");
            return ExitCode::FAILURE;
        }
    }
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            if let Some(error) = error.downcast_ref::<curator_manager::error::ReconstructionError>()
            {
                log_reconstruction_error(error);
            } else {
                tracing::error!(error = %error, "Application failed");
            }
            ExitCode::FAILURE
        }
    }
}

/// Log failures once at the CLI boundary, with the available input location.
fn log_reconstruction_error(error: &curator_manager::error::ReconstructionError) {
    use curator_manager::error::ReconstructionError;
    match error {
        ReconstructionError::InvalidDelivery { line, .. } => {
            tracing::error!(line, error = %error, "Invalid delivery")
        }
        ReconstructionError::ConflictingEvent { block, log_index, first_line, incoming_line } => {
            tracing::error!(block, log_index, first_line, incoming_line, error = %error, "Conflicting event");
        }
        ReconstructionError::InvalidVaultHistory {
            line,
            vault,
            position: (block, log_index),
            source,
        } => {
            tracing::error!(line, vault = %vault, block, log_index, cause = ?source, error = %error, "Vault excluded");
        }
    }
}
