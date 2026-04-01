/// DeFindex strategy — rwa-lending
///
/// Implements `DeFindexStrategyTrait` so any DeFindex vault can allocate
/// funds into Neko's rwa-lending pools and earn yield from RWA lending.
///
/// ## Multi-vault design
///
/// Multiple DeFindex vaults can share the same deployed strategy contract.
/// The strategy contract is the lender in rwa-lending (all deposits go under
/// `env.current_contract_address()`). Each vault's position is tracked via
/// b_token attribution in persistent storage.
///
/// ## Auth flow — deposit
///
///   vault → token.transfer(vault, strategy, amount)        [vault self-auth]
///   vault → strategy.deposit(amount, vault)
///     strategy → authorize_as_current_contract([token.transfer(strategy→lending, amount)])
///     strategy → lending.deposit(strategy, asset, amount)
///       lending: lender.require_auth()                     [strategy is invoker → PASS]
///       lending: token.transfer(strategy, lending, amount) [pre-authorized → PASS]
///       lending: mints b_tokens to strategy's position
///
/// ## Auth flow — withdraw
///
///   vault → strategy.withdraw(amount, vault, vault)
///     strategy → authorize_as_current_contract([token.transfer(lending→strategy, underlying)])
///     strategy → lending.withdraw(strategy, asset, b_tokens)
///     strategy → token.transfer(strategy, to, actual_withdrawn)
use defindex_strategy_core::{
    event::{emit_deposit, emit_harvest, emit_withdraw},
    StrategyError,
};
use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    contract, contractimpl,
    token::TokenClient,
    Address, Bytes, Env, IntoVal, String, TryIntoVal, Val, Vec,
};

use crate::neko_pool;
use crate::storage::{
    get_b_tokens, is_initialized, load_config, save_config, set_b_tokens, StrategyConfig,
    SCALAR_12,
};

const STRATEGY_NAME: &str = "NekoStrategy";

#[contract]
pub struct NekoStrategy;

#[contractimpl]
impl NekoStrategy {
    // ═══════════════════════════════════════════════════════════════════════
    // Constructor
    // ═══════════════════════════════════════════════════════════════════════

    /// Called once at deployment by the DeFindex Factory.
    ///
    /// # Arguments
    /// * `asset`     — deposit token address (e.g. the CETES token contract)
    /// * `init_args` — `[lending_pool: Address, rwa_asset: Symbol]`
    ///
    /// # Panics
    /// Panics with `StrategyError::InvalidArgument` if already initialized or
    /// if `init_args` is missing/malformed.
    pub fn __constructor(env: Env, asset: Address, init_args: Vec<Val>) {
        if is_initialized(&env) {
            soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument);
        }

