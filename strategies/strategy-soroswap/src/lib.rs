#![no_std]

mod contract;
mod soroswap_pool;
mod storage;

pub use contract::{SoroswapStrategy, SoroswapStrategyClient};

/// Soroswap Router interface.
pub mod soroswap_router {
    soroban_sdk::contractimport!(file = "../../wasms/soroswap/router.wasm");
    pub type RouterClient<'a> = Client<'a>;
}

/// Soroswap Pair interface.
pub mod soroswap_pair {
    soroban_sdk::contractimport!(file = "../../wasms/soroswap/pair.wasm");
    pub type PairClient<'a> = Client<'a>;
}
