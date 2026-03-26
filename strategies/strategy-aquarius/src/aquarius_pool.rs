/// Aquarius AMM interaction logic.
///
/// Mirrors adapter-aquarius's aquarius_pool module, adapted for multi-vault
/// strategy use. LP share tokens are tracked per-vault in storage rather than
/// relying on share_token.balance(contract_address) directly.
use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    token::TokenClient,
    vec, Address, Env, IntoVal, Symbol,
};

use crate::aquarius_pool_contract;
use crate::storage::StrategyConfig;

const BPS: u128 = 10_000;

fn min_with_slippage(expected: u128, slippage_bps: u32) -> u128 {
    expected
        .checked_mul(BPS - slippage_bps as u128)
        .unwrap_or(0)
        .checked_div(BPS)
        .unwrap_or(0)
}

/// Deposit `amount` of deposit_token into the Aquarius pool.
///
/// Pre-condition: `amount` of deposit_token is already held by the strategy.
///
/// Returns the net LP share tokens minted during this deposit (delta).
pub fn deposit(env: &Env, amount: i128, config: &StrategyConfig) -> i128 {
    let strategy = env.current_contract_address();
    let pool     = aquarius_pool_contract::PoolClient::new(env, &config.pool);
    let share    = TokenClient::new(env, &config.share_token);

    let swap_amount = amount / 2;
    let remaining   = amount - swap_amount;

    // ── Step 1: swap half deposit_token → pair_token ──────────────────────
    let estimated_pair = pool.estimate_swap(
        &config.deposit_token_idx,
        &config.pair_token_idx,
        &(swap_amount as u128),
    );
    let min_pair_out = min_with_slippage(estimated_pair, config.max_slippage_bps);

    env.authorize_as_current_contract(vec![
        env,
        InvokerContractAuthEntry::Contract(SubContractInvocation {
            context: ContractContext {
                contract: config.deposit_token.clone(),
                fn_name:  Symbol::new(env, "transfer"),
                args:     vec![
                    env,
                    strategy.clone().into_val(env),
                    config.pool.clone().into_val(env),
                    swap_amount.into_val(env),
                ],
            },
            sub_invocations: vec![env],
        }),
    ]);

    let pair_received = pool.swap(
        &strategy,
        &config.deposit_token_idx,
        &config.pair_token_idx,
        &(swap_amount as u128),
        &min_pair_out,
    ) as i128;

    // ── Step 2: compute optimal pair_token for add_liquidity ─────────────
    let reserves        = pool.get_reserves();
    let reserve_deposit = reserves.get(config.deposit_token_idx).unwrap_or(0) as i128;
    let reserve_pair    = reserves.get(config.pair_token_idx).unwrap_or(0) as i128;

    let pair_optimal = if reserve_deposit > 0 {
        remaining
            .checked_mul(reserve_pair)
            .unwrap_or(0)
            .checked_div(reserve_deposit)
            .unwrap_or(0)
    } else {
        pair_received
    };
    let pair_to_add = pair_optimal.min(pair_received);

    // ── Step 3: estimate shares, then add liquidity ───────────────────────
    let desired = if config.deposit_token_idx == 0 {
        vec![env, remaining as u128, pair_to_add as u128]
    } else {
        vec![env, pair_to_add as u128, remaining as u128]
    };

    let estimated_shares = pool.estimate_deposit(&desired);
    let min_shares = min_with_slippage(estimated_shares, config.max_slippage_bps);

    let shares_before = share.balance(&strategy);

    env.authorize_as_current_contract(vec![
        env,
        InvokerContractAuthEntry::Contract(SubContractInvocation {
            context: ContractContext {
                contract: config.deposit_token.clone(),
                fn_name:  Symbol::new(env, "transfer"),
                args:     vec![
                    env,
                    strategy.clone().into_val(env),
                    config.pool.clone().into_val(env),
                    remaining.into_val(env),
                ],
            },
            sub_invocations: vec![env],
        }),
        InvokerContractAuthEntry::Contract(SubContractInvocation {
            context: ContractContext {
                contract: config.pair_token.clone(),
                fn_name:  Symbol::new(env, "transfer"),
                args:     vec![
                    env,
                    strategy.clone().into_val(env),
                    config.pool.clone().into_val(env),
                    pair_to_add.into_val(env),
                ],
            },
            sub_invocations: vec![env],
        }),
    ]);

    pool.deposit(&strategy, &desired, &min_shares);

    let shares_after = share.balance(&strategy);

    // Return net shares minted for this deposit.
    shares_after - shares_before
}

