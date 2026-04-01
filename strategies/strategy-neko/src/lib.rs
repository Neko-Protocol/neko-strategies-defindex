#![no_std]

mod contract;
mod storage;

pub use contract::{NekoStrategy, NekoStrategyClient};

/// Import neko-pool contract interface from the bundled WASM.
///
/// The WASM lives in wasms/ at the repo root. To update it:
///   cd ../neko-contracts/stellar-contracts
///   cargo build --package neko-pool --target wasm32v1-none --release
///   cp target/wasm32v1-none/release/neko_pool.wasm \
///      ../../neko-strategies-defindex/wasms/neko/neko_pool.wasm
pub mod neko_pool {
    soroban_sdk::contractimport!(
        file = "../../wasms/neko/neko_pool.wasm"
    );
}
