#![no_std]

mod aquarius_pool;
mod contract;
mod storage;

pub use contract::{AquariusStrategy, AquariusStrategyClient};

/// Aquarius liquidity pool interface.
pub mod aquarius_pool_contract {
    soroban_sdk::contractimport!(
        file = "../../wasms/aquarius/soroban_liquidity_pool_contract.wasm"
    );
    pub type PoolClient<'a> = Client<'a>;
}