/// Withdraw tokens from the Aquarius pool and send deposit_token to `to`.
///
/// Burns `shares_to_burn` share tokens, swaps received pair_token → deposit_token,
/// then transfers deposit_token to the vault.
/// Returns the actual deposit_token amount sent to `to`.
pub fn withdraw(env: &Env, shares_to_burn: i128, to: &Address, config: &StrategyConfig) -> i128 {
    let strategy = env.current_contract_address();
    let pool     = aquarius_pool_contract::PoolClient::new(env, &config.pool);

    // ── Compute min_amounts for remove_liquidity ──────────────────────────
    let reserves        = pool.get_reserves();
    let total_lp        = pool.get_total_shares();
    let reserve_deposit = reserves.get(config.deposit_token_idx).unwrap_or(0);
    let reserve_pair    = reserves.get(config.pair_token_idx).unwrap_or(0);

    let expected_deposit = reserve_deposit
        .checked_mul(shares_to_burn as u128).unwrap_or(0)
        .checked_div(total_lp).unwrap_or(0);
    let expected_pair = reserve_pair
        .checked_mul(shares_to_burn as u128).unwrap_or(0)
        .checked_div(total_lp).unwrap_or(0);

    let min_deposit_out = min_with_slippage(expected_deposit, config.max_slippage_bps);
    let min_pair_out    = min_with_slippage(expected_pair,    config.max_slippage_bps);

    let min_amounts = if config.deposit_token_idx == 0 {
        vec![env, min_deposit_out, min_pair_out]
    } else {
        vec![env, min_pair_out, min_deposit_out]
    };

    // ── Remove liquidity ──────────────────────────────────────────────────
    env.authorize_as_current_contract(vec![
        env,
        InvokerContractAuthEntry::Contract(SubContractInvocation {
            context: ContractContext {
                contract: config.share_token.clone(),
                fn_name:  Symbol::new(env, "burn"),
                args:     vec![
                    env,
                    strategy.clone().into_val(env),
                    shares_to_burn.into_val(env),
                ],
            },
            sub_invocations: vec![env],
        }),
    ]);

    let amounts_out = pool.withdraw(&strategy, &(shares_to_burn as u128), &min_amounts);
    let deposit_out = amounts_out.get(config.deposit_token_idx).unwrap_or(0) as i128;
    let pair_out    = amounts_out.get(config.pair_token_idx).unwrap_or(0) as i128;

    // ── Swap pair_token → deposit_token ───────────────────────────────────
    let total_deposit = if pair_out > 0 {
        let estimated = pool.estimate_swap(
            &config.pair_token_idx,
            &config.deposit_token_idx,
            &(pair_out as u128),
        );
        let min_deposit_from_swap = min_with_slippage(estimated, config.max_slippage_bps);

        env.authorize_as_current_contract(vec![
            env,
            InvokerContractAuthEntry::Contract(SubContractInvocation {
                context: ContractContext {
                    contract: config.pair_token.clone(),
                    fn_name:  Symbol::new(env, "transfer"),
                    args:     vec![
                        env,
                        strategy.clone().into_val(env),
                        config.pool.clone().into_val(env),
                        pair_out.into_val(env),
                    ],
                },
                sub_invocations: vec![env],
            }),
        ]);

        let swapped = pool.swap(
            &strategy,
            &config.pair_token_idx,
            &config.deposit_token_idx,
            &(pair_out as u128),
            &min_deposit_from_swap,
        ) as i128;

        deposit_out + swapped
    } else {
        deposit_out
    };

    // ── Transfer deposit_token to vault ───────────────────────────────────
    if total_deposit > 0 {
        let token = TokenClient::new(env, &config.deposit_token);
        token.transfer(&strategy, to, &total_deposit);
    }

    total_deposit
}

/// Returns the value in deposit_token units of `vault_shares` LP share tokens.
///
/// value = share_of_reserve_deposit + (share_of_reserve_pair × spot_price_pair_in_deposit)
pub fn shares_value_in_deposit(env: &Env, vault_shares: i128, config: &StrategyConfig) -> i128 {
    if vault_shares == 0 {
        return 0;
    }

    let pool     = aquarius_pool_contract::PoolClient::new(env, &config.pool);
    let total_lp = pool.get_total_shares() as i128;
    if total_lp == 0 {
        return 0;
    }

    let reserves        = pool.get_reserves();
    let reserve_deposit = reserves.get(config.deposit_token_idx).unwrap_or(0) as i128;
    let reserve_pair    = reserves.get(config.pair_token_idx).unwrap_or(0) as i128;
    if reserve_pair == 0 {
        return 0;
    }

    let lp_u  = vault_shares as u128;
    let tot_u = total_lp as u128;
    let rd    = reserve_deposit as u128;
    let rp    = reserve_pair as u128;

    let share_deposit = rd.checked_mul(lp_u).unwrap_or(0).checked_div(tot_u).unwrap_or(0);
    let share_pair    = rp.checked_mul(lp_u).unwrap_or(0).checked_div(tot_u).unwrap_or(0);
    let pair_in_deposit = share_pair.checked_mul(rd).unwrap_or(0).checked_div(rp).unwrap_or(0);

    (share_deposit + pair_in_deposit) as i128
}

/// Claim AQUA rewards from the pool and forward them to `to`.
///
/// Returns the AQUA amount harvested (0 if no rewards accrued).
pub fn claim(env: &Env, to: &Address, config: &StrategyConfig) -> i128 {
    let strategy = env.current_contract_address();
    let pool     = aquarius_pool_contract::PoolClient::new(env, &config.pool);

    let aqua_harvested = pool.claim(&strategy) as i128;

    if aqua_harvested > 0 {
        let aqua = TokenClient::new(env, &config.aqua_token);
        aqua.transfer(&strategy, to, &aqua_harvested);
    }

    aqua_harvested
}
