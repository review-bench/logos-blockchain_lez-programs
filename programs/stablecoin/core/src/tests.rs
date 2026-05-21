use nssa_core::account::{AccountId, Data};

use crate::{
    accrue_position_stability_fee, accrue_stability_fees, accrued_debt_amount,
    apply_debt_change_after_fee_accrual, collateralization_ratio_bps,
    ensure_minimum_collateralization, stability_fee_growth_factor, DebtChange, Position,
    StabilityFeeError, StabilityFeeState, FEE_ACCUMULATOR_SCALE,
};

fn position(collateral_amount: u128, debt_amount: u128, fee_accumulator: u128) -> Position {
    Position {
        collateral_vault_id: AccountId::new([0x11; 32]),
        collateral_definition_id: AccountId::new([0x22; 32]),
        collateral_amount,
        debt_amount,
        fee_accumulator,
    }
}

#[test]
fn stability_fee_growth_factor_approximates_continuous_compounding() {
    let one_percent_per_tick = FEE_ACCUMULATOR_SCALE / 100;
    let factor = stability_fee_growth_factor(one_percent_per_tick, 10);

    assert!(factor > 1_105_170_918_000_000_000);
    assert!(factor < 1_105_170_918_100_000_000);
}

#[test]
fn stability_fee_growth_factor_uses_range_reduction_for_large_finite_exponent() {
    let one_percent_per_tick = FEE_ACCUMULATOR_SCALE / 100;
    let factor = stability_fee_growth_factor(one_percent_per_tick, 4_000);

    assert!(factor > 235_385_266_000_000_000_000_000_000_000_000_000);
    assert!(factor < 235_385_267_500_000_000_000_000_000_000_000_000);
}

#[test]
fn stability_fee_growth_factor_saturates_on_extreme_input() {
    assert_eq!(stability_fee_growth_factor(u128::MAX, u64::MAX), u128::MAX);
}

#[test]
fn accrue_global_saturates_and_never_bricks() {
    let mut state = StabilityFeeState::new(u128::MAX, 0);

    state.accrue_global(1_000).unwrap();
    assert_eq!(state.stability_fee_accumulator, u128::MAX);
    assert_eq!(state.last_fee_update_timestamp, 1_000);

    state.accrue_global(2_000).unwrap();
    assert_eq!(state.stability_fee_accumulator, u128::MAX);
    assert_eq!(state.last_fee_update_timestamp, 2_000);
}

#[test]
fn stability_fee_state_accrues_global_accumulator() {
    let mut state = StabilityFeeState::new(FEE_ACCUMULATOR_SCALE / 100, 10);

    state.accrue_global(20).unwrap();

    assert!(state.stability_fee_accumulator > FEE_ACCUMULATOR_SCALE);
    assert_eq!(state.last_fee_update_timestamp, 20);
}

#[test]
fn stability_fee_state_rejects_timestamp_regression() {
    let mut state = StabilityFeeState::new(FEE_ACCUMULATOR_SCALE / 100, 20);

    let err = state.accrue_global(19).unwrap_err();

    assert_eq!(
        err,
        StabilityFeeError::TimestampMovedBackward {
            current_timestamp: 19,
            last_fee_update_timestamp: 20,
        }
    );
}

#[test]
fn accrued_debt_amount_scales_nominal_debt_from_position_accumulator() {
    let position = position(1_000, 100, FEE_ACCUMULATOR_SCALE);

    let debt = accrued_debt_amount(&position, FEE_ACCUMULATOR_SCALE * 2).unwrap();

    assert_eq!(debt, 200);
}

#[test]
fn accrued_debt_amount_ceils_fractional_growth() {
    let position = position(1_000, 1, FEE_ACCUMULATOR_SCALE);

    let debt = accrued_debt_amount(&position, FEE_ACCUMULATOR_SCALE + 1).unwrap();

    assert_eq!(debt, 2);
}

#[test]
fn accrued_debt_amount_rejects_zero_position_accumulator() {
    let position = position(1_000, 100, 0);

    let err = accrued_debt_amount(&position, FEE_ACCUMULATOR_SCALE).unwrap_err();

    assert_eq!(err, StabilityFeeError::AccumulatorIsZero);
}

