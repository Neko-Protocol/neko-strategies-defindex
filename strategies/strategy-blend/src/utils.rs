use defindex_strategy_core::StrategyError;
use soroban_fixed_point_math::FixedPoint;

use crate::{constants::SCALAR_12, reserves::StrategyReserves};

pub fn shares_to_underlying(
    shares: i128,
    reserves: StrategyReserves,
) -> Result<i128, StrategyError> {
    let total_shares = reserves.total_shares;
    let total_b_tokens = reserves.total_b_tokens;

    if total_shares == 0 || total_b_tokens == 0 {
        return Ok(0i128);
    }
    let vault_b_tokens = reserves.shares_to_b_tokens_down(shares)?;
    reserves.b_tokens_to_underlying_down(vault_b_tokens)
}

pub fn calculate_optimal_deposit_amount(
    deposit_amount: i128,
    reserves: &StrategyReserves,
) -> Result<(i128, i128), StrategyError> {
    let b_tokens_minted = deposit_amount
        .fixed_mul_floor(SCALAR_12, reserves.b_rate)
        .ok_or(StrategyError::ArithmeticError)?;

    let optimal_b_token_amount = if reserves.total_shares == 0 {
        b_tokens_minted
    } else {
        let shares_minted = reserves.b_tokens_to_shares_down(b_tokens_minted)?;
        if shares_minted == 0 {
            return Err(StrategyError::InvalidSharesMinted);
        }
        shares_minted
            .fixed_mul_ceil(reserves.total_b_tokens, reserves.total_shares)
            .ok_or(StrategyError::ArithmeticError)?
    };

    if optimal_b_token_amount <= 0 {
        return Err(StrategyError::BTokensAmountBelowMin);
    }

    let optimal_deposit_amt = optimal_b_token_amount
        .fixed_mul_ceil(reserves.b_rate, SCALAR_12)
        .ok_or(StrategyError::ArithmeticError)?;

    Ok((optimal_deposit_amt, optimal_b_token_amount))
}

pub fn calculate_optimal_withdraw_amount(
    withdraw_amount: i128,
    reserves: &StrategyReserves,
) -> Result<(i128, i128), StrategyError> {
    let b_tokens_burnt = withdraw_amount
        .fixed_mul_ceil(SCALAR_12, reserves.b_rate)
        .ok_or(StrategyError::ArithmeticError)?;
    let shares_burnt = reserves.b_tokens_to_shares_up(b_tokens_burnt)?;
    let optimal_b_tokens = shares_burnt
        .fixed_mul_floor(reserves.total_b_tokens, reserves.total_shares)
        .ok_or(StrategyError::ArithmeticError)?;
    let optimal_withdraw_amount = optimal_b_tokens
        .fixed_mul_floor(reserves.b_rate, SCALAR_12)
        .ok_or(StrategyError::ArithmeticError)?;
    Ok((optimal_withdraw_amount, optimal_b_tokens))
}
