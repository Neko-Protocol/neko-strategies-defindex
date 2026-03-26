/// DeFindex strategy — Soroswap AMM
///
/// Single-asset (token_a) entry into a Soroswap pair. On deposit the strategy
/// swaps half to token_b and provides liquidity; on withdraw it removes
/// liquidity and swaps token_b back to token_a.
///
/// ## Multi-vault design
///
/// The strategy contract holds all LP tokens. Each vault's share is tracked
/// via per-vault LP attribution in persistent storage.
///
/// ## Auth flow — deposit
///
///   vault → token_a.transfer(vault, strategy, amount)    [vault self-auth]
///   vault → strategy.deposit(amount, vault)
///     strategy → authorize_as_current_contract([token_a.transfer(strategy→pair, swap)])
///     strategy → router.swap_exact_tokens_for_tokens(...)  → b_received
///     strategy → authorize_as_current_contract([token_a.transfer, token_b.transfer])
///     strategy → router.add_liquidity(...)                → lp_minted
///     storage: lp_tokens[vault] += lp_minted
///
/// ## Auth flow — withdraw
///
///   vault → strategy.withdraw(amount, vault, vault)
///     strategy → authorize_as_current_contract([pair.transfer(strategy→pair, lp_to_burn)])
///     strategy → router.remove_liquidity(...)             → (a_out, b_out)
///     strategy → authorize_as_current_contract([token_b.transfer(strategy→pair, b_out)])
///     strategy → router.swap_exact_tokens_for_tokens(b→a)
///     strategy → token_a.transfer(strategy, to, total_a)
///     storage: lp_tokens[vault] -= lp_to_burn
use defindex_strategy_core::{
    event::{emit_deposit, emit_harvest, emit_withdraw},
    StrategyError,
};
use soroban_sdk::{
    contract, contractimpl, Address, Bytes, Env, String, TryIntoVal, Val, Vec,
};

use crate::soroswap_pool;
use crate::soroswap_router;
use crate::storage::{
    get_lp_tokens, is_initialized, load_config, save_config, set_lp_tokens, StrategyConfig,
};

const STRATEGY_NAME: &str = "SoroswapStrategy";

#[contract]
pub struct SoroswapStrategy;

#[contractimpl]
impl SoroswapStrategy {
    // ═══════════════════════════════════════════════════════════════════════
    // Constructor
    // ═══════════════════════════════════════════════════════════════════════

    /// Called once at deployment.
    ///
    /// # Arguments
    /// * `asset`     — token_a address (single-asset deposit token, e.g. USDC)
    /// * `init_args` — `[router: Address, token_b: Address]`
    ///
    /// The pair address is resolved automatically from the router.
    pub fn __constructor(env: Env, asset: Address, init_args: Vec<Val>) {
        if is_initialized(&env) {
            soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument);
        }

        let router: Address = init_args
            .get(0)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument))
            .try_into_val(&env)
            .unwrap_or_else(|_| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument));

        let token_b: Address = init_args
            .get(1)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument))
            .try_into_val(&env)
            .unwrap_or_else(|_| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument));

        // Resolve pair address from router
        let router_client = soroswap_router::RouterClient::new(&env, &router);
        let pair = router_client.router_pair_for(&asset, &token_b);

        save_config(&env, &StrategyConfig { token_a: asset, token_b, router, pair });
    }

    // ═══════════════════════════════════════════════════════════════════════
    // DeFindexStrategyTrait
    // ═══════════════════════════════════════════════════════════════════════

    /// Returns token_a — the underlying asset depositors interact with.
    pub fn asset(env: Env) -> Result<Address, StrategyError> {
        Ok(load_config(&env).token_a)
    }

    /// Deposit `amount` of token_a, entering the Soroswap pair as LP.
    ///
    /// Pre-condition: the vault has already transferred `amount` token_a to
    /// this strategy contract before calling `deposit`.
    ///
    /// Returns the vault's updated position value in token_a units.
    pub fn deposit(env: Env, amount: i128, from: Address) -> Result<i128, StrategyError> {
        if amount <= 0 {
            return Err(StrategyError::OnlyPositiveAmountAllowed);
        }

        let config = load_config(&env);

        // Swap half + add liquidity. Returns net LP tokens minted.
        let lp_minted = soroswap_pool::deposit(&env, amount, &config);

        // Attribute the new LP tokens to this vault.
        let prev_lp = get_lp_tokens(&env, &from);
        set_lp_tokens(&env, &from, prev_lp + lp_minted);

        emit_deposit(
            &env,
            String::from_str(&env, STRATEGY_NAME),
            amount,
            from.clone(),
        );

        Ok(soroswap_pool::lp_value_in_a(&env, get_lp_tokens(&env, &from), &config))
    }

    /// Withdraw `amount` of token_a from the pair, burning proportional LP tokens.
    ///
    /// Returns the vault's remaining position value in token_a units.
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

        let vault_lp = get_lp_tokens(&env, &from);
        if vault_lp == 0 {
            return Err(StrategyError::InsufficientBalance);
        }

        let total_value = soroswap_pool::lp_value_in_a(&env, vault_lp, &config);
        if total_value == 0 {
            return Err(StrategyError::InsufficientBalance);
        }

        // Proportional LP burn: lp_to_burn = vault_lp * amount / total_value
        let lp_to_burn = if amount >= total_value {
            vault_lp
        } else {
            vault_lp
                .checked_mul(amount)
                .ok_or(StrategyError::ArithmeticError)?
                .checked_div(total_value)
                .ok_or(StrategyError::ArithmeticError)?
        };

        if lp_to_burn == 0 {
            return Err(StrategyError::InsufficientBalance);
        }

        let actual_withdrawn = soroswap_pool::withdraw(&env, lp_to_burn, &to, &config);

        set_lp_tokens(&env, &from, vault_lp - lp_to_burn);

        emit_withdraw(
            &env,
            String::from_str(&env, STRATEGY_NAME),
            actual_withdrawn,
            from.clone(),
        );

        Ok(soroswap_pool::lp_value_in_a(&env, get_lp_tokens(&env, &from), &config))
    }

    /// Returns the vault's current LP position value in token_a units.
    ///
    /// value = share_of_reserve_a + (share_of_reserve_b × spot_price_b_in_a)
    pub fn balance(env: Env, from: Address) -> Result<i128, StrategyError> {
        let config = load_config(&env);
        let vault_lp = get_lp_tokens(&env, &from);
        Ok(soroswap_pool::lp_value_in_a(&env, vault_lp, &config))
    }

    /// Harvest — Soroswap fees accrue into reserves and are realized on LP removal.
    ///
    /// Emits a `HarvestEvent` with the current token_a value per LP token as
    /// `price_per_share`, enabling DeFindex to track fee-based APY over time.
    pub fn harvest(env: Env, from: Address, _data: Option<Bytes>) -> Result<(), StrategyError> {
        let config = load_config(&env);
        let vault_lp = get_lp_tokens(&env, &from);

        // price_per_share = total_value / vault_lp (in token_a units, scaled ×1e7)
        let price_per_share = if vault_lp > 0 {
            let total_value = soroswap_pool::lp_value_in_a(&env, vault_lp, &config);
            total_value
                .checked_mul(10_000_000)
                .unwrap_or(0)
                .checked_div(vault_lp)
                .unwrap_or(0)
        } else {
            0
        };

        emit_harvest(
            &env,
            String::from_str(&env, STRATEGY_NAME),
            0, // fees are embedded in reserves, no explicit claim
            from,
            price_per_share,
        );

        Ok(())
    }
}
