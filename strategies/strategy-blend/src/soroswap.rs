use defindex_strategy_core::StrategyError;
use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    panic_with_error, vec, Address, Env, IntoVal, InvokeError, Symbol, Vec,
};

use crate::storage::Config;

pub fn internal_swap_exact_tokens_for_tokens(
    e: &Env,
    amount_in: &i128,
    amount_out_min: &i128,
    path: Vec<Address>,
    to: &Address,
    deadline: &u64,
    config: &Config,
) -> Result<Vec<i128>, StrategyError> {
    let swap_args = vec![
        e,
        amount_in.into_val(e),
        amount_out_min.into_val(e),
        path.into_val(e),
        to.to_val(),
        deadline.into_val(e),
    ];

    let pair_address: Address = match e.try_invoke_contract::<Address, InvokeError>(
        &config.router,
        &Symbol::new(e, "router_pair_for"),
        path.into_val(e),
    ) {
        Ok(Ok(addr)) => addr,
        _ => panic_with_error!(e, StrategyError::SoroswapPairError),
    };

    e.authorize_as_current_contract(vec![
        e,
        InvokerContractAuthEntry::Contract(SubContractInvocation {
            context: ContractContext {
                contract: match path.get(0) {
                    Some(address) => address.clone(),
                    None => panic_with_error!(e, StrategyError::InvalidArgument),
                },
                fn_name: Symbol::new(e, "transfer"),
                args: (
                    e.current_contract_address(),
                    pair_address,
                    amount_in.clone(),
                )
                    .into_val(e),
            },
            sub_invocations: vec![e],
        }),
    ]);

    match e.try_invoke_contract::<Vec<i128>, InvokeError>(
        &config.router,
        &Symbol::new(e, "swap_exact_tokens_for_tokens"),
        swap_args.into_val(e),
    ) {
        Ok(Ok(result)) => Ok(result),
        _ => panic_with_error!(e, StrategyError::InternalSwapError),
    }
}
