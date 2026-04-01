//! Blend V2 strategy — adapted from Palta DeFindex `apps/contracts/strategies/blend`.
//!
//! Deposits pull CETES (or configured `asset`) from the vault via `TokenClient::transfer`
//! inside `deposit` (vault must authorize). Underlying is supplied to Blend via `submit`;
//! per-vault accounting uses internal shares (`reserves` + `storage`).

use defindex_strategy_core::{
    event::{emit_deposit, emit_harvest, emit_withdraw},
    StrategyError,
};
use soroban_sdk::{
    contract, contractimpl,
    token::TokenClient,
    Address, Bytes, Env, String, TryIntoVal, Val, Vec,
};

use crate::blend_pool;
use crate::constants::SCALAR_12;
use crate::reserves;
use crate::storage::{
    self, extend_instance_ttl, Config,
};
use crate::utils::{
    calculate_optimal_deposit_amount, calculate_optimal_withdraw_amount, shares_to_underlying,
};

const STRATEGY_NAME: &str = "BlendStrategy";

#[contract]
pub struct BlendStrategy;

fn check_positive_amount(amount: i128) -> Result<(), StrategyError> {
    if amount <= 0 {
        Err(StrategyError::OnlyPositiveAmountAllowed)
    } else {
        Ok(())
    }
}

#[contractimpl]
impl BlendStrategy {
    /// `init_args`: `[blend_pool, blend_token, soroswap_router, reward_threshold, keeper]`
    pub fn __constructor(env: Env, asset: Address, init_args: Vec<Val>) {
        if storage::is_initialized(&env) {
            soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument);
        }

        let blend_pool_address: Address = init_args
            .get(0)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument))
            .try_into_val(&env)
            .unwrap_or_else(|_| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument));
        let blend_token: Address = init_args
            .get(1)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument))
            .try_into_val(&env)
            .unwrap_or_else(|_| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument));
        let soroswap_router: Address = init_args
            .get(2)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument))
            .try_into_val(&env)
            .unwrap_or_else(|_| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument));
        let reward_threshold: i128 = init_args
            .get(3)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument))
            .try_into_val(&env)
            .unwrap_or_else(|_| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument));
        let keeper: Address = init_args
            .get(4)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument))
            .try_into_val(&env)
            .unwrap_or_else(|_| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument));

        let blend_pool_client = blend_pool::BlendPoolClient::new(&env, &blend_pool_address);
        let reserve_id = blend_pool_client.get_reserve(&asset).config.index;
        let claim_id = reserve_id * 2 + 1;
        let claim_ids: Vec<u32> = soroban_sdk::vec![&env, claim_id];

        check_positive_amount(reward_threshold).unwrap_or_else(|_| {
            soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument)
        });

        let config = Config {
            asset: asset.clone(),
            pool: blend_pool_address,
            reserve_id,
            blend_token,
            router: soroswap_router,
            claim_ids,
            reward_threshold,
        };

        storage::set_config(&env, config);
        storage::set_keeper(&env, &keeper);
    }

    pub fn asset(env: Env) -> Result<Address, StrategyError> {
        extend_instance_ttl(&env);
        Ok(storage::get_config(&env)?.asset)
    }

    pub fn deposit(env: Env, amount: i128, from: Address) -> Result<i128, StrategyError> {
        extend_instance_ttl(&env);

        check_positive_amount(amount)?;
        from.require_auth();

        let config = storage::get_config(&env)?;
        let reserves = reserves::get_strategy_reserve_updated(&env, &config);
        let (optimal_deposit_amount, b_tokens_minted) =
            calculate_optimal_deposit_amount(amount, &reserves)?;

        let token_client = TokenClient::new(&env, &config.asset);
        token_client.transfer(
            &from,
            &env.current_contract_address(),
            &amount,
        );
        if amount != optimal_deposit_amount {
            token_client.transfer(
                &env.current_contract_address(),
                &from,
                &(amount - optimal_deposit_amount),
            );
        }

        blend_pool::supply(&env, &from, &optimal_deposit_amount, &config, false)?;

        let (vault_shares, reserves) =
            reserves::deposit(&env, &from, b_tokens_minted, &reserves)?;

        let underlying_balance = shares_to_underlying(vault_shares, reserves)?;

        emit_deposit(
            &env,
            String::from_str(&env, STRATEGY_NAME),
            optimal_deposit_amount,
            from,
        );
        Ok(underlying_balance)
    }

    pub fn harvest(env: Env, from: Address, data: Option<Bytes>) -> Result<(), StrategyError> {
        extend_instance_ttl(&env);

        let keeper = storage::get_keeper(&env)?;
        keeper.require_auth();

        if from != keeper {
            return Err(StrategyError::NotAuthorized);
        }

        let config = storage::get_config(&env)?;

        let harvested_blend =
            blend_pool::claim(&env, &env.current_contract_address(), &config);

        let amount_out_min: i128 = match &data {
            Some(bytes) if !bytes.is_empty() => {
                let mut slice = [0u8; 16];
                bytes.copy_into_slice(&mut slice);
                i128::from_be_bytes(slice)
            }
            _ => 0,
        };

        let reserves_after = blend_pool::perform_reinvest(&env, &config, amount_out_min)?;

        let price_per_share = if reserves_after.total_shares > 0 {
            let total_u = shares_to_underlying(reserves_after.total_shares, reserves_after.clone())?;
            total_u
                .checked_mul(SCALAR_12)
                .and_then(|x| x.checked_div(reserves_after.total_shares))
                .unwrap_or(0)
        } else {
            0
        };

        emit_harvest(
            &env,
            String::from_str(&env, STRATEGY_NAME),
            harvested_blend,
            keeper,
            price_per_share,
        );
        Ok(())
    }

    pub fn withdraw(
        env: Env,
        amount: i128,
        from: Address,
        to: Address,
    ) -> Result<i128, StrategyError> {
        extend_instance_ttl(&env);

        check_positive_amount(amount)?;
        from.require_auth();

        let config = storage::get_config(&env)?;
        let reserves = reserves::get_strategy_reserve_updated(&env, &config);
        let (optimal_withdraw_amount, b_tokens_burnt) =
            calculate_optimal_withdraw_amount(amount, &reserves)?;

        blend_pool::withdraw(&env, &to, &optimal_withdraw_amount, &config)?;

        let (vault_shares, reserves) =
            reserves::withdraw(&env, &from, b_tokens_burnt, &reserves)?;
        let underlying_balance = shares_to_underlying(vault_shares, reserves)?;

        emit_withdraw(
            &env,
            String::from_str(&env, STRATEGY_NAME),
            optimal_withdraw_amount,
            from,
        );
        Ok(underlying_balance)
    }

    pub fn balance(env: Env, from: Address) -> Result<i128, StrategyError> {
        extend_instance_ttl(&env);

        let vault_shares = storage::get_vault_shares(&env, &from);
        if vault_shares > 0 {
            let config = storage::get_config(&env)?;
            let reserves = reserves::get_strategy_reserve_updated(&env, &config);
            Ok(shares_to_underlying(vault_shares, reserves)?)
        } else {
            Ok(0)
        }
    }

    pub fn set_keeper(env: Env, new_keeper: Address) -> Result<(), StrategyError> {
        extend_instance_ttl(&env);

        let old_keeper = storage::get_keeper(&env)?;
        old_keeper.require_auth();

        storage::set_keeper(&env, &new_keeper);
        Ok(())
    }

    pub fn get_keeper(env: Env) -> Result<Address, StrategyError> {
        extend_instance_ttl(&env);
        storage::get_keeper(&env)
    }
}
