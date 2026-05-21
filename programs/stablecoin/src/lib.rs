//! The Stablecoin Program implementation.

pub use stablecoin_core as core;

/// Initialize the program-global stability-fee accumulator state.
pub mod initialize_stability_fee_state;

/// Open a new collateral-only position for a calling owner.
pub mod open_position;

/// Repay outstanding stablecoin debt against an existing position.
pub mod repay_debt;

/// Withdraw collateral from an existing position back to a user-controlled holding.
pub mod withdraw_collateral;

#[cfg(test)]
mod tests;
