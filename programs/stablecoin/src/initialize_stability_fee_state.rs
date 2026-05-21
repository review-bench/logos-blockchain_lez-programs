use nssa_core::{
    account::{Account, AccountWithMetadata, Data},
    program::{AccountPostState, Claim, ProgramId},
};
use stablecoin_core::{verify_stability_fee_state_and_get_seed, StabilityFeeState};

/// Initialize the program-global stability-fee accumulator state.
///
/// This is intentionally separate from `open_position` so position creation cannot silently
/// materialize a zero-rate global fee state.
///
/// # Authority is not yet governance-bound
/// `authority` is only checked for the runtime `is_authorized` flag — it is **not** pinned to a
/// designated governance identity. Because [`StabilityFeeState`] is a write-once singleton, the
/// first caller permanently fixes `stability_fee_rate`. Binding `authority` to a governance
/// account is deferred to a dedicated governance follow-up; until then this instruction must only
/// be exercised as a trusted deployment/bootstrap step.
///
/// `current_timestamp` is caller-supplied here rather than oracle-sourced (unlike the clock
/// required by [`StabilityFeeState::accrue_global`]). It should be set to the live oracle time at
/// bootstrap: a far-future value stalls the first accrual until the oracle clock passes it, and a
/// far-past value applies a large retroactive delta on the first accrual.
///
/// # Panics
/// - `authority` is not authorized.
/// - `stability_fee_state` is already initialized.
/// - `stability_fee_state.account_id` does not match the program-global PDA.
pub fn initialize_stability_fee_state(
    authority: AccountWithMetadata,
    stability_fee_state: AccountWithMetadata,
    stablecoin_program_id: ProgramId,
    stability_fee_rate: u128,
    current_timestamp: u64,
) -> Vec<AccountPostState> {
    // TODO(governance): pin `authority` to a designated governance account ID instead of
    // accepting any authorized signer. Tracked as a governance follow-up.
    assert!(
        authority.is_authorized,
        "Stability fee authority authorization is missing"
    );
    assert_eq!(
        stability_fee_state.account,
        Account::default(),
        "Stability fee state account must be uninitialized"
    );

    let seed = verify_stability_fee_state_and_get_seed(&stability_fee_state, stablecoin_program_id);
    let fee_state = StabilityFeeState::new(stability_fee_rate, current_timestamp);

    let mut state_post = stability_fee_state.account;
    state_post.program_owner = stablecoin_program_id;
    state_post.data = Data::from(&fee_state);

    vec![
        AccountPostState::new(authority.account),
        AccountPostState::new_claimed(state_post, Claim::Pda(seed)),
    ]
}
