/// DeFindex strategy — Aquarius AMM
///
/// Single-asset (deposit_token) entry into an Aquarius pool. On deposit the
/// strategy swaps half to pair_token and adds liquidity; on withdraw it removes
/// liquidity and swaps pair_token back. AQUA reward tokens are harvested
/// explicitly via `harvest()`.
///
/// ## Multi-vault design
///
/// The strategy holds all LP share tokens. Each vault's share is tracked
/// via per-vault attribution in persistent storage.
///
/// ## Key difference from Soroswap strategy
///
/// Aquarius has an explicit `pool.claim()` that returns AQUA reward tokens,
/// making `harvest()` meaningful — it both claims AQUA and emits a HarvestEvent
/// with AQUA amount as the APY signal for DeFindex.
use defindex_strategy_core::{
    event::{emit_deposit, emit_harvest, emit_withdraw},
    StrategyError,
};
use soroban_sdk::{
    contract, contractimpl,
    token::TokenClient,
    Address, Bytes, Env, String, TryIntoVal, Val, Vec,
};

use crate::aquarius_pool;
use crate::storage::{
    get_shares, is_initialized, load_config, save_config, set_shares, validate_slippage,
    StrategyConfig,
};

const STRATEGY_NAME: &str = "AquariusStrategy";

#[contract]
pub struct AquariusStrategy;

#[contractimpl]
impl AquariusStrategy {
    // ═══════════════════════════════════════════════════════════════════════
    // Constructor
    // ═══════════════════════════════════════════════════════════════════════

    /// Called once at deployment.
    ///
    /// # Arguments
    /// * `asset`     — deposit_token address (single-asset entry, e.g. CETES)
    /// * `init_args` — `[pool: Address, pair_token: Address, aqua_token: Address, max_slippage_bps: u32]`
    ///
    /// Pool token indices and the share_token address are resolved automatically
    /// from the pool contract.
    pub fn __constructor(env: Env, asset: Address, init_args: Vec<Val>) {
        if is_initialized(&env) {
            soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument);
        }