#[test]
fn accrued_debt_amount_rejects_zero_global_accumulator() {
    let position = position(1_000, 100, FEE_ACCUMULATOR_SCALE);

    let err = accrued_debt_amount(&position, 0).unwrap_err();

    assert_eq!(err, StabilityFeeError::AccumulatorIsZero);
}

#[test]
fn accrued_debt_amount_rejects_backward_accumulator() {
    let position = position(1_000, 100, FEE_ACCUMULATOR_SCALE * 2);

    let err = accrued_debt_amount(&position, FEE_ACCUMULATOR_SCALE).unwrap_err();

    assert_eq!(
        err,
        StabilityFeeError::AccumulatorMovedBackward {
            current_accumulator: FEE_ACCUMULATOR_SCALE,
            position_fee_accumulator: FEE_ACCUMULATOR_SCALE * 2,
        }
    );
}

#[test]
fn accrue_position_stability_fee_updates_debt_and_snapshots_accumulator() {
    let mut position = position(1_000, 100, FEE_ACCUMULATOR_SCALE);

    accrue_position_stability_fee(&mut position, FEE_ACCUMULATOR_SCALE * 2).unwrap();

    assert_eq!(position.debt_amount, 200);
    assert_eq!(position.fee_accumulator, FEE_ACCUMULATOR_SCALE * 2);
}

#[test]
fn accrue_stability_fees_accrues_global_then_position() {
    let mut position = position(1_000, 100, FEE_ACCUMULATOR_SCALE);
    let mut state = StabilityFeeState::new(FEE_ACCUMULATOR_SCALE / 10, 0);

    accrue_stability_fees(&mut position, &mut state, 1).unwrap();

    assert!(state.stability_fee_accumulator > FEE_ACCUMULATOR_SCALE);
    assert_eq!(position.fee_accumulator, state.stability_fee_accumulator);
    assert!(position.debt_amount > 100);
}

#[test]
fn debt_change_accrues_existing_debt_before_increase() {
    let mut position = position(1_000, 100, FEE_ACCUMULATOR_SCALE);
    let mut state = StabilityFeeState::new(FEE_ACCUMULATOR_SCALE / 10, 0);

    apply_debt_change_after_fee_accrual(&mut position, &mut state, 1, DebtChange::Increase(50))
        .unwrap();

    assert_eq!(position.fee_accumulator, state.stability_fee_accumulator);
    assert_eq!(position.debt_amount, 161);
}

#[test]
fn debt_change_rejects_repayment_above_accrued_debt() {
    let mut position = position(1_000, 100, FEE_ACCUMULATOR_SCALE);
    let mut state = StabilityFeeState::new(0, 0);

    let err = apply_debt_change_after_fee_accrual(
        &mut position,
        &mut state,
        0,
        DebtChange::Decrease(101),
    )
    .unwrap_err();

    assert_eq!(
        err,
        StabilityFeeError::DebtRepaymentExceedsDebt {
            debt_amount: 100,
            repayment_amount: 101,
        }
    );
}

#[test]
fn debt_change_rejects_increase_overflow() {
    let mut position = position(1_000, u128::MAX, FEE_ACCUMULATOR_SCALE);
    let mut state = StabilityFeeState::new(0, 0);

    let err =
        apply_debt_change_after_fee_accrual(&mut position, &mut state, 0, DebtChange::Increase(1))
            .unwrap_err();

    assert_eq!(err, StabilityFeeError::ArithmeticOverflow);
}

#[test]
fn partial_repayment_reduces_accrued_nominal_debt() {
    let mut state = StabilityFeeState {
        stability_fee_accumulator: 3 * FEE_ACCUMULATOR_SCALE / 2,
        stability_fee_rate: 0,
        last_fee_update_timestamp: 5,
    };
    let mut position = position(1_000, 100, FEE_ACCUMULATOR_SCALE);

    apply_debt_change_after_fee_accrual(&mut position, &mut state, 5, DebtChange::Decrease(50))
        .unwrap();

    assert_eq!(position.debt_amount, 100);
    assert_eq!(position.fee_accumulator, 3 * FEE_ACCUMULATOR_SCALE / 2);
}

