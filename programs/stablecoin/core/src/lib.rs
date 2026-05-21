//! Core data structures and utilities for the Stablecoin Program.

use borsh::{BorshDeserialize, BorshSerialize};
use nssa_core::{
    account::{Account, AccountId, AccountWithMetadata, Data},
    program::{PdaSeed, ProgramId},
};
use serde::{Deserialize, Serialize};
use spel_framework_macros::account_type;

mod stability_fee;

#[cfg(test)]
mod tests;

pub use stability_fee::{
    accrue_position_stability_fee, accrue_stability_fees, accrued_debt_amount,
    apply_debt_change_after_fee_accrual, collateralization_ratio_bps,
    ensure_minimum_collateralization, stability_fee_growth_factor, DebtChange, StabilityFeeError,
    StabilityFeeState, COLLATERALIZATION_RATIO_BPS_DENOMINATOR, FEE_ACCUMULATOR_SCALE,
};

const POSITION_PDA_DOMAIN: [u8; 32] = [0; 32];
const POSITION_VAULT_PDA_DOMAIN: [u8; 32] = [1; 32];
const STABILITY_FEE_STATE_PDA_DOMAIN: [u8; 32] = [2; 32];

/// Stablecoin Program Instruction.
#[derive(Debug, Serialize, Deserialize)]
pub enum Instruction {
    /// Initialize the program-global [`StabilityFeeState`] account.
    ///
    /// Required accounts (2):
    /// - Stability-fee authority account (authorized; not yet bound to a designated governance
    ///   identity — see `initialize_stability_fee_state`, treat as a trusted bootstrap step)
    /// - Program-global [`StabilityFeeState`] account (uninitialized, address must match
    ///   `compute_stability_fee_state_pda(self_program_id)`)
    InitializeStabilityFeeState {
        /// Fixed-point stability fee rate per timestamp unit.
        stability_fee_rate: u128,
        /// Initial accumulator timestamp.
        current_timestamp: u64,
    },

    /// Open a new collateral-only [`Position`] for the calling owner.
    ///
    /// Required accounts (6):
    /// - Owner account (authorized)
    /// - Position account (uninitialized, address must match
    ///   `compute_position_pda(self_program_id, owner, token_definition)`)
    /// - Position vault token holding account (uninitialized, address must match
    ///   `compute_position_vault_pda(self_program_id, position_id)`)
    /// - Owner's source token holding for the collateral (authorized, initialized)
    /// - Token definition account for the collateral (matches the user holding's `definition_id`;
    ///   its `program_owner` determines the Token Program used by the chained `InitializeAccount`
    ///   / `Transfer` calls)
    /// - Program-global [`StabilityFeeState`] account (initialized, address must match
    ///   `compute_stability_fee_state_pda(self_program_id)`)
    OpenPosition {
        /// Amount of collateral tokens to deposit into the position vault.
        collateral_amount: u128,
    },
    /// Withdraw `amount` collateral tokens from a position back to a user-controlled holding.
    ///
    /// Required accounts (4):
    /// - Owner account (authorized)
    /// - Position account (initialized, owned by `self_program_id`)
    /// - Position vault token holding (address must match
    ///   `compute_position_vault_pda(self_program_id, position_id)`)
    /// - Destination user collateral holding (initialized, owned by the vault's Token Program,
    ///   `TokenHolding.definition_id == Position.collateral_definition_id`)
    ///
    /// `token_program_id` is derived from `vault.account.program_owner`;
    /// `collateral_definition_id` is read from the decoded [`Position`].
    ///
    /// **Note:** until issues #97/#96/#95 land, this instruction hard-asserts
    /// `Position.debt_amount == 0` instead of accruing fees and checking the
    /// collateralization ratio.
    WithdrawCollateral {
        /// Amount of collateral tokens to move from the vault back to `destination`.
        amount: u128,
    },
    /// Repay `amount` of outstanding stablecoin debt against an existing position.
    ///
    /// Required accounts (4):
    /// - Owner account (authorized; binds caller-as-owner via position PDA re-derivation)
    /// - Position account (initialized, owned by `self_program_id`)
    /// - Stablecoin token definition account (the definition of the stablecoin being repaid)
    /// - User's stablecoin holding (authorized, initialized, owned by the same Token Program as
    ///   the definition, with `TokenHolding.definition_id == stablecoin_definition.account_id`)
    ///
    /// `token_program_id` is derived from `user_stablecoin_holding.account.program_owner`.
    /// `collateral_definition_id` (for position PDA verification) is read from the
    /// decoded [`Position`].
    ///
    /// **Note:** until issue #97 (stability fee accrual) lands, this instruction does
    /// not accrue fees before reducing debt. A `// TODO(#97)` comment in the host
    /// function marks where the accrual code will plug in. Today every position has
    /// `debt_amount = 0` (no `generate_debt` yet), so the precondition is vacuously met.
    ///
    /// **Note:** until issue #91 (`generate_debt`) records the stablecoin definition
    /// into `Position`, this instruction cannot validate that the passed
    /// `stablecoin_token_definition` is the one this position's debt is denominated
    /// in. The caller is trusted for that until then.
    RepayDebt {
        /// Amount of stablecoin debt to repay (also the amount burned from the user's holding).
        amount: u128,
    },
}

