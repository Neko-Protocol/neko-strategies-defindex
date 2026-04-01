use defindex_strategy_core::StrategyError;
use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    token::TokenClient,
    vec, Address, Env, IntoVal, Symbol, Vec,
};

use crate::{
    reserves,
    reserves::StrategyReserves,
    soroswap::internal_swap_exact_tokens_for_tokens,
    storage::Config,
};

soroban_sdk::contractimport!(file = "../../wasms/blend/pool.wasm");
pub type BlendPoolClient<'a> = Client<'a>;

#[derive(Clone, PartialEq)]
#[repr(u32)]
pub enum RequestType {
    Supply = 0,
    Withdraw = 1,
}

impl RequestType {
    fn to_u32(self) -> u32 {
        self as u32
    }
}

pub fn supply(
    e: &Env,
    from: &Address,
    amount: &i128,
    config: &Config,
    is_reinvest: bool,
) -> Result<i128, StrategyError> {
    let pool_client = BlendPoolClient::new(e, &config.pool);

    let pre_supply_amount = if is_reinvest {
        pool_client
            .get_positions(&e.current_contract_address())
            .supply
            .try_get(config.reserve_id)
            .unwrap_or(Some(0))
            .unwrap_or(0)
    } else {
        0
    };

    let requests: Vec<Request> = vec![
        e,
        Request {
            address: config.asset.clone(),
            amount: amount.clone(),
            request_type: RequestType::Supply.to_u32(),
        },
    ];

    e.authorize_as_current_contract(vec![
        e,
        InvokerContractAuthEntry::Contract(SubContractInvocation {
            context: ContractContext {
                contract: config.asset.clone(),
                fn_name: Symbol::new(e, "transfer"),
                args: (
                    e.current_contract_address(),
                    config.pool.clone(),
                    amount.clone(),
                )
                    .into_val(e),
            },
            sub_invocations: vec![e],
        }),
    ]);

    if is_reinvest {
        let new_positions = pool_client.submit(
            &e.current_contract_address(),
            &e.current_contract_address(),
            from,
            &requests,
        );
        let new_supply_amount = new_positions
            .supply
            .try_get(config.reserve_id)
            .unwrap_or(Some(0))
            .unwrap_or(0);

        let b_tokens_amount = new_supply_amount
            .checked_sub(pre_supply_amount)
            .ok_or(StrategyError::UnderflowOverflow)?;

        Ok(b_tokens_amount)
    } else {
        pool_client.submit(
            &e.current_contract_address(),
            &e.current_contract_address(),
            from,
            &requests,
        );
        Ok(*amount)
    }
}

pub fn withdraw(
    e: &Env,
    to: &Address,
    amount: &i128,
    config: &Config,
) -> Result<i128, StrategyError> {
    let pool_client = BlendPoolClient::new(e, &config.pool);

    let requests: Vec<Request> = vec![
        e,
        Request {
            address: config.asset.clone(),
            amount: amount.clone(),
            request_type: RequestType::Withdraw.to_u32(),
        },
    ];

    pool_client.submit(
        &e.current_contract_address(),
        &e.current_contract_address(),
        to,
        &requests,
    );

    Ok(*amount)
}

pub fn claim(e: &Env, from: &Address, config: &Config) -> i128 {
    let pool_client = BlendPoolClient::new(e, &config.pool);
    pool_client.claim(from, &config.claim_ids, from)
}

pub fn perform_reinvest(
    e: &Env,
    config: &Config,
    amount_out_min: i128,
) -> Result<StrategyReserves, StrategyError> {
    let blnd_balance =
        TokenClient::new(e, &config.blend_token).balance(&e.current_contract_address());

    if blnd_balance < config.reward_threshold {
        let reserves = reserves::get_strategy_reserve_updated(e, config);
        return Ok(reserves);
    }

    let swap_path = vec![e, config.blend_token.clone(), config.asset.clone()];

    let deadline = e
        .ledger()
        .timestamp()
        .checked_add(1)
        .ok_or(StrategyError::UnderflowOverflow)?;

    let swapped_amounts = internal_swap_exact_tokens_for_tokens(
        e,
        &blnd_balance,
        &amount_out_min,
        swap_path,
        &e.current_contract_address(),
        &deadline,
        config,
    )?;
    let amount_out: i128 = swapped_amounts
        .get(1)
        .ok_or(StrategyError::InternalSwapError)?
        .into_val(e);

    let b_tokens_minted = supply(
        e,
        &e.current_contract_address(),
        &amount_out,
        config,
        true,
    )?;

    reserves::harvest(e, b_tokens_minted, config)
}

pub fn reserve_b_rate(e: &Env, config: &Config) -> i128 {
    let pool_client = BlendPoolClient::new(e, &config.pool);
    pool_client.get_reserve(&config.asset).data.b_rate
}
