use defindex_strategy_core::StrategyError;
use soroban_sdk::{contracttype, panic_with_error, Address, Env};

pub const ONE_DAY_LEDGERS: u32 = 17_280;
pub const INSTANCE_TTL: u32  = ONE_DAY_LEDGERS * 30;
pub const INSTANCE_BUMP: u32 = ONE_DAY_LEDGERS * 31;
pub const PERSISTENT_TTL: u32  = ONE_DAY_LEDGERS * 365;
pub const PERSISTENT_BUMP: u32 = ONE_DAY_LEDGERS * 366;

const MAX_SLIPPAGE_BPS: u32 = 1_000; // 10%

/// Strategy configuration stored in instance storage.
#[contracttype]
#[derive(Clone, Debug)]
pub struct StrategyConfig {
    /// Single-asset entry token (e.g. CETES). This is what vaults deposit/withdraw.
    pub deposit_token: Address,
    /// Index of deposit_token in the Aquarius pool's token list (0 or 1).
    pub deposit_token_idx: u32,
    /// Pair token (e.g. USDC). Held transiently during deposit/withdraw.
    pub pair_token: Address,
    /// Index of pair_token in the pool's token list (0 or 1).
    pub pair_token_idx: u32,
    /// Aquarius pool contract.
    pub pool: Address,
    /// LP share token minted by the pool (tracked per-vault in persistent storage).
    pub share_token: Address,
    /// AQUA reward token claimable via pool.claim().
    pub aqua_token: Address,
    /// Maximum acceptable slippage in basis points (e.g. 50 = 0.5%).
    pub max_slippage_bps: u32,
    /// Admin address — can update slippage and sweep stuck tokens.
    pub admin: Address,
}

#[contracttype]
enum StorageKey {
    Config,
    /// LP share tokens attributed to a specific vault address.
    ShareTokens(Address),
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

pub fn validate_slippage(env: &Env, slippage_bps: u32) {
    if slippage_bps > MAX_SLIPPAGE_BPS {
        panic_with_error!(env, StrategyError::InvalidArgument);
    }
}

// ─── Per-vault LP share token tracking ───────────────────────────────────────

/// Returns the LP share tokens currently attributed to `vault`.
///
/// Aquarius pools mint a separate ERC20-like share token (not the pool contract
/// itself). The strategy holds all share tokens; per-vault attribution here
/// allows multiple DeFindex vaults to share the same strategy.
pub fn get_shares(env: &Env, vault: &Address) -> i128 {
    let key = StorageKey::ShareTokens(vault.clone());
    let val: i128 = env.storage().persistent().get(&key).unwrap_or(0);
    if val > 0 {
        env.storage()
            .persistent()
            .extend_ttl(&key, PERSISTENT_TTL, PERSISTENT_BUMP);
    }
    val
}

/// Overwrites the LP share tokens attributed to `vault`.
pub fn set_shares(env: &Env, vault: &Address, shares: i128) {
    let key = StorageKey::ShareTokens(vault.clone());
    env.storage().persistent().set(&key, &shares);
    env.storage()
        .persistent()
        .extend_ttl(&key, PERSISTENT_TTL, PERSISTENT_BUMP);
}