/// Persistent state held by a Stablecoin [`Position`] account.
///
/// `debt_amount` is nominal debt, and `fee_accumulator` is the global stability-fee accumulator
/// snapshot used when this position was last settled. `open_position` initializes `debt_amount`
/// to `0` and snapshots the initialized global accumulator; drawing debt is deferred to a future
/// `generate_debt` instruction.
#[account_type]
#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct Position {
    /// Token holding account (vault PDA) that custodies the collateral backing this position.
    pub collateral_vault_id: AccountId,
    /// Token definition for the collateral held in `collateral_vault_id`.
    pub collateral_definition_id: AccountId,
    /// Amount of collateral tokens deposited.
    pub collateral_amount: u128,
    /// Outstanding nominal stablecoin debt against this position.
    pub debt_amount: u128,
    /// Global stability-fee accumulator value at the last position fee settlement.
    pub fee_accumulator: u128,
}

impl TryFrom<&Data> for Position {
    type Error = std::io::Error;

    fn try_from(data: &Data) -> Result<Self, Self::Error> {
        Self::try_from_slice(data.as_ref())
    }
}

impl From<&Position> for Data {
    fn from(position: &Position) -> Self {
        let mut data = Vec::with_capacity(std::mem::size_of_val(position));
        BorshSerialize::serialize(position, &mut data)
            .expect("Serialization to Vec should not fail");
        Self::try_from(data).expect("Position encoded data should fit into Data")
    }
}

/// PDA seed for the [`Position`] account owned by `owner_id` for `collateral_definition_id`.
///
/// Derived from the owner and collateral definition addresses with a domain-separation tag
/// so one owner can hold separate positions for separate collateral definitions.
pub fn compute_position_pda_seed(
    owner_id: AccountId,
    collateral_definition_id: AccountId,
) -> PdaSeed {
    use risc0_zkvm::sha::{Impl, Sha256 as _};

    let mut bytes = [0u8; 96];
    bytes[0..32].copy_from_slice(&owner_id.to_bytes());
    bytes[32..64].copy_from_slice(&collateral_definition_id.to_bytes());
    bytes[64..96].copy_from_slice(&POSITION_PDA_DOMAIN);

    let mut out = [0u8; 32];
    out.copy_from_slice(Impl::hash_bytes(&bytes).as_bytes());
    PdaSeed::new(out)
}

/// Account id of the [`Position`] PDA owned by `owner_id` under `stablecoin_program_id`.
pub fn compute_position_pda(
    stablecoin_program_id: ProgramId,
    owner_id: AccountId,
    collateral_definition_id: AccountId,
) -> AccountId {
    AccountId::for_public_pda(
        &stablecoin_program_id,
        &compute_position_pda_seed(owner_id, collateral_definition_id),
    )
}

/// PDA seed for the collateral vault token holding bound to a [`Position`].
///
/// Derived from the position's address with a distinct domain-separation tag so the vault
/// id cannot collide with the position id even though both PDAs share the same program.
pub fn compute_position_vault_pda_seed(position_id: AccountId) -> PdaSeed {
    use risc0_zkvm::sha::{Impl, Sha256 as _};

    let mut bytes = [0u8; 64];
    bytes[0..32].copy_from_slice(&position_id.to_bytes());
    bytes[32..64].copy_from_slice(&POSITION_VAULT_PDA_DOMAIN);

    let mut out = [0u8; 32];
    out.copy_from_slice(Impl::hash_bytes(&bytes).as_bytes());
    PdaSeed::new(out)
}

/// Account id of the collateral vault PDA for `position_id` under `stablecoin_program_id`.
pub fn compute_position_vault_pda(
    stablecoin_program_id: ProgramId,
    position_id: AccountId,
) -> AccountId {
    AccountId::for_public_pda(
        &stablecoin_program_id,
        &compute_position_vault_pda_seed(position_id),
    )
}

