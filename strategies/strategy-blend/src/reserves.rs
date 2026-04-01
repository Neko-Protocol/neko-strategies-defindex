use crate::{
    blend_pool,
    constants::SCALAR_12,
    storage::{self, Config},
};

use defindex_strategy_core::StrategyError;
use soroban_fixed_point_math::FixedPoint;
use soroban_sdk::{contracttype, panic_with_error, Address, Env};

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StrategyReserves {
    pub total_shares: i128,
    pub total_b_tokens: i128,
    pub b_rate: i128,
}

impl StrategyReserves {
    pub fn b_tokens_to_shares_down(&self, amount: i128) -> Result<i128, StrategyError> {
        if self.total_shares == 0 || self.total_b_tokens == 0 {
            return Ok(amount);
        }
        amount
            .fixed_mul_floor(self.total_shares, self.total_b_tokens)
            .ok_or(StrategyError::ArithmeticError)
    }

    pub fn b_tokens_to_shares_up(&self, amount: i128) -> Result<i128, StrategyError> {
        if self.total_shares == 0 || self.total_b_tokens == 0 {
            return Ok(amount);
        }
        amount
            .fixed_mul_ceil(self.total_shares, self.total_b_tokens)
            .ok_or(StrategyError::ArithmeticError)
    }

    pub fn shares_to_b_tokens_down(&self, amount: i128) -> Result<i128, StrategyError> {
        amount
            .fixed_div_floor(self.total_shares, self.total_b_tokens)
            .ok_or(StrategyError::DivisionByZero)
    }

    pub fn b_tokens_to_underlying_down(&self, amount: i128) -> Result<i128, StrategyError> {
        amount
            .fixed_mul_floor(self.b_rate, SCALAR_12)
            .ok_or(StrategyError::ArithmeticError)
    }

    pub fn update_rate(&mut self, e: &Env, config: &Config) {
        self.b_rate = blend_pool::reserve_b_rate(e, config);
    }
}

pub fn get_strategy_reserve_updated(e: &Env, config: &Config) -> StrategyReserves {
    let mut reserve = storage::get_strategy_reserves(e);
    reserve.update_rate(e, config);
    reserve
}

pub fn set_validated_vault_shares(
    e: &Env,
    from: &Address,
    vault_shares: i128,
) -> Result<i128, StrategyError> {
    if vault_shares >= 0 {
        storage::set_vault_shares(e, from, vault_shares);
        Ok(vault_shares)
    } else {
        Err(StrategyError::OnlyPositiveAmountAllowed)
    }
}

pub fn deposit(
    e: &Env,
    from: &Address,
    b_tokens_amount: i128,
    reserves: &StrategyReserves,
) -> Result<(i128, StrategyReserves), StrategyError> {
    let mut reserves = reserves.clone();
    if b_tokens_amount <= 0 {
        return Err(StrategyError::BTokensAmountBelowMin);
    }

    let old_vault_shares = storage::get_vault_shares(e, from);

    let new_minted_shares: i128 = reserves.b_tokens_to_shares_down(b_tokens_amount)?;

    if new_minted_shares <= 0 {
        panic_with_error!(e, StrategyError::InvalidSharesMinted);
    }

    let new_vault_minted_shares = if reserves.total_shares == 0 {
        if new_minted_shares <= 1000 {
            panic_with_error!(e, StrategyError::InvalidSharesMinted);
        }
        new_minted_shares
            .checked_sub(1000)
            .ok_or(StrategyError::UnderflowOverflow)?
    } else {
        new_minted_shares
    };

    reserves.total_shares = reserves
        .total_shares
        .checked_add(new_minted_shares)
        .ok_or(StrategyError::UnderflowOverflow)?;
    reserves.total_b_tokens = reserves
        .total_b_tokens
        .checked_add(b_tokens_amount)
        .ok_or(StrategyError::UnderflowOverflow)?;

    let new_vault_shares = old_vault_shares
        .checked_add(new_vault_minted_shares)
        .ok_or(StrategyError::UnderflowOverflow)?;

    storage::set_strategy_reserves(e, reserves.clone());
    set_validated_vault_shares(e, from, new_vault_shares)?;
    Ok((new_vault_shares, reserves))
}

pub fn withdraw(
    e: &Env,
    from: &Address,
    b_tokens_amount: i128,
    reserves: &StrategyReserves,
) -> Result<(i128, StrategyReserves), StrategyError> {
    let mut reserves = reserves.clone();

    if b_tokens_amount <= 0 {
        return Err(StrategyError::BTokensAmountBelowMin);
    }

    let mut vault_shares = storage::get_vault_shares(e, from);
    let share_amount = reserves.b_tokens_to_shares_up(b_tokens_amount)?;

    if reserves.total_shares < share_amount || reserves.total_b_tokens < b_tokens_amount {
        return Err(StrategyError::InsufficientBalance);
    }

    reserves.total_shares = reserves
        .total_shares
        .checked_sub(share_amount)
        .ok_or(StrategyError::UnderflowOverflow)?;
    reserves.total_b_tokens = reserves
        .total_b_tokens
        .checked_sub(b_tokens_amount)
        .ok_or(StrategyError::UnderflowOverflow)?;

    if share_amount > vault_shares {
        return Err(StrategyError::InsufficientBalance);
    }

    vault_shares = vault_shares
        .checked_sub(share_amount)
        .ok_or(StrategyError::UnderflowOverflow)?;

    storage::set_strategy_reserves(e, reserves.clone());
    set_validated_vault_shares(e, from, vault_shares)?;
    Ok((vault_shares, reserves))
}

pub fn harvest(
    e: &Env,
    b_tokens_amount: i128,
    config: &Config,
) -> Result<StrategyReserves, StrategyError> {
    let mut reserves = get_strategy_reserve_updated(e, config);

    if b_tokens_amount <= 0 {
        panic_with_error!(e, StrategyError::BTokensAmountBelowMin);
    }

    reserves.total_b_tokens = reserves
        .total_b_tokens
        .checked_add(b_tokens_amount)
        .ok_or(StrategyError::UnderflowOverflow)?;

    storage::set_strategy_reserves(e, reserves.clone());

    Ok(reserves)
}