#[test]
fn exact_full_repayment_zeroes_debt() {
    let mut state = StabilityFeeState {
        stability_fee_accumulator: 3 * FEE_ACCUMULATOR_SCALE / 2,
        stability_fee_rate: 0,
        last_fee_update_timestamp: 5,
    };
    let mut position = position(1_000, 100, FEE_ACCUMULATOR_SCALE);

    apply_debt_change_after_fee_accrual(&mut position, &mut state, 5, DebtChange::Decrease(150))
        .unwrap();

    assert_eq!(position.debt_amount, 0);
    assert_eq!(position.fee_accumulator, 3 * FEE_ACCUMULATOR_SCALE / 2);
}

#[test]
fn collateralization_ratio_uses_accrued_debt_and_redemption_price() {
    let position = position(300, 100, FEE_ACCUMULATOR_SCALE);
    let current_accumulator = FEE_ACCUMULATOR_SCALE * 2;

    let ratio = collateralization_ratio_bps(&position, current_accumulator, 4, 2).unwrap();

    assert_eq!(ratio, Some(30_000));
}

#[test]
fn minimum_collateralization_check_fails_after_fee_growth() {
    let position = position(150, 100, FEE_ACCUMULATOR_SCALE);
    let current_accumulator = FEE_ACCUMULATOR_SCALE * 2;

    let err =
        ensure_minimum_collateralization(&position, current_accumulator, 1, 1, 10_000).unwrap_err();

    assert_eq!(
        err,
        StabilityFeeError::CollateralizationRatioTooLow {
            collateral_value: 150,
            debt_value: 200,
            minimum_collateralization_ratio_bps: 10_000,
        }
    );
}

#[test]
fn minimum_collateralization_check_allows_debt_free_position() {
    let position = position(0, 0, FEE_ACCUMULATOR_SCALE);

    ensure_minimum_collateralization(&position, FEE_ACCUMULATOR_SCALE * 10, 1, 1, u128::MAX)
        .unwrap();
}

#[test]
fn collateralization_ratio_rejects_zero_collateral_price() {
    let position = position(150, 100, FEE_ACCUMULATOR_SCALE);

    let err = collateralization_ratio_bps(&position, FEE_ACCUMULATOR_SCALE, 0, 1).unwrap_err();

    assert_eq!(err, StabilityFeeError::InvalidCollateralPrice);
}

#[test]
fn collateralization_ratio_rejects_zero_redemption_price() {
    let position = position(150, 100, FEE_ACCUMULATOR_SCALE);

    let err = collateralization_ratio_bps(&position, FEE_ACCUMULATOR_SCALE, 1, 0).unwrap_err();

    assert_eq!(err, StabilityFeeError::InvalidRedemptionPrice);
}

#[test]
fn collateralization_ratio_rejects_collateral_value_overflow() {
    let position = position(u128::MAX, 100, FEE_ACCUMULATOR_SCALE);

    let err = collateralization_ratio_bps(&position, FEE_ACCUMULATOR_SCALE, 2, 1).unwrap_err();

    assert_eq!(err, StabilityFeeError::ArithmeticOverflow);
}

#[test]
fn ensure_minimum_collateralization_rejects_collateral_value_overflow() {
    let position = position(u128::MAX, 100, FEE_ACCUMULATOR_SCALE);

    let err = ensure_minimum_collateralization(&position, FEE_ACCUMULATOR_SCALE, 2, 1, 10_000)
        .unwrap_err();

    assert_eq!(err, StabilityFeeError::ArithmeticOverflow);
}

#[test]
fn stability_fee_state_round_trips_through_data() {
    let state = StabilityFeeState {
        stability_fee_accumulator: FEE_ACCUMULATOR_SCALE + 123,
        stability_fee_rate: FEE_ACCUMULATOR_SCALE / 100,
        last_fee_update_timestamp: 42,
    };

    let data = Data::from(&state);
    let decoded = StabilityFeeState::try_from(&data).unwrap();

    assert_eq!(decoded, state);
}