/// Verify the position account's address matches
/// `(stablecoin_program_id, owner, collateral_definition_id)` and return the [`PdaSeed`] for
/// use in post-state claims.
///
/// # Panics
/// If `position.account_id` does not match the address derived from `owner`,
/// `collateral_definition_id`, and `stablecoin_program_id`.
pub fn verify_position_and_get_seed(
    position: &AccountWithMetadata,
    owner: &AccountWithMetadata,
    collateral_definition_id: AccountId,
    stablecoin_program_id: ProgramId,
) -> PdaSeed {
    let seed = compute_position_pda_seed(owner.account_id, collateral_definition_id);
    let expected_id = AccountId::for_public_pda(&stablecoin_program_id, &seed);
    assert_eq!(
        position.account_id, expected_id,
        "Position account ID does not match expected derivation"
    );
    seed
}

/// Verify the vault account's address matches `(stablecoin_program_id, position)` and
/// return the [`PdaSeed`] for use in chained calls.
///
/// # Panics
/// If `vault.account_id` does not match the address derived from `position_id` and
/// `stablecoin_program_id`.
pub fn verify_position_vault_and_get_seed(
    vault: &AccountWithMetadata,
    position_id: AccountId,
    stablecoin_program_id: ProgramId,
) -> PdaSeed {
    let seed = compute_position_vault_pda_seed(position_id);
    let expected_id = AccountId::for_public_pda(&stablecoin_program_id, &seed);
    assert_eq!(
        vault.account_id, expected_id,
        "Position vault account ID does not match expected derivation"
    );
    seed
}

/// PDA seed for the program-global [`StabilityFeeState`] singleton account.
///
/// The stablecoin program holds exactly one stability-fee state, so the seed is derived
/// solely from a domain-separation tag with no per-caller input.
pub fn compute_stability_fee_state_pda_seed() -> PdaSeed {
    use risc0_zkvm::sha::{Impl, Sha256 as _};

    let mut out = [0u8; 32];
    out.copy_from_slice(Impl::hash_bytes(&STABILITY_FEE_STATE_PDA_DOMAIN).as_bytes());
    PdaSeed::new(out)
}

/// Account id of the program-global [`StabilityFeeState`] PDA under `stablecoin_program_id`.
pub fn compute_stability_fee_state_pda(stablecoin_program_id: ProgramId) -> AccountId {
    AccountId::for_public_pda(
        &stablecoin_program_id,
        &compute_stability_fee_state_pda_seed(),
    )
}

/// Verify the stability-fee state account's address matches the program-global PDA and
/// return the [`PdaSeed`] for use in post-state claims.
///
/// # Panics
/// If `stability_fee_state.account_id` does not match the address derived from
/// `stablecoin_program_id`.
pub fn verify_stability_fee_state_and_get_seed(
    stability_fee_state: &AccountWithMetadata,
    stablecoin_program_id: ProgramId,
) -> PdaSeed {
    let seed = compute_stability_fee_state_pda_seed();
    let expected_id = AccountId::for_public_pda(&stablecoin_program_id, &seed);
    assert_eq!(
        stability_fee_state.account_id, expected_id,
        "Stability fee state account ID does not match expected derivation"
    );
    seed
}

/// Verify the stability-fee state account is the initialized, program-owned global PDA, and
/// return its [`PdaSeed`] (for post-state claims) together with the decoded [`StabilityFeeState`].
///
/// # Panics
/// - `stability_fee_state.account_id` does not match the program-global PDA.
/// - the account is uninitialized.
/// - the account is not owned by this Stablecoin Program.
/// - the account data cannot be decoded as a [`StabilityFeeState`].
pub fn verify_initialized_stability_fee_state(
    stability_fee_state: &AccountWithMetadata,
    stablecoin_program_id: ProgramId,
) -> (PdaSeed, StabilityFeeState) {
    let seed = verify_stability_fee_state_and_get_seed(stability_fee_state, stablecoin_program_id);
    assert_ne!(
        stability_fee_state.account,
        Account::default(),
        "Stability fee state account must be initialized"
    );
    assert_eq!(
        stability_fee_state.account.program_owner, stablecoin_program_id,
        "Stability fee state account is not owned by this Stablecoin Program"
    );
    let fee_state = StabilityFeeState::try_from(&stability_fee_state.account.data)
        .expect("Stability fee state account must hold valid StabilityFeeState data");
    (seed, fee_state)
}
