use borsh::{BorshDeserialize, BorshSerialize};
use nssa_core::account::Data;
use serde::{Deserialize, Serialize};
use spel_framework_macros::account_type;

use crate::Position;

/// Fixed-point scale for stability-fee accumulator indexes and rates.
pub const FEE_ACCUMULATOR_SCALE: u128 = 1_000_000_000_000_000_000;

/// Basis-point denominator used by collateralization ratio checks.
pub const COLLATERALIZATION_RATIO_BPS_DENOMINATOR: u128 = 10_000;

const EXP_TAYLOR_TERMS: u32 = 32;
const EXP_TAYLOR_MAX_INPUT: u128 = FEE_ACCUMULATOR_SCALE;

/// Program-global stability-fee accumulator state.
///
/// `stability_fee_accumulator` starts at [`FEE_ACCUMULATOR_SCALE`] and grows over time according
/// to `stability_fee_rate`. Every position stores the accumulator value it was last settled
/// against. Accruing a position scales its nominal `debt_amount` by
/// `global_accumulator / position.fee_accumulator`, then snapshots the global accumulator into the
/// position.
///
/// `stability_fee_rate` is a non-negative fixed-point rate per timestamp unit using
/// [`FEE_ACCUMULATOR_SCALE`]. For example, `FEE_ACCUMULATOR_SCALE / 100` means 1% per timestamp
/// unit, compounded continuously.
#[account_type]
#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize, BorshSerialize, BorshDeserialize)]
pub struct StabilityFeeState {
    /// Global debt growth index.
    pub stability_fee_accumulator: u128,
    /// Fixed-point stability fee rate per timestamp unit.
    pub stability_fee_rate: u128,
    /// Timestamp of the last global accumulator update.
    pub last_fee_update_timestamp: u64,
}

impl StabilityFeeState {
    /// Create a new global stability-fee state at `last_fee_update_timestamp`.
    pub fn new(stability_fee_rate: u128, last_fee_update_timestamp: u64) -> Self {
        Self {
            stability_fee_accumulator: FEE_ACCUMULATOR_SCALE,
            stability_fee_rate,
            last_fee_update_timestamp,
        }
    }

    /// Accrue global stability fees through `current_timestamp`.
    ///
    /// This updates `stability_fee_accumulator *= e^(rate * delta_time)` using fixed-point
    /// arithmetic and a range-reduced Taylor expansion. Fixed-point division rounds up, so
    /// accrual is protocol-favorable.
    ///
    /// When continuous compounding produces a factor too large to represent, the accumulator
    /// saturates at [`u128::MAX`] instead of failing; `current_timestamp` is still recorded, so the
    /// global state is not permanently bricked by a long accrual gap.
    ///
    /// # Errors
    /// Returns [`StabilityFeeError::TimestampMovedBackward`] if `current_timestamp` predates the
    /// last recorded update.
    pub fn accrue_global(&mut self, current_timestamp: u64) -> Result<(), StabilityFeeError> {
        let elapsed = current_timestamp
            .checked_sub(self.last_fee_update_timestamp)
            .ok_or(StabilityFeeError::TimestampMovedBackward {
                current_timestamp,
                last_fee_update_timestamp: self.last_fee_update_timestamp,
            })?;
        let growth_factor = stability_fee_growth_factor(self.stability_fee_rate, elapsed);
        self.stability_fee_accumulator = mul_div_ceil(
            self.stability_fee_accumulator,
            growth_factor,
            FEE_ACCUMULATOR_SCALE,
        )
        .unwrap_or(u128::MAX);
        self.last_fee_update_timestamp = current_timestamp;
        Ok(())
    }
}

impl Default for StabilityFeeState {
    fn default() -> Self {
        Self::new(0, 0)
    }
}

impl TryFrom<&Data> for StabilityFeeState {
    type Error = std::io::Error;

    fn try_from(data: &Data) -> Result<Self, Self::Error> {
        Self::try_from_slice(data.as_ref())
    }
}

