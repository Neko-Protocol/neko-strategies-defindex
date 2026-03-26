/// Soroswap AMM interaction logic.
///
/// Mirrors adapter-soroswap's soroswap_pool module, adapted for multi-vault
/// strategy use. Key difference: LP tokens are tracked per-vault in storage
/// rather than relying on pair.balance(contract_address) directly.
use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    token::TokenClient,
    vec, Address, Env, IntoVal, Symbol,
};

use crate::soroswap_pair;
use crate::soroswap_router;
use crate::storage::StrategyConfig;

// No slippage protection for MVP — configure min_out for production.
const MIN_OUT: i128 = 0;

/// Deposit `amount` of token_a into the Soroswap pair.
///
/// Pre-condition: `amount` of token_a is already held by the strategy contract.
///
/// Returns the net LP tokens minted during this deposit (delta, not total).
pub fn deposit(env: &Env, amount: i128, config: &StrategyConfig) -> i128 {
    let strategy = env.current_contract_address();
    let deadline = env.ledger().timestamp() + 3600;

    let swap_amount = amount / 2;
    let remaining_a = amount - swap_amount;

    let router = soroswap_router::RouterClient::new(env, &config.router);
    let pair   = soroswap_pair::PairClient::new(env, &config.pair);

    // ── Step 1: swap half of token_a → token_b ───────────────────────────
    env.authorize_as_current_contract(vec![
        env,
        InvokerContractAuthEntry::Contract(SubContractInvocation {
            context: ContractContext {
                contract: config.token_a.clone(),
                fn_name:  Symbol::new(env, "transfer"),
                args:     vec![
                    env,
                    strategy.clone().into_val(env),
                    config.pair.clone().into_val(env),
                    swap_amount.into_val(env),
                ],
            },
            sub_invocations: vec![env],
        }),
    ]);

    let path = vec![env, config.token_a.clone(), config.token_b.clone()];
    let swap_out = router.swap_exact_tokens_for_tokens(
        &swap_amount,
        &MIN_OUT,
        &path,
        &strategy,
        &deadline,
    );
    let b_received = swap_out.last().unwrap_or(0);

    // ── Step 2: compute exact token_b the router will use ────────────────
    // Soroswap's add_liquidity adjusts to the optimal ratio of current reserves.
    // Pre-authorize with the exact amount the router will pull.
    let token0 = pair.token_0();
    let (reserve0, reserve1) = pair.get_reserves();
    let (reserve_a, reserve_b) = if token0 == config.token_a {
        (reserve0, reserve1)
    } else {
        (reserve1, reserve0)
    };
    let b_optimal = remaining_a
        .checked_mul(reserve_b)
        .unwrap_or(0)
        .checked_div(reserve_a)
        .unwrap_or(0);
    let b_to_add = b_optimal.min(b_received);

    // ── Step 3: add liquidity with (remaining_a, b_to_add) ───────────────
    let lp_before = pair.balance(&strategy);

    env.authorize_as_current_contract(vec![
        env,
        InvokerContractAuthEntry::Contract(SubContractInvocation {
            context: ContractContext {
                contract: config.token_a.clone(),
                fn_name:  Symbol::new(env, "transfer"),
                args:     vec![
                    env,
                    strategy.clone().into_val(env),
                    config.pair.clone().into_val(env),
                    remaining_a.into_val(env),
                ],
            },
            sub_invocations: vec![env],
        }),
        InvokerContractAuthEntry::Contract(SubContractInvocation {
            context: ContractContext {
                contract: config.token_b.clone(),
                fn_name:  Symbol::new(env, "transfer"),
                args:     vec![
                    env,
                    strategy.clone().into_val(env),
                    config.pair.clone().into_val(env),
                    b_to_add.into_val(env),
                ],
            },
            sub_invocations: vec![env],
        }),
    ]);

    router.add_liquidity(
        &config.token_a,
        &config.token_b,
        &remaining_a,
        &b_to_add,
        &MIN_OUT,
        &MIN_OUT,
        &strategy,
        &deadline,
    );

    let lp_after = pair.balance(&strategy);

    // Return the net LP tokens minted for this deposit.
    lp_after - lp_before
}