        let pool: Address = init_args
            .get(0)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument))
            .try_into_val(&env)
            .unwrap_or_else(|_| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument));

        let pair_token: Address = init_args
            .get(1)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument))
            .try_into_val(&env)
            .unwrap_or_else(|_| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument));

        let aqua_token: Address = init_args
            .get(2)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument))
            .try_into_val(&env)
            .unwrap_or_else(|_| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument));

        let max_slippage_bps: u32 = init_args
            .get(3)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument))
            .try_into_val(&env)
            .unwrap_or_else(|_| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument));

        validate_slippage(&env, max_slippage_bps);

        // Resolve token indices and share_token from the pool contract.
        let pool_client = crate::aquarius_pool_contract::PoolClient::new(&env, &pool);
        let tokens      = pool_client.get_tokens();
        let share_token = pool_client.share_id();

        let admin = env.current_contract_address();

        let (deposit_token_idx, pair_token_idx) = resolve_indices(&env, &tokens, &asset, &pair_token);

        save_config(&env, &StrategyConfig {
            deposit_token: asset,
            deposit_token_idx,
            pair_token,
            pair_token_idx,
            pool,
            share_token,
            aqua_token,
            max_slippage_bps,
            admin,
        });
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Admin
    // ═══════════════════════════════════════════════════════════════════════

    /// Update slippage tolerance. Admin-only.
    pub fn update_slippage(env: Env, admin: Address, new_slippage_bps: u32) {
        admin.require_auth();
        let mut config = load_config(&env);
        if admin != config.admin {
            soroban_sdk::panic_with_error!(&env, StrategyError::NotAuthorized);
        }
        validate_slippage(&env, new_slippage_bps);
        config.max_slippage_bps = new_slippage_bps;
        save_config(&env, &config);
    }

    /// Sweep stuck tokens to `to`. Admin-only.
    ///
    /// Tokens can accumulate when add_liquidity refunds excess pair_token due to
    /// reserve ratio drift between the swap and deposit steps.
    pub fn sweep(env: Env, admin: Address, token: Address, to: Address, amount: i128) {
        admin.require_auth();
        let config = load_config(&env);
        if admin != config.admin {
            soroban_sdk::panic_with_error!(&env, StrategyError::NotAuthorized);
        }
        TokenClient::new(&env, &token).transfer(&env.current_contract_address(), &to, &amount);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // DeFindexStrategyTrait
    // ═══════════════════════════════════════════════════════════════════════

    /// Returns deposit_token — the underlying asset depositors interact with.
    pub fn asset(env: Env) -> Result<Address, StrategyError> {
        Ok(load_config(&env).deposit_token)
    }

    /// Deposit `amount` of deposit_token, entering the Aquarius pool as LP.
    ///
    /// Pre-condition: the vault has already transferred `amount` deposit_token to
    /// this strategy contract before calling `deposit`.
    ///
    /// Returns the vault's updated position value in deposit_token units.
    pub fn deposit(env: Env, amount: i128, from: Address) -> Result<i128, StrategyError> {
        if amount <= 0 {
            return Err(StrategyError::OnlyPositiveAmountAllowed);
        }

        let config = load_config(&env);

        // Returns the delta of LP share tokens minted for this deposit.
        let shares_minted = aquarius_pool::deposit(&env, amount, &config);

        let prev = get_shares(&env, &from);
        set_shares(&env, &from, prev + shares_minted);

        emit_deposit(
            &env,
            String::from_str(&env, STRATEGY_NAME),
            amount,
            from.clone(),
        );

        Ok(aquarius_pool::shares_value_in_deposit(
            &env,
            get_shares(&env, &from),
            &config,
        ))
    }

    /// Withdraw `amount` of deposit_token from the pool, burning proportional LP shares.
    ///
    /// Returns the vault's remaining position value in deposit_token units.
    pub fn withdraw(
        env: Env,
        amount: i128,
        from: Address,
        to: Address,
    ) -> Result<i128, StrategyError> {
        if amount <= 0 {
            return Err(StrategyError::OnlyPositiveAmountAllowed);
        }

        let config = load_config(&env);

        let vault_shares = get_shares(&env, &from);
        if vault_shares == 0 {
            return Err(StrategyError::InsufficientBalance);
        }

        let total_value =
            aquarius_pool::shares_value_in_deposit(&env, vault_shares, &config);
        if total_value == 0 {
            return Err(StrategyError::InsufficientBalance);
        }

        // Proportional burn: shares_to_burn = vault_shares * amount / total_value
        let shares_to_burn = if amount >= total_value {
            vault_shares
        } else {
            vault_shares
                .checked_mul(amount)
                .ok_or(StrategyError::ArithmeticError)?
                .checked_div(total_value)
                .ok_or(StrategyError::ArithmeticError)?
        };

        if shares_to_burn == 0 {
            return Err(StrategyError::InsufficientBalance);
        }

        let actual_withdrawn = aquarius_pool::withdraw(&env, shares_to_burn, &to, &config);

        set_shares(&env, &from, vault_shares - shares_to_burn);

        emit_withdraw(
            &env,
            String::from_str(&env, STRATEGY_NAME),
            actual_withdrawn,
            from.clone(),
        );

        Ok(aquarius_pool::shares_value_in_deposit(
            &env,
            get_shares(&env, &from),
            &config,
        ))
    }

    /// Returns the vault's current LP position value in deposit_token units.
    pub fn balance(env: Env, from: Address) -> Result<i128, StrategyError> {
        let config = load_config(&env);
        let vault_shares = get_shares(&env, &from);
        Ok(aquarius_pool::shares_value_in_deposit(&env, vault_shares, &config))
    }

    /// Harvest AQUA rewards and emit a HarvestEvent.
    ///
    /// Unlike rwa-lending and Soroswap, Aquarius has an explicit reward token (AQUA)
    /// that accumulates per-block and is claimable via pool.claim(). The AQUA amount
    /// is forwarded to `from` (the vault) and emitted as `amount` in HarvestEvent,
    /// letting DeFindex track real yield in addition to fee-based LP appreciation.
    ///
    /// `price_per_share` = current deposit_token value per LP share (×1e7 scaled).
    pub fn harvest(env: Env, from: Address, _data: Option<Bytes>) -> Result<(), StrategyError> {
        let config = load_config(&env);

        let aqua_harvested = aquarius_pool::claim(&env, &from, &config);

        let vault_shares = get_shares(&env, &from);
        let price_per_share = if vault_shares > 0 {
            let total_value =
                aquarius_pool::shares_value_in_deposit(&env, vault_shares, &config);
            total_value
                .checked_mul(10_000_000)
                .unwrap_or(0)
                .checked_div(vault_shares)
                .unwrap_or(0)
        } else {
            0
        };

        emit_harvest(
            &env,
            String::from_str(&env, STRATEGY_NAME),
            aqua_harvested, // explicit AQUA reward amount
            from,
            price_per_share,
        );

        Ok(())
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn resolve_indices(
    env: &Env,
    tokens: &soroban_sdk::Vec<Address>,
    deposit_token: &Address,
    pair_token: &Address,
) -> (u32, u32) {
    let mut deposit_idx = u32::MAX;
    let mut pair_idx    = u32::MAX;

    for i in 0..tokens.len() {
        let t = tokens.get(i).unwrap();
        if t == *deposit_token { deposit_idx = i; }
        if t == *pair_token    { pair_idx    = i; }
    }

    if deposit_idx == u32::MAX || pair_idx == u32::MAX {
        soroban_sdk::panic_with_error!(env, StrategyError::ProtocolAddressNotFound);
    }

    (deposit_idx, pair_idx)
}
