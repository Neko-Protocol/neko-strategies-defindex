use defindex_strategy_core::StrategyError;
use soroban_sdk::{contracttype, panic_with_error, Address, Env};

pub const ONE_DAY_LEDGERS: u32 = 17_280;
pub const INSTANCE_TTL: u32  = ONE_DAY_LEDGERS * 30;
pub const INSTANCE_BUMP: u32 = ONE_DAY_LEDGERS * 31;
pub const PERSISTENT_TTL: u32  = ONE_DAY_LEDGERS * 365;
pub const PERSISTENT_BUMP: u32 = ONE_DAY_LEDGERS * 366;

/// Strategy configuration stored in instance storage.
#[contracttype]
#[derive(Clone, Debug)]
pub struct StrategyConfig {
    /// Single-asset entry token (e.g. USDC). This is what vaults deposit/withdraw.
    pub token_a: Address,
    /// Pair token (e.g. XLM). Held transiently during deposit/withdraw.
    pub token_b: Address,
    /// Soroswap router contract address.
    pub router: Address,
    /// Soroswap pair contract address (resolved from router at init).
    pub pair: Address,
}

#[contracttype]
enum StorageKey {
    Config,
    /// LP tokens attributed to a specific vault address.
    LpTokens(Address),
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

// ─── Per-vault LP token tracking ─────────────────────────────────────────────

/// Returns the LP tokens currently attributed to `vault` in this strategy.
///
/// The strategy contract holds all LP tokens in the pair. Per-vault attribution
/// allows multiple DeFindex vaults to share the same strategy contract.
pub fn get_lp_tokens(env: &Env, vault: &Address) -> i128 {
    let key = StorageKey::LpTokens(vault.clone());
    let val: i128 = env.storage().persistent().get(&key).unwrap_or(0);
    if val > 0 {
        env.storage()
            .persistent()
            .extend_ttl(&key, PERSISTENT_TTL, PERSISTENT_BUMP);
    }
    val
}

/// Overwrites the LP tokens attributed to `vault`.
pub fn set_lp_tokens(env: &Env, vault: &Address, lp: i128) {
    let key = StorageKey::LpTokens(vault.clone());
    env.storage().persistent().set(&key, &lp);
    env.storage()
        .persistent()
        .extend_ttl(&key, PERSISTENT_TTL, PERSISTENT_BUMP);
}