        let lending_pool: Address = init_args
            .get(0)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument))
            .try_into_val(&env)
            .unwrap_or_else(|_| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument));

        let rwa_asset = init_args
            .get(1)
            .unwrap_or_else(|| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument))
            .try_into_val(&env)
            .unwrap_or_else(|_| soroban_sdk::panic_with_error!(&env, StrategyError::InvalidArgument));

        save_config(&env, &StrategyConfig { asset, lending_pool, rwa_asset });
    }

    // ═══════════════════════════════════════════════════════════════════════
    // DeFindexStrategyTrait
    // ═══════════════════════════════════════════════════════════════════════

    /// Returns the underlying asset managed by this strategy.
    pub fn asset(env: Env) -> Result<Address, StrategyError> {
        Ok(load_config(&env).asset)
    }

    /// Deposit `amount` tokens into rwa-lending on behalf of `from` (the vault).
    ///
    /// Pre-condition: the vault has already transferred `amount` tokens to this
    /// strategy contract before calling `deposit`.
    ///
    /// Returns the vault's updated balance in underlying token units.
    pub fn deposit(env: Env, amount: i128, from: Address) -> Result<i128, StrategyError> {
        if amount <= 0 {
            return Err(StrategyError::OnlyPositiveAmountAllowed);
        }

        let config = load_config(&env);
        let strategy_addr = env.current_contract_address();

        // Pre-authorize token.transfer(strategy → lending_pool, amount)
        // which lending.deposit() will execute internally.
        env.authorize_as_current_contract(soroban_sdk::vec![
            &env,
            InvokerContractAuthEntry::Contract(SubContractInvocation {
                context: ContractContext {
                    contract: config.asset.clone(),
                    fn_name: soroban_sdk::symbol_short!("transfer"),
                    args: soroban_sdk::vec![
                        &env,
                        strategy_addr.clone().into_val(&env),
                        config.lending_pool.clone().into_val(&env),
                        amount.into_val(&env),
                    ],
                },
                sub_invocations: soroban_sdk::vec![&env],
            }),
        ]);

        let lending = neko_pool::Client::new(&env, &config.lending_pool);
        let b_tokens_minted = lending.deposit(&strategy_addr, &config.rwa_asset, &amount);

        // Attribute the minted b_tokens to this vault.
        let prev_b = get_b_tokens(&env, &from);
        set_b_tokens(&env, &from, prev_b + b_tokens_minted);

        emit_deposit(
            &env,
            String::from_str(&env, STRATEGY_NAME),
            amount,
            from.clone(),
        );

        Ok(underlying_balance(&env, &from, &config))
    }

    /// Withdraw `amount` underlying tokens from rwa-lending and send them to `to`.
    ///
    /// `from` identifies the vault whose position is reduced.
    /// Returns the vault's remaining balance in underlying token units.
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
        let strategy_addr = env.current_contract_address();
        let lending = neko_pool::Client::new(&env, &config.lending_pool);

        let b_rate = lending.get_b_token_rate(&config.rwa_asset);
        if b_rate == 0 {
            return Err(StrategyError::DivisionByZero);
        }

        // Convert requested underlying amount → b_tokens (round up to avoid dust).
        let b_to_burn = amount
            .checked_mul(SCALAR_12)
            .ok_or(StrategyError::ArithmeticError)?
            .checked_add(b_rate - 1)
            .ok_or(StrategyError::ArithmeticError)?
            .checked_div(b_rate)
            .ok_or(StrategyError::ArithmeticError)?;

        if b_to_burn == 0 {
            return Err(StrategyError::InsufficientBalance);
        }

        // Cap at what this vault actually has.
        let vault_b = get_b_tokens(&env, &from);
        if vault_b == 0 {
            return Err(StrategyError::InsufficientBalance);
        }
        let b_actual = b_to_burn.min(vault_b);

        // Compute the underlying amount lending will return for b_actual b_tokens.
        let underlying_out = b_actual
            .checked_mul(b_rate)
            .ok_or(StrategyError::ArithmeticError)?
            .checked_div(SCALAR_12)
            .ok_or(StrategyError::ArithmeticError)?;

        // Pre-authorize token.transfer(lending_pool → strategy, underlying_out)
        // which lending.withdraw() will execute internally.
        env.authorize_as_current_contract(soroban_sdk::vec![
            &env,
            InvokerContractAuthEntry::Contract(SubContractInvocation {
                context: ContractContext {
                    contract: config.asset.clone(),
                    fn_name: soroban_sdk::symbol_short!("transfer"),
                    args: soroban_sdk::vec![
                        &env,
                        config.lending_pool.clone().into_val(&env),
                        strategy_addr.clone().into_val(&env),
                        underlying_out.into_val(&env),
                    ],
                },
                sub_invocations: soroban_sdk::vec![&env],
            }),
        ]);

        let actual_withdrawn =
            lending.withdraw(&strategy_addr, &config.rwa_asset, &b_actual);

        // Forward the received tokens to the vault.
        let token = TokenClient::new(&env, &config.asset);
        token.transfer(&strategy_addr, &to, &actual_withdrawn);

        // Reduce this vault's attributed b_tokens.
        set_b_tokens(&env, &from, vault_b - b_actual);

        emit_withdraw(
            &env,
            String::from_str(&env, STRATEGY_NAME),
            actual_withdrawn,
            from.clone(),
        );

        Ok(underlying_balance(&env, &from, &config))
    }

    /// Returns the vault's current position value in underlying token units.
    ///
    /// value = vault_b_tokens * b_rate / SCALAR_12
    pub fn balance(env: Env, from: Address) -> Result<i128, StrategyError> {
        let config = load_config(&env);
        Ok(underlying_balance(&env, &from, &config))
    }

    /// Harvest — rwa-lending yield is embedded in b_rate appreciation (no separate reward token).
    ///
    /// Emits a `HarvestEvent` with the current b_rate as `price_per_share`.
    /// DeFindex derives APY by comparing b_rate values across consecutive harvests.
    ///
    /// The `data` parameter is unused (no swap params needed).
    pub fn harvest(env: Env, from: Address, _data: Option<Bytes>) -> Result<(), StrategyError> {
        let config = load_config(&env);
        let lending = neko_pool::Client::new(&env, &config.lending_pool);

        // b_rate starts at SCALAR_12 (1.0) and grows as interest accrues.
        // Using it as price_per_share lets DeFindex compute APY from b_rate growth rate.
        let b_rate = lending.get_b_token_rate(&config.rwa_asset);

        emit_harvest(
            &env,
            String::from_str(&env, STRATEGY_NAME),
            0,      // no explicit reward token harvested
            from,
            b_rate, // b_rate = price_per_share for this strategy
        );

        Ok(())
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn underlying_balance(env: &Env, vault: &Address, config: &StrategyConfig) -> i128 {
    let b_tokens = get_b_tokens(env, vault);
    if b_tokens == 0 {
        return 0;
    }
    let lending = neko_pool::Client::new(env, &config.lending_pool);
    let b_rate = lending.get_b_token_rate(&config.rwa_asset);
    b_tokens
        .checked_mul(b_rate)
        .unwrap_or(0)
        .checked_div(SCALAR_12)
        .unwrap_or(0)
}
