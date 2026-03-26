#![no_std]

mod contract;
mod storage;

pub use contract::{RwaLendingStrategy, RwaLendingStrategyClient};

/// Import rwa-lending contract interface from the bundled WASM.
///
/// The WASM lives in wasms/ at the repo root. To update it:
///   cd ../neko-contracts/stellar-contracts
///   cargo build --package rwa-lending --target wasm32v1-none --release
///   cp target/wasm32v1-none/release/rwa_lending.wasm \
///      ../../neko-strategies-defindex/wasms/rwa_lending.wasm
pub mod rwa_lending {
    soroban_sdk::contractimport!(
        file = "../../wasms/neko/rwa_lending.wasm"
    );
}
