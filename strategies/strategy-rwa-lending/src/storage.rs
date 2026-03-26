use defindex_strategy_core::StrategyError;
use soroban_sdk::{contracttype, panic_with_error, Address, Env, Symbol};

/// rwa-lending uses 12-decimal fixed-point arithmetic for b_rate.
pub const SCALAR_12: i128 = 1_000_000_000_000;

pub const ONE_DAY_LEDGERS: u32 = 17_280;
/// Instance storage TTL: 30 days, bumped to 31 on every access.
pub const INSTANCE_TTL: u32  = ONE_DAY_LEDGERS * 30;
pub const INSTANCE_BUMP: u32 = ONE_DAY_LEDGERS * 31;
/// Persistent storage TTL: 1 year.
pub const PERSISTENT_TTL: u32  = ONE_DAY_LEDGERS * 365;
pub const PERSISTENT_BUMP: u32 = ONE_DAY_LEDGERS * 366;

/// Strategy configuration stored in instance storage.
#[contracttype]
#[derive(Clone, Debug)]
pub struct StrategyConfig {
    /// Underlying asset managed by this strategy (e.g. CETES token address).
    pub asset: Address,
    /// rwa-lending pool contract address.
    pub lending_pool: Address,
    /// Asset symbol used in rwa-lending (e.g. symbol_short!("CETES")).
    pub rwa_asset: Symbol,
}

#[contracttype]
enum StorageKey {
    Config,
    /// b_tokens attributed to a specific vault address.
    BTokens(Address),
}

// ─── Config ──────────────────────────────────────────────────────────────────

pub fn load_config(env: &Env) -> StrategyConfig {
    env.storage()
        .instance()
        .get(&StorageKey::Config)
        .unwrap_or_else(|| panic_with_error!(env, StrategyError::NotInitialized))
}

pub fn save_config(env: &Env, config: &StrategyConfig) {
    env.storage().instance().set(&StorageKey::Config, config);
    env.storage()
        .instance()
        .extend_ttl(INSTANCE_TTL, INSTANCE_BUMP);
}

pub fn is_initialized(env: &Env) -> bool {
    env.storage().instance().has(&StorageKey::Config)
}

// ─── Per-vault b_token tracking ──────────────────────────────────────────────

/// Returns the b_tokens currently attributed to `vault` in this strategy.
///
/// Multiple DeFindex vaults can use the same strategy contract. The strategy
/// is the lender in rwa-lending; b_tokens track each vault's share of the
/// total position. value = b_tokens * b_rate / SCALAR_12.
pub fn get_b_tokens(env: &Env, vault: &Address) -> i128 {
    let key = StorageKey::BTokens(vault.clone());
    let val: i128 = env.storage().persistent().get(&key).unwrap_or(0);
    if val > 0 {
        env.storage()
            .persistent()
            .extend_ttl(&key, PERSISTENT_TTL, PERSISTENT_BUMP);
    }
    val
}

/// Overwrites the b_tokens attributed to `vault`.
pub fn set_b_tokens(env: &Env, vault: &Address, b_tokens: i128) {
    let key = StorageKey::BTokens(vault.clone());
    env.storage().persistent().set(&key, &b_tokens);
    env.storage()
        .persistent()
        .extend_ttl(&key, PERSISTENT_TTL, PERSISTENT_BUMP);
}