/// Withdraw token_a proportional to `lp_to_burn` LP tokens and transfer to `to`.
///
/// Removes liquidity, swaps received token_b back to token_a, transfers to vault.
/// Returns the actual token_a amount sent to `to`.
pub fn withdraw(env: &Env, lp_to_burn: i128, to: &Address, config: &StrategyConfig) -> i128 {
    let strategy = env.current_contract_address();
    let deadline = env.ledger().timestamp() + 3600;

    let router = soroswap_router::RouterClient::new(env, &config.router);

    // ── Remove liquidity ─────────────────────────────────────────────────
    env.authorize_as_current_contract(vec![
        env,
        InvokerContractAuthEntry::Contract(SubContractInvocation {
            context: ContractContext {
                contract: config.pair.clone(),
                fn_name:  Symbol::new(env, "transfer"),
                args:     vec![
                    env,
                    strategy.clone().into_val(env),
                    config.pair.clone().into_val(env),
                    lp_to_burn.into_val(env),
                ],
            },
            sub_invocations: vec![env],
        }),
    ]);

    let (a_out, b_out) = router.remove_liquidity(
        &config.token_a,
        &config.token_b,
        &lp_to_burn,
        &MIN_OUT,
        &MIN_OUT,
        &strategy,
        &deadline,
    );

    // ── Swap token_b → token_a ───────────────────────────────────────────
    let total_a = if b_out > 0 {
        env.authorize_as_current_contract(vec![
            env,
            InvokerContractAuthEntry::Contract(SubContractInvocation {
                context: ContractContext {
                    contract: config.token_b.clone(),
                    fn_name:  Symbol::new(env, "transfer"),
                    args:     vec![
                        env,
                        strategy.clone().into_val(env),
                        config.pair.clone().into_val(env),
                        b_out.into_val(env),
                    ],
                },
                sub_invocations: vec![env],
            }),
        ]);

        let path = vec![env, config.token_b.clone(), config.token_a.clone()];
        let swap_out = router.swap_exact_tokens_for_tokens(
            &b_out,
            &MIN_OUT,
            &path,
            &strategy,
            &deadline,
        );
        a_out + swap_out.last().unwrap_or(0)
    } else {
        a_out
    };

    // ── Transfer token_a to vault ────────────────────────────────────────
    if total_a > 0 {
        let token = TokenClient::new(env, &config.token_a);
        token.transfer(&strategy, to, &total_a);
    }

    total_a
}

/// Returns the value in token_a units of `vault_lp` LP tokens.
///
/// value = share_of_reserve_a + (share_of_reserve_b × spot_price_b_in_a)
///
/// Uses vault_lp directly (not pair.balance) so it works per-vault even when
/// multiple vaults share the same strategy contract.
pub fn lp_value_in_a(env: &Env, vault_lp: i128, config: &StrategyConfig) -> i128 {
    if vault_lp == 0 {
        return 0;
    }

    let pair = soroswap_pair::PairClient::new(env, &config.pair);
    let total_lp = pair.total_supply();
    if total_lp == 0 {
        return 0;
    }

    let token0 = pair.token_0();
    let (reserve0, reserve1) = pair.get_reserves();
    let (reserve_a, reserve_b) = if token0 == config.token_a {
        (reserve0, reserve1)
    } else {
        (reserve1, reserve0)
    };

    if reserve_b == 0 {
        return 0;
    }

    let share_a = reserve_a
        .checked_mul(vault_lp)
        .unwrap_or(0)
        .checked_div(total_lp)
        .unwrap_or(0);
    let share_b = reserve_b
        .checked_mul(vault_lp)
        .unwrap_or(0)
        .checked_div(total_lp)
        .unwrap_or(0);

    // Convert share_b → token_a at AMM spot price
    let b_in_a = share_b
        .checked_mul(reserve_a)
        .unwrap_or(0)
        .checked_div(reserve_b)
        .unwrap_or(0);

    share_a + b_in_a
}