impl From<&StabilityFeeState> for Data {
    fn from(state: &StabilityFeeState) -> Self {
        let mut data = Vec::with_capacity(std::mem::size_of_val(state));
        BorshSerialize::serialize(state, &mut data).expect("Serialization to Vec should not fail");
        Self::try_from(data).expect("Stability fee state encoded data should fit into Data")
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum StabilityFeeError {
    TimestampMovedBackward {
        current_timestamp: u64,
        last_fee_update_timestamp: u64,
    },
    AccumulatorIsZero,
    AccumulatorMovedBackward {
        current_accumulator: u128,
        position_fee_accumulator: u128,
    },
    InvalidCollateralPrice,
    InvalidRedemptionPrice,
    DebtRepaymentExceedsDebt {
        debt_amount: u128,
        repayment_amount: u128,
    },
    ArithmeticOverflow,
    CollateralizationRatioTooLow {
        collateral_value: u128,
        debt_value: u128,
        minimum_collateralization_ratio_bps: u128,
    },
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum DebtChange {
    Increase(u128),
    Decrease(u128),
}

/// Return `e^(rate_per_time_unit * elapsed)` as a fixed-point accumulator factor.
///
/// The result saturates at [`u128::MAX`] when continuous compounding produces a factor too large
/// to represent.
pub fn stability_fee_growth_factor(rate_per_time_unit: u128, elapsed: u64) -> u128 {
    if rate_per_time_unit == 0 || elapsed == 0 {
        return FEE_ACCUMULATOR_SCALE;
    }

    let Some(x) = rate_per_time_unit.checked_mul(u128::from(elapsed)) else {
        return u128::MAX;
    };
    let chunks = div_ceil(x, EXP_TAYLOR_MAX_INPUT);
    let reduced_x = div_ceil(x, chunks);

    fixed_point_pow_ceil(exp_taylor(reduced_x), chunks)
}

/// Derive the debt owed by a position at `current_stability_fee_accumulator`.
///
/// # Errors
/// - [`StabilityFeeError::AccumulatorIsZero`] for a zero global or position accumulator.
/// - [`StabilityFeeError::AccumulatorMovedBackward`] if the current accumulator is below the
///   position snapshot.
pub fn accrued_debt_amount(
    position: &Position,
    current_stability_fee_accumulator: u128,
) -> Result<u128, StabilityFeeError> {
    validate_accumulators(current_stability_fee_accumulator, position.fee_accumulator)?;
    if position.debt_amount == 0 {
        return Ok(0);
    }
    Ok(mul_div_ceil(
        position.debt_amount,
        current_stability_fee_accumulator,
        position.fee_accumulator,
    )
    .unwrap_or(u128::MAX))
}

/// Accrue one position against the current global accumulator and snapshot the index.
///
/// # Errors
/// - [`StabilityFeeError::AccumulatorIsZero`] for a zero global or position accumulator.
/// - [`StabilityFeeError::AccumulatorMovedBackward`] if the current accumulator is below the
///   position snapshot.
pub fn accrue_position_stability_fee(
    position: &mut Position,
    current_stability_fee_accumulator: u128,
) -> Result<(), StabilityFeeError> {
    position.debt_amount = accrued_debt_amount(position, current_stability_fee_accumulator)?;
    position.fee_accumulator = current_stability_fee_accumulator;
    Ok(())
}

/// Accrue global fees through `current_timestamp`, then accrue the position to that accumulator.
///
/// # Errors
/// Returns global or position accrual errors.
pub fn accrue_stability_fees(
    position: &mut Position,
    fee_state: &mut StabilityFeeState,
    current_timestamp: u64,
) -> Result<(), StabilityFeeError> {
    fee_state.accrue_global(current_timestamp)?;
    accrue_position_stability_fee(position, fee_state.stability_fee_accumulator)
}

/// Accrue fees first, then apply a nominal debt increase or decrease.
///
/// Debt-changing instructions should use this helper so they cannot mutate debt against a stale
/// fee index.
///
/// # Errors
/// - global or position accrual errors,
/// - [`StabilityFeeError::ArithmeticOverflow`] if an increase overflows `u128`,
/// - [`StabilityFeeError::DebtRepaymentExceedsDebt`] if a decrease exceeds accrued debt.
pub fn apply_debt_change_after_fee_accrual(
    position: &mut Position,
    fee_state: &mut StabilityFeeState,
    current_timestamp: u64,
    debt_change: DebtChange,
) -> Result<(), StabilityFeeError> {
    accrue_stability_fees(position, fee_state, current_timestamp)?;
    match debt_change {
        DebtChange::Increase(amount) => {
            position.debt_amount = position
                .debt_amount
                .checked_add(amount)
                .ok_or(StabilityFeeError::ArithmeticOverflow)?;
        }
        DebtChange::Decrease(amount) => {
            if amount > position.debt_amount {
                return Err(StabilityFeeError::DebtRepaymentExceedsDebt {
                    debt_amount: position.debt_amount,
                    repayment_amount: amount,
                });
            }
            position.debt_amount -= amount;
        }
    }
    Ok(())
}

/// Compute the current collateralization ratio in basis points after accrued fee growth.
///
/// Returns `None` for debt-free positions.
///
/// # Errors
/// - [`StabilityFeeError::InvalidCollateralPrice`] / [`StabilityFeeError::InvalidRedemptionPrice`]
///   for a zero price.
/// - accumulator validation errors.
/// - [`StabilityFeeError::ArithmeticOverflow`] if a value product overflows `u128`.
pub fn collateralization_ratio_bps(
    position: &Position,
    current_stability_fee_accumulator: u128,
    collateral_price: u128,
    redemption_price: u128,
) -> Result<Option<u128>, StabilityFeeError> {
    validate_prices(collateral_price, redemption_price)?;
    let debt_amount = accrued_debt_amount(position, current_stability_fee_accumulator)?;
    if debt_amount == 0 {
        return Ok(None);
    }

    let collateral_value = position
        .collateral_amount
        .checked_mul(collateral_price)
        .ok_or(StabilityFeeError::ArithmeticOverflow)?;
    let debt_value = debt_amount
        .checked_mul(redemption_price)
        .ok_or(StabilityFeeError::ArithmeticOverflow)?;
    let scaled_collateral_value = collateral_value
        .checked_mul(COLLATERALIZATION_RATIO_BPS_DENOMINATOR)
        .ok_or(StabilityFeeError::ArithmeticOverflow)?;

    Ok(Some(scaled_collateral_value / debt_value))
}

/// Require a position to meet `minimum_collateralization_ratio_bps` after fee growth.
///
/// # Errors
/// - [`StabilityFeeError::InvalidCollateralPrice`] / [`StabilityFeeError::InvalidRedemptionPrice`]
///   for a zero price.
/// - accumulator validation errors.
/// - [`StabilityFeeError::ArithmeticOverflow`] if a value product overflows `u128`.
/// - [`StabilityFeeError::CollateralizationRatioTooLow`] if the position is undercollateralized.
pub fn ensure_minimum_collateralization(
    position: &Position,
    current_stability_fee_accumulator: u128,
    collateral_price: u128,
    redemption_price: u128,
    minimum_collateralization_ratio_bps: u128,
) -> Result<(), StabilityFeeError> {
    validate_prices(collateral_price, redemption_price)?;
    let debt_amount = accrued_debt_amount(position, current_stability_fee_accumulator)?;
    if debt_amount == 0 {
        return Ok(());
    }

    let collateral_value = position
        .collateral_amount
        .checked_mul(collateral_price)
        .ok_or(StabilityFeeError::ArithmeticOverflow)?;
    let debt_value = debt_amount
        .checked_mul(redemption_price)
        .ok_or(StabilityFeeError::ArithmeticOverflow)?;
    let scaled_collateral_value = collateral_value
        .checked_mul(COLLATERALIZATION_RATIO_BPS_DENOMINATOR)
        .ok_or(StabilityFeeError::ArithmeticOverflow)?;
    let minimum_debt_value = debt_value
        .checked_mul(minimum_collateralization_ratio_bps)
        .ok_or(StabilityFeeError::ArithmeticOverflow)?;

    if scaled_collateral_value < minimum_debt_value {
        return Err(StabilityFeeError::CollateralizationRatioTooLow {
            collateral_value,
            debt_value,
            minimum_collateralization_ratio_bps,
        });
    }

    Ok(())
}

fn validate_accumulators(
    current_accumulator: u128,
    position_fee_accumulator: u128,
) -> Result<(), StabilityFeeError> {
    if current_accumulator == 0 || position_fee_accumulator == 0 {
        return Err(StabilityFeeError::AccumulatorIsZero);
    }
    if current_accumulator < position_fee_accumulator {
        return Err(StabilityFeeError::AccumulatorMovedBackward {
            current_accumulator,
            position_fee_accumulator,
        });
    }
    Ok(())
}

fn validate_prices(
    collateral_price: u128,
    redemption_price: u128,
) -> Result<(), StabilityFeeError> {
    if collateral_price == 0 {
        return Err(StabilityFeeError::InvalidCollateralPrice);
    }
    if redemption_price == 0 {
        return Err(StabilityFeeError::InvalidRedemptionPrice);
    }
    Ok(())
}

fn exp_taylor(x: u128) -> u128 {
    let mut sum = FEE_ACCUMULATOR_SCALE;
    let mut term = FEE_ACCUMULATOR_SCALE;

    for divisor in 1..=EXP_TAYLOR_TERMS {
        let denominator = FEE_ACCUMULATOR_SCALE * u128::from(divisor);
        let Some(next_term) = mul_div_ceil(term, x, denominator) else {
            return u128::MAX;
        };
        term = next_term;
        if term == 0 {
            break;
        }
        let Some(next_sum) = sum.checked_add(term) else {
            return u128::MAX;
        };
        sum = next_sum;
    }

    sum
}

fn fixed_point_pow_ceil(mut base: u128, mut exponent: u128) -> u128 {
    let mut result = FEE_ACCUMULATOR_SCALE;

    while exponent > 0 {
        if exponent & 1 == 1 {
            let Some(next_result) = mul_div_ceil(result, base, FEE_ACCUMULATOR_SCALE) else {
                return u128::MAX;
            };
            result = next_result;
        }
        exponent >>= 1;
        if exponent > 0 {
            let Some(next_base) = mul_div_ceil(base, base, FEE_ACCUMULATOR_SCALE) else {
                return u128::MAX;
            };
            base = next_base;
        }
    }

    result
}

fn div_ceil(numerator: u128, denominator: u128) -> u128 {
    let quotient = numerator / denominator;
    quotient.saturating_add(u128::from(!numerator.is_multiple_of(denominator)))
}

/// Computes the exact quotient and remainder-flag of `(lhs * rhs) / denominator` using a full
/// 256-bit intermediate product so the multiplication never overflows prematurely.
fn mul_div(lhs: u128, rhs: u128, denominator: u128) -> Option<(u128, bool)> {
    if denominator == 0 {
        return None;
    }

    let (product_high, product_low) = widening_mul(lhs, rhs);
    if product_high == 0 {
        return Some((product_low / denominator, product_low % denominator != 0));
    }

    let mut quotient: u128 = 0;
    let mut remainder: u128 = 0;
    for bit in (0..256u32).rev() {
        let next_bit = if bit >= 128 {
            (product_high >> (bit - 128)) & 1
        } else {
            (product_low >> bit) & 1
        };
        let carry = remainder >> 127;
        remainder = (remainder << 1) | next_bit;
        if carry == 1 || remainder >= denominator {
            remainder = remainder.wrapping_sub(denominator);
            if bit >= 128 {
                return None;
            }
            quotient |= 1u128 << bit;
        }
    }

    Some((quotient, remainder != 0))
}

fn mul_div_ceil(lhs: u128, rhs: u128, denominator: u128) -> Option<u128> {
    let (quotient, has_remainder) = mul_div(lhs, rhs, denominator)?;
    quotient.checked_add(u128::from(has_remainder))
}

fn widening_mul(lhs: u128, rhs: u128) -> (u128, u128) {
    const LOW_64: u128 = u64::MAX as u128;
    let (lhs_low, lhs_high) = (lhs & LOW_64, lhs >> 64);
    let (rhs_low, rhs_high) = (rhs & LOW_64, rhs >> 64);

    let low_low = lhs_low * rhs_low;
    let low_high = lhs_low * rhs_high;
    let high_low = lhs_high * rhs_low;
    let high_high = lhs_high * rhs_high;

    let mut low = low_low;
    let mut high = high_high;

    let (sum, carried) = low.overflowing_add(low_high << 64);
    low = sum;
    high += (low_high >> 64) + u128::from(carried);

    let (sum, carried) = low.overflowing_add(high_low << 64);
    low = sum;
    high += (high_low >> 64) + u128::from(carried);

    (high, low)
}
