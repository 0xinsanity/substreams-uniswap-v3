extern crate core;

mod abi;
mod db;
mod eth;
mod helper;
mod keyer;
mod macros;
mod math;
mod pb;
mod price;
mod rpc;
mod utils;

use crate::abi::pool::events::Swap;
use crate::ethpb::v2::{Block, StorageChange};
use crate::keyer::native_pool_from_key;
use crate::pb::uniswap::entity_change::Operation;
use crate::pb::uniswap::event::Type::{Burn as BurnEvent, Mint as MintEvent, Swap as SwapEvent};
use crate::pb::uniswap::field::Type as FieldType;
use crate::pb::uniswap::{
    Burn, EntitiesChanges, EntityChange, Erc20Token, Erc20Tokens, Event, EventAmount, Events,
    Field, Mint, Pool, PoolLiquidities, PoolLiquidity, PoolSqrtPrice, PoolSqrtPrices, Pools, Tick,
};
use crate::price::WHITELIST_TOKENS;
use crate::utils::UNISWAP_V3_FACTORY;
use bigdecimal::ToPrimitive;
use bigdecimal::{BigDecimal, FromPrimitive};
use num_bigint::BigInt;
use std::collections::HashMap;
use std::ops::{Add, Div, Mul, Neg, Sub};
use std::str::FromStr;
use substreams::errors::Error;
use substreams::pb::substreams::StoreDeltas;
use substreams::store;
use substreams::store::{StoreAddBigFloat, StoreAddBigInt, StoreAppend, StoreGet, StoreSet};
use substreams::{log, proto, Hex};
use substreams_ethereum::{pb::eth as ethpb, Event as EventTrait};

#[substreams::handlers::map]
pub fn map_pools_created(block: Block) -> Result<Pools, Error> {
    let mut pools = vec![];
    for log in block.logs() {
        if let Some(event) = abi::factory::events::PoolCreated::match_and_decode(log) {
            log::info!("pool addr: {}", Hex(&event.pool));

            let mut ignore = false;
            if log.address() != UNISWAP_V3_FACTORY
                || Hex(&event.pool)
                    .to_string()
                    .eq("8fe8d9bb8eeba3ed688069c3d6b556c9ca258248")
            {
                ignore = true;
            }

            let mut pool: Pool = Pool {
                address: Hex(&log.data()[44..64]).to_string(),
                transaction_id: Hex(&log.receipt.transaction.hash).to_string(),
                created_at_block_number: block.number.to_string(),
                created_at_timestamp: block
                    .header
                    .as_ref()
                    .unwrap()
                    .timestamp
                    .as_ref()
                    .unwrap()
                    .seconds
                    .to_string(),
                fee_tier: event.fee.as_u32(),
                tick_spacing: event.tick_spacing.to_i32().unwrap(),
                log_ordinal: log.ordinal(),
                ignore_pool: ignore,
                ..Default::default()
            };
            // check the validity of the token0 and token1
            let mut token0 = Erc20Token {
                address: "".to_string(),
                name: "".to_string(),
                symbol: "".to_string(),
                decimals: 0,
                total_supply: "".to_string(),
                whitelist_pools: vec![],
            };
            let mut token1 = Erc20Token {
                address: "".to_string(),
                name: "".to_string(),
                symbol: "".to_string(),
                decimals: 0,
                total_supply: "".to_string(),
                whitelist_pools: vec![],
            };

            let token0_address: String = Hex(&event.token0).to_string();
            match rpc::create_uniswap_token(&token0_address) {
                None => {
                    continue;
                }
                Some(token) => {
                    token0 = token;
                }
            }

            let token1_address: String = Hex(&event.token1).to_string();
            match rpc::create_uniswap_token(&token1_address) {
                None => {
                    continue;
                }
                Some(token) => {
                    token1 = token;
                }
            }

            let token0_total_supply: BigInt = rpc::token_total_supply_call(&token0_address);
            token0.total_supply = token0_total_supply.to_string();

            let token1_total_supply: BigInt = rpc::token_total_supply_call(&token1_address);
            token1.total_supply = token1_total_supply.to_string();

            pool.token0 = Some(token0.clone());
            pool.token1 = Some(token1.clone());
            pools.push(pool);
        }
    }
    Ok(Pools { pools })
}

#[substreams::handlers::store]
pub fn store_pools(pools: Pools, output: StoreSet) {
    for pool in pools.pools {
        output.set(
            pool.log_ordinal,
            keyer::pool_key(&pool.address),
            &proto::encode(&pool).unwrap(),
        );
        output.set(
            pool.log_ordinal,
            keyer::pool_token_index_key(
                &pool.token0.as_ref().unwrap().address,
                &pool.token1.as_ref().unwrap().address,
            ),
            &proto::encode(&pool).unwrap(),
        );
    }
}

#[substreams::handlers::store]
pub fn store_pool_count(pools: Pools, output: StoreAddBigInt) {
    for pool in pools.pools {
        output.add(
            pool.log_ordinal,
            keyer::factory_pool_count_key(),
            &BigInt::from(1 as i32),
        )
    }
}

#[substreams::handlers::map]
pub fn map_tokens_whitelist_pools(pools: Pools) -> Result<Erc20Tokens, Error> {
    let mut erc20_tokens = Erc20Tokens { tokens: vec![] };

    for pool in pools.pools {
        let mut token0 = pool.token0.unwrap();
        let mut token1 = pool.token1.unwrap();

        if WHITELIST_TOKENS.contains(&token0.address.as_str()) {
            log::info!("adding pool: {} to token: {}", pool.address, token1.address);
            token1.whitelist_pools.push(pool.address.to_string());
            erc20_tokens.tokens.push(token1.clone());
        }

        if WHITELIST_TOKENS.contains(&token1.address.as_str()) {
            log::info!("adding pool: {} to token: {}", pool.address, token0.address);
            token0.whitelist_pools.push(pool.address.to_string());
            erc20_tokens.tokens.push(token0.clone());
        }
    }

    Ok(erc20_tokens)
}

#[substreams::handlers::store]
pub fn store_tokens_whitelist_pools(tokens: Erc20Tokens, output_append: StoreAppend) {
    for token in tokens.tokens {
        for pools in token.whitelist_pools {
            output_append.append(
                1,
                keyer::token_pool_whitelist(&token.address),
                &format!("{};", pools.to_string()),
            )
        }
    }
}

#[substreams::handlers::map]
pub fn map_pool_sqrt_price(block: Block, pools_store: StoreGet) -> Result<PoolSqrtPrices, Error> {
    let mut pool_sqrt_prices = vec![];
    for log in block.logs() {
        let pool_address = &Hex(log.address()).to_string();
        if let Some(event) = abi::pool::events::Initialize::match_and_decode(log) {
            log::info!(
                "log addr: {}",
                Hex(&log.receipt.transaction.hash.as_slice()).to_string()
            );
            match helper::get_pool(&pools_store, pool_address) {
                Err(err) => {
                    log::info!("skipping pool {}: {:?}", &pool_address, err);
                }
                Ok(pool) => {
                    pool_sqrt_prices.push(PoolSqrtPrice {
                        pool_address: pool.address,
                        ordinal: log.ordinal(),
                        sqrt_price: event.sqrt_price_x96.to_string(),
                        tick: event.tick.to_string(),
                    });
                }
            }
        } else if let Some(event) = Swap::match_and_decode(log) {
            log::info!(
                "log addr: {}",
                Hex(&log.receipt.transaction.hash.as_slice()).to_string()
            );
            match helper::get_pool(&pools_store, &pool_address) {
                Err(err) => {
                    log::info!("skipping pool {}: {:?}", &pool_address, err);
                }
                Ok(pool) => {
                    pool_sqrt_prices.push(PoolSqrtPrice {
                        pool_address: pool.address,
                        ordinal: log.ordinal(),
                        sqrt_price: event.sqrt_price_x96.to_string(),
                        tick: event.tick.to_string(),
                    });
                }
            }
        }
    }
    Ok(PoolSqrtPrices { pool_sqrt_prices })
}

#[substreams::handlers::store]
pub fn store_pool_sqrt_price(sqrt_prices: PoolSqrtPrices, output: StoreSet) {
    for sqrt_price in sqrt_prices.pool_sqrt_prices {
        log::info!("storing sqrt price {}", &sqrt_price.pool_address);
        // fixme: probably need to have a similar key for like we have for a swap
        output.set(
            sqrt_price.ordinal,
            keyer::pool_sqrt_price_key(&sqrt_price.pool_address),
            &proto::encode(&sqrt_price).unwrap(),
        )
    }
}

#[substreams::handlers::map]
pub fn map_pool_liquidities(block: Block, pools_store: StoreGet) -> Result<PoolLiquidities, Error> {
    let mut pool_liquidities = vec![];
    for trx in block.transaction_traces {
        if trx.status != 1 {
            continue;
        }
        for call in trx.calls {
            let _call_index = call.index;
            if call.state_reverted {
                continue;
            }
            for log in call.logs {
                let pool_key = keyer::pool_key(&Hex(&log.address).to_string());
                if let Some(_) = Swap::match_and_decode(&log) {
                    log::debug!("swap - trx_id: {}", Hex(&trx.hash).to_string());
                    match pools_store.get_last(&pool_key) {
                        None => continue,
                        Some(pool_bytes) => {
                            let pool: Pool = proto::decode(&pool_bytes).unwrap();
                            if !utils::should_handle_swap(&pool) {
                                continue;
                            }
                            if let Some(pl) = utils::extract_pool_liquidity(
                                log.ordinal,
                                &log.address,
                                &call.storage_changes,
                            ) {
                                pool_liquidities.push(pl)
                            }
                        }
                    }
                } else if let Some(_) = abi::pool::events::Mint::match_and_decode(&log) {
                    log::debug!("mint - trx_id: {}", Hex(&trx.hash).to_string());
                    match pools_store.get_last(&pool_key) {
                        None => {
                            log::info!("unknown pool");
                            continue;
                        }
                        Some(pool_bytes) => {
                            let pool: Pool = proto::decode(&pool_bytes).unwrap();
                            if !utils::should_handle_mint_and_burn(&pool) {
                                continue;
                            }
                            if let Some(pl) = utils::extract_pool_liquidity(
                                log.ordinal,
                                &log.address,
                                &call.storage_changes,
                            ) {
                                pool_liquidities.push(pl)
                            }
                        }
                    }
                } else if let Some(_) = abi::pool::events::Burn::match_and_decode(&log) {
                    log::debug!("burn - trx_id: {}", Hex(&trx.hash).to_string());
                    match pools_store.get_last(&pool_key) {
                        None => continue,
                        Some(pool_bytes) => {
                            let pool: Pool = proto::decode(&pool_bytes).unwrap();
                            if !utils::should_handle_mint_and_burn(&pool) {
                                continue;
                            }
                            if let Some(pl) = utils::extract_pool_liquidity(
                                log.ordinal,
                                &log.address,
                                &call.storage_changes,
                            ) {
                                pool_liquidities.push(pl)
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(PoolLiquidities { pool_liquidities })
}

#[substreams::handlers::store]
pub fn store_pool_liquidities(pool_liquidities: PoolLiquidities, output: StoreSet) {
    for pool_liquidity in pool_liquidities.pool_liquidities {
        // fixme: probably need to have a similar key for like we have for a swap
        output.set(
            0,
            keyer::pool_liquidity(&pool_liquidity.pool_address),
            &Vec::from(pool_liquidity.liquidity),
        )
    }
}

#[substreams::handlers::store]
pub fn store_prices(pool_sqrt_prices: PoolSqrtPrices, pools_store: StoreGet, output: StoreSet) {
    for sqrt_price_update in pool_sqrt_prices.pool_sqrt_prices {
        match helper::get_pool(&pools_store, &sqrt_price_update.pool_address) {
            Err(err) => {
                log::info!(
                    "skipping pool {}: {:?}",
                    &sqrt_price_update.pool_address,
                    err
                );
                continue;
            }
            Ok(pool) => {
                let token0 = pool.token0.as_ref().unwrap();
                let token1 = pool.token1.as_ref().unwrap();
                log::info!(
                    "pool addr: {}, pool trx_id: {}, token 0 addr: {}, token 1 addr: {}",
                    pool.address,
                    pool.transaction_id,
                    token0.address,
                    token1.address
                );

                let sqrt_price =
                    BigDecimal::from_str(sqrt_price_update.sqrt_price.as_str()).unwrap();
                log::info!("sqrtPrice: {}", sqrt_price.to_string());

                let tokens_price: (BigDecimal, BigDecimal) =
                    price::sqrt_price_x96_to_token_prices(&sqrt_price, &token0, &token1);
                log::debug!("token prices: {} {}", tokens_price.0, tokens_price.1);

                output.set(
                    sqrt_price_update.ordinal,
                    keyer::prices_pool_token_key(
                        &pool.address,
                        &token0.address,
                        "token0".to_string(),
                    ),
                    &Vec::from(tokens_price.0.to_string()),
                );
                output.set(
                    sqrt_price_update.ordinal,
                    keyer::prices_pool_token_key(
                        &pool.address,
                        &token1.address,
                        "token1".to_string(),
                    ),
                    &Vec::from(tokens_price.1.to_string()),
                );

                output.set(
                    sqrt_price_update.ordinal,
                    keyer::prices_token_pair(
                        &pool.token0.as_ref().unwrap().address,
                        &pool.token1.as_ref().unwrap().address,
                    ),
                    &Vec::from(tokens_price.0.to_string()),
                );
                output.set(
                    sqrt_price_update.ordinal,
                    keyer::prices_token_pair(
                        &pool.token1.as_ref().unwrap().address,
                        &pool.token0.as_ref().unwrap().address,
                    ),
                    &Vec::from(tokens_price.1.to_string()),
                );
            }
        }
    }
}

#[substreams::handlers::map]
pub fn map_swaps_mints_burns(block: Block, pools_store: StoreGet) -> Result<Events, Error> {
    let mut events = vec![];
    for log in block.logs() {
        let pool_key = &format!("pool:{}", Hex(&log.address()).to_string());

        if let Some(swap) = Swap::match_and_decode(log) {
            match pools_store.get_last(pool_key) {
                None => {
                    log::info!(
                        "invalid swap. pool does not exist. pool address {} transaction {}",
                        Hex(&log.address()).to_string(),
                        Hex(&log.receipt.transaction.hash).to_string()
                    );
                    continue;
                }
                Some(pool_bytes) => {
                    let pool: Pool = proto::decode(&pool_bytes).unwrap();
                    if !utils::should_handle_swap(&pool) {
                        continue;
                    }

                    let token0 = pool.token0.as_ref().unwrap();
                    let token1 = pool.token1.as_ref().unwrap();

                    let amount0 = utils::convert_token_to_decimal(&swap.amount0, token0.decimals);
                    let amount1 = utils::convert_token_to_decimal(&swap.amount1, token1.decimals);
                    log::debug!("amount0: {}, amount1:{}", amount0, amount1);

                    events.push(Event {
                        log_ordinal: log.ordinal(),
                        log_index: log.block_index() as u64,
                        pool_address: pool.address.to_string(),
                        token0: pool.token0.as_ref().unwrap().address.to_string(),
                        token1: pool.token1.as_ref().unwrap().address.to_string(),
                        fee: pool.fee_tier.to_string(),
                        transaction_id: Hex(&log.receipt.transaction.hash).to_string(), // todo: need to add #tx_count at the end
                        timestamp: block
                            .header
                            .as_ref()
                            .unwrap()
                            .timestamp
                            .as_ref()
                            .unwrap()
                            .seconds as u64,
                        r#type: Some(SwapEvent(pb::uniswap::Swap {
                            sender: Hex(&swap.sender).to_string(),
                            recipient: Hex(&swap.recipient).to_string(),
                            origin: Hex(&log.receipt.transaction.from).to_string(),
                            amount_0: amount0.to_string(),
                            amount_1: amount1.to_string(),
                            sqrt_price: swap.sqrt_price_x96.to_string(),
                            liquidity: swap.liquidity.to_string(),
                            tick: swap.tick.to_i32().unwrap(),
                        })),
                    });
                }
            }
        } else if let Some(mint) = abi::pool::events::Mint::match_and_decode(log) {
            match pools_store.get_last(pool_key) {
                None => {
                    log::info!(
                        "invalid mint. pool does not exist. pool address {} transaction {}",
                        Hex(&log.address()).to_string(),
                        Hex(&log.receipt.transaction.hash).to_string()
                    );
                    continue;
                }
                Some(pool_bytes) => {
                    let pool: Pool = proto::decode(&pool_bytes).unwrap();
                    if !utils::should_handle_mint_and_burn(&pool) {
                        continue;
                    }

                    let token0 = pool.token0.as_ref().unwrap();
                    let token1 = pool.token1.as_ref().unwrap();

                    let amount0_bi = BigInt::from_str(mint.amount0.to_string().as_str()).unwrap();
                    let amount1_bi = BigInt::from_str(mint.amount1.to_string().as_str()).unwrap();
                    let amount0 = utils::convert_token_to_decimal(&amount0_bi, token0.decimals);
                    let amount1 = utils::convert_token_to_decimal(&amount1_bi, token1.decimals);
                    log::debug!(
                        "logOrdinal: {}, amount0: {}, amount1:{}",
                        log.ordinal(),
                        amount0,
                        amount1
                    );

                    events.push(Event {
                        log_ordinal: log.ordinal(),
                        log_index: log.block_index() as u64,
                        pool_address: pool.address.to_string(),
                        token0: pool.token0.unwrap().address,
                        token1: pool.token1.unwrap().address,
                        fee: pool.fee_tier.to_string(),
                        transaction_id: Hex(&log.receipt.transaction.hash).to_string(),
                        timestamp: block
                            .header
                            .as_ref()
                            .unwrap()
                            .timestamp
                            .as_ref()
                            .unwrap()
                            .seconds as u64,
                        r#type: Some(MintEvent(Mint {
                            owner: Hex(&mint.owner).to_string(),
                            sender: Hex(&mint.sender).to_string(),
                            origin: Hex(&log.receipt.transaction.from).to_string(),
                            amount: mint.amount.to_string(),
                            amount_0: amount0.to_string(),
                            amount_1: amount1.to_string(),
                            tick_lower: mint.tick_lower.to_i32().unwrap(),
                            tick_upper: mint.tick_upper.to_i32().unwrap(),
                        })),
                    });
                }
            }
        } else if let Some(burn) = abi::pool::events::Burn::match_and_decode(log) {
            match pools_store.get_last(pool_key) {
                None => {
                    log::info!(
                        "invalid burn. pool does not exist. pool address {} transaction {}",
                        Hex(&log.address()).to_string(),
                        Hex(&log.receipt.transaction.hash).to_string()
                    );
                    continue;
                }
                Some(pool_bytes) => {
                    let pool: Pool = proto::decode(&pool_bytes).unwrap();
                    if !utils::should_handle_mint_and_burn(&pool) {
                        continue;
                    }

                    let token0 = pool.token0.as_ref().unwrap();
                    let token1 = pool.token1.as_ref().unwrap();

                    let amount0_bi = BigInt::from_str(burn.amount0.to_string().as_str()).unwrap();
                    let amount1_bi = BigInt::from_str(burn.amount1.to_string().as_str()).unwrap();
                    let amount0 = utils::convert_token_to_decimal(&amount0_bi, token0.decimals);
                    let amount1 = utils::convert_token_to_decimal(&amount1_bi, token1.decimals);
                    log::debug!("amount0: {}, amount1:{}", amount0, amount1);

                    events.push(Event {
                        log_ordinal: log.ordinal(),
                        log_index: log.block_index() as u64,
                        pool_address: pool.address.to_string(),
                        token0: pool.token0.as_ref().unwrap().address.to_string(),
                        token1: pool.token1.as_ref().unwrap().address.to_string(),
                        fee: pool.fee_tier.to_string(),
                        transaction_id: Hex(&log.receipt.transaction.hash).to_string(),
                        timestamp: block
                            .header
                            .as_ref()
                            .unwrap()
                            .timestamp
                            .as_ref()
                            .unwrap()
                            .seconds as u64,
                        r#type: Some(BurnEvent(Burn {
                            owner: Hex(&burn.owner).to_string(),
                            origin: Hex(&log.receipt.transaction.from).to_string(),
                            amount: burn.amount.to_string(),
                            amount_0: amount0.to_string(),
                            amount_1: amount1.to_string(),
                            tick_lower: burn.tick_lower.to_i32().unwrap(),
                            tick_upper: burn.tick_upper.to_i32().unwrap(),
                        })),
                    });
                }
            }
        }
    }
    Ok(Events { events })
}

#[substreams::handlers::map]
pub fn map_event_amounts(events: Events) -> Result<pb::uniswap::EventAmounts, Error> {
    let mut event_amounts = vec![];
    for event in events.events {
        log::debug!("transaction id: {}", event.transaction_id);
        if event.r#type.is_none() {
            continue;
        }

        if event.r#type.is_some() {
            match event.r#type.unwrap() {
                BurnEvent(burn) => {
                    log::debug!("handling burn for pool {}", event.pool_address);
                    let amount0 = BigDecimal::from_str(burn.amount_0.as_str()).unwrap();
                    let amount1 = BigDecimal::from_str(burn.amount_1.as_str()).unwrap();
                    event_amounts.push(EventAmount {
                        pool_address: event.pool_address,
                        log_ordinal: event.log_ordinal,
                        token0_addr: event.token0,
                        amount0_value: amount0.neg().to_string(),
                        token1_addr: event.token1,
                        amount1_value: amount1.neg().to_string(),
                        ..Default::default()
                    });
                }
                MintEvent(mint) => {
                    log::debug!("handling mint for pool {}", event.pool_address);
                    let amount0 = BigDecimal::from_str(mint.amount_0.as_str()).unwrap();
                    let amount1 = BigDecimal::from_str(mint.amount_1.as_str()).unwrap();
                    event_amounts.push(EventAmount {
                        pool_address: event.pool_address,
                        log_ordinal: event.log_ordinal,
                        token0_addr: event.token0,
                        amount0_value: amount0.to_string(),
                        token1_addr: event.token1,
                        amount1_value: amount1.to_string(),
                        ..Default::default()
                    });
                }
                SwapEvent(swap) => {
                    log::debug!("handling swap for pool {}", event.pool_address);
                    let amount0 = BigDecimal::from_str(swap.amount_0.as_str()).unwrap();
                    let amount1 = BigDecimal::from_str(swap.amount_1.as_str()).unwrap();
                    event_amounts.push(EventAmount {
                        pool_address: event.pool_address,
                        log_ordinal: event.log_ordinal,
                        token0_addr: event.token0,
                        amount0_value: amount0.to_string(),
                        token1_addr: event.token1,
                        amount1_value: amount1.to_string(),
                        ..Default::default()
                    });
                }
            }
        }
    }
    Ok(pb::uniswap::EventAmounts { event_amounts })
}

//todo: need to find a way to compute totalValueLockedETH for the factory
// -> factory.totalValueLockedETH = factory.totalValueLockedETH.minus(pool.totalValueLockedETH)
//    on each mint, burn and swap, we have to minus the totalValueLockedETH with the previous
//    totalValueLockedETH from the pool and then add the newly computed pool.totalValueLockedETH
//    to factory.totalValueLockedETH
// Does the mean we need a substreams to read and write in the same store? Or think of a way to
// keep the last value of pool.totalValueLockedETH and then "set" it?
#[substreams::handlers::store]
pub fn store_totals(
    store_eth_prices: StoreGet,
    total_value_locked_deltas: store::Deltas,
    output: StoreAddBigFloat,
) {
    let mut pool_total_value_locked_eth_new_value: BigDecimal = BigDecimal::from(0);
    for delta in total_value_locked_deltas {
        if !delta.key.starts_with("pool:") {
            continue;
        }
        match delta.key.as_str().split(":").last().unwrap() {
            "eth" => {
                let pool_total_value_locked_eth_old_value: BigDecimal =
                    math::decimal_from_bytes(&delta.old_value);
                pool_total_value_locked_eth_new_value = math::decimal_from_bytes(&delta.new_value);

                let pool_total_value_locked_eth_diff: BigDecimal =
                    pool_total_value_locked_eth_old_value
                        .sub(pool_total_value_locked_eth_new_value.clone());

                output.add(
                    delta.ordinal,
                    keyer::factory_total_value_locked_eth(),
                    &pool_total_value_locked_eth_diff,
                )
            }
            "usd" => {
                let bundle_eth_price: BigDecimal = match store_eth_prices.get_last("bundle") {
                    Some(price) => math::decimal_from_bytes(&price),
                    None => continue,
                };
                log::debug!("eth_price_usd: {}", bundle_eth_price);

                let total_value_locked_usd: BigDecimal = pool_total_value_locked_eth_new_value
                    .clone()
                    .mul(bundle_eth_price);

                // here we have to do a hackish way to set the value, to not have to
                // create a new store which would do the same but that would set the
                // value instead of summing it, what we do is calculate the difference
                // and simply add/sub the difference and that mimics the same as setting
                // the value
                let total_value_locked_usd_old_value: BigDecimal =
                    math::decimal_from_bytes(&delta.old_value);
                let diff: BigDecimal = total_value_locked_usd.sub(total_value_locked_usd_old_value);

                output.add(
                    delta.ordinal,
                    keyer::factory_total_value_locked_usd(),
                    &diff,
                );
            }
            _ => continue,
        }
    }
}

#[substreams::handlers::store]
pub fn store_total_tx_counts(events: Events, output: StoreAddBigInt) {
    for event in events.events {
        output.add(
            event.log_ordinal,
            keyer::pool_total_tx_count(&event.pool_address),
            &BigInt::from(1 as i32),
        );
        output.add(
            event.log_ordinal,
            keyer::token_total_tx_count(&event.token0),
            &BigInt::from(1 as i32),
        );
        output.add(
            event.log_ordinal,
            keyer::token_total_tx_count(&event.token1),
            &BigInt::from(1 as i32),
        );
        output.add(
            event.log_ordinal,
            keyer::factory_total_tx_count(),
            &BigInt::from(1 as i32),
        );
    }
}

#[substreams::handlers::store]
pub fn store_swaps_volume(
    events: Events,
    store_pool: StoreGet,
    store_total_tx_counts: StoreGet,
    store_eth_prices: StoreGet,
    output: StoreAddBigFloat,
) {
    for event in events.events {
        let pool: Pool = match store_pool.get_last(keyer::pool_key(&event.pool_address)) {
            None => continue,
            Some(bytes) => proto::decode(&bytes).unwrap(),
        };
        match store_total_tx_counts.get_last(keyer::pool_total_tx_count(&event.pool_address)) {
            None => {}
            Some(_) => match event.r#type.unwrap() {
                SwapEvent(swap) => {
                    let mut eth_price_in_usd = helper::get_eth_price(&store_eth_prices).unwrap();

                    let mut token0_derived_eth_price: BigDecimal = BigDecimal::from(0 as i32);
                    match store_eth_prices.get_last(keyer::token_eth_price(&event.token0)) {
                        None => continue,
                        Some(bytes) => token0_derived_eth_price = math::decimal_from_bytes(&bytes),
                    }

                    let mut token1_derived_eth_price: BigDecimal = BigDecimal::from(0 as i32);
                    match store_eth_prices.get_last(keyer::token_eth_price(&event.token1)) {
                        None => continue,
                        Some(bytes) => token1_derived_eth_price = math::decimal_from_bytes(&bytes),
                    }

                    let mut amount0_abs: BigDecimal =
                        BigDecimal::from_str(swap.amount_0.as_str()).unwrap();
                    if amount0_abs.lt(&BigDecimal::from(0 as u64)) {
                        amount0_abs = amount0_abs.mul(BigDecimal::from(-1 as i64))
                    }

                    let mut amount1_abs: BigDecimal =
                        BigDecimal::from_str(swap.amount_1.as_str()).unwrap();
                    if amount1_abs.lt(&BigDecimal::from(0 as u64)) {
                        amount1_abs = amount1_abs.mul(BigDecimal::from(-1 as i64))
                    }

                    log::info!("trx_id: {}", event.transaction_id);
                    log::info!("bundle.ethPriceUSD: {}", eth_price_in_usd);
                    log::info!("token0_derived_eth_price: {}", token0_derived_eth_price);
                    log::info!("token1_derived_eth_price: {}", token1_derived_eth_price);
                    log::info!("amount0_abs: {}", amount0_abs);
                    log::info!("amount1_abs: {}", amount1_abs);

                    let amount_total_usd_tracked: BigDecimal = utils::get_tracked_amount_usd(
                        &event.token0,
                        &event.token1,
                        &token0_derived_eth_price,
                        &token1_derived_eth_price,
                        &amount0_abs,
                        &amount1_abs,
                        &eth_price_in_usd,
                    )
                    .div(BigDecimal::from(2 as i32));

                    let amount_total_eth_tracked =
                        math::safe_div(&amount_total_usd_tracked, &eth_price_in_usd);

                    let amount_total_usd_untracked: BigDecimal = amount0_abs
                        .clone()
                        .add(amount1_abs.clone())
                        .div(BigDecimal::from(2 as i32));

                    let fee_tier: BigDecimal = BigDecimal::from(pool.fee_tier);
                    let fee_usd: BigDecimal = amount_total_usd_tracked
                        .clone()
                        .mul(fee_tier.clone())
                        .div(BigDecimal::from(1000000 as u64));
                    let fee_eth: BigDecimal = amount_total_eth_tracked
                        .clone()
                        .mul(fee_tier)
                        .div(BigDecimal::from(1000000 as u64));

                    output.add(
                        event.log_ordinal,
                        keyer::swap_volume_token_0(&event.pool_address),
                        &amount0_abs,
                    );
                    output.add(
                        event.log_ordinal,
                        keyer::swap_volume_token_1(&event.pool_address),
                        &amount1_abs,
                    );
                    output.add(
                        event.log_ordinal,
                        keyer::swap_volume_usd(&event.pool_address),
                        &amount_total_usd_tracked,
                    );
                    output.add(
                        event.log_ordinal,
                        keyer::swap_untracked_volume_usd(&event.pool_address),
                        &amount_total_usd_untracked,
                    );
                    output.add(
                        event.log_ordinal,
                        keyer::swap_fee_usd(&event.pool_address),
                        &fee_usd,
                    );
                    output.add(
                        event.log_ordinal,
                        keyer::swap_token_volume(&event.token0, "token0".to_string()),
                        &amount0_abs,
                    );
                    output.add(
                        event.log_ordinal,
                        keyer::swap_token_volume(&event.token1, "token1".to_string()),
                        &amount1_abs,
                    );
                    output.add(
                        event.log_ordinal,
                        keyer::swap_token_volume_usd(&event.token0),
                        &amount_total_usd_tracked,
                    );
                    output.add(
                        event.log_ordinal,
                        keyer::swap_token_volume_usd(&event.token1),
                        &amount_total_usd_tracked,
                    );
                    output.add(
                        event.log_ordinal,
                        keyer::swap_token_volume_untracked_volume_usd(&event.token0),
                        &amount_total_usd_untracked,
                    );
                    output.add(
                        event.log_ordinal,
                        keyer::swap_token_volume_untracked_volume_usd(&event.token1),
                        &amount_total_usd_untracked,
                    );
                    output.add(
                        event.log_ordinal,
                        keyer::swap_token_fee_usd(&event.token0),
                        &fee_usd,
                    );
                    output.add(
                        event.log_ordinal,
                        keyer::swap_token_fee_usd(&event.token1),
                        &fee_usd,
                    );
                    output.add(
                        event.log_ordinal,
                        keyer::swap_factory_total_volume_eth(),
                        &amount_total_eth_tracked,
                    );
                    output.add(
                        event.log_ordinal,
                        keyer::swap_factory_total_fees_eth(),
                        &fee_eth,
                    )
                }
                _ => {}
            },
        }
    }
}

#[substreams::handlers::store]
pub fn store_pool_fee_growth_global_x128(pools: Pools, output: StoreSet) {
    for pool in pools.pools {
        log::info!(
            "pool address: {} trx_id:{}",
            pool.address,
            pool.transaction_id
        );
        let (bd0, bd1) = rpc::fee_growth_global_x128_call(&pool.address);
        log::debug!("big decimal0: {}", bd0);
        log::debug!("big decimal1: {}", bd1);

        output.set(
            pool.log_ordinal,
            keyer::pool_fee_growth_global_x128(&pool.address, "token0".to_string()),
            &Vec::from(bd0.to_string().as_str()),
        );
        output.set(
            pool.log_ordinal,
            keyer::pool_fee_growth_global_x128(&pool.address, "token1".to_string()),
            &Vec::from(bd1.to_string().as_str()),
        );
    }
}

#[substreams::handlers::store]
pub fn store_native_total_value_locked(
    event_amounts: pb::uniswap::EventAmounts,
    output: StoreAddBigFloat,
) {
    for event_amount in event_amounts.event_amounts {
        output.add(
            event_amount.log_ordinal,
            keyer::token_native_total_value_locked(&event_amount.token0_addr),
            &BigDecimal::from_str(event_amount.amount0_value.as_str()).unwrap(),
        );
        output.add(
            event_amount.log_ordinal,
            keyer::pool_native_total_value_locked_token(
                &event_amount.pool_address,
                &event_amount.token0_addr,
            ),
            &BigDecimal::from_str(event_amount.amount0_value.as_str()).unwrap(),
        );
        output.add(
            event_amount.log_ordinal,
            keyer::token_native_total_value_locked(&event_amount.token1_addr),
            &BigDecimal::from_str(event_amount.amount1_value.as_str()).unwrap(),
        );
        output.add(
            event_amount.log_ordinal,
            keyer::pool_native_total_value_locked_token(
                &event_amount.pool_address,
                &event_amount.token1_addr,
            ),
            &BigDecimal::from_str(event_amount.amount1_value.as_str()).unwrap(),
        );
    }
}

#[substreams::handlers::store]
pub fn store_eth_prices(
    pool_sqrt_prices: PoolSqrtPrices,
    pools_store: StoreGet,
    prices_store: StoreGet,
    tokens_whitelist_pools_store: StoreGet,
    total_native_value_locked_store: StoreGet,
    pool_liquidities_store: StoreGet,
    output: StoreSet,
) {
    for pool_sqrt_price in pool_sqrt_prices.pool_sqrt_prices {
        log::debug!(
            "handling pool price update - addr: {} price: {}",
            pool_sqrt_price.pool_address,
            pool_sqrt_price.sqrt_price
        );
        let pool = helper::get_pool(&pools_store, &pool_sqrt_price.pool_address).unwrap();
        let token_0 = pool.token0.as_ref().unwrap();
        let token_1 = pool.token1.as_ref().unwrap();

        utils::log_token(token_0, 0);
        utils::log_token(token_1, 1);

        let bundle_eth_price_usd =
            price::get_eth_price_in_usd(&prices_store, pool_sqrt_price.ordinal);
        log::info!("bundle_eth_price_usd: {}", bundle_eth_price_usd);

        let token0_derived_eth_price = price::find_eth_per_token(
            pool_sqrt_price.ordinal,
            &pool.address,
            &token_0.address,
            &pools_store,
            &pool_liquidities_store,
            &tokens_whitelist_pools_store,
            &total_native_value_locked_store,
            &prices_store,
        );
        log::info!(
            "token 0 {} derived eth price: {}",
            token_0.address,
            token0_derived_eth_price
        );

        let token1_derived_eth_price = price::find_eth_per_token(
            pool_sqrt_price.ordinal,
            &pool.address,
            &token_1.address,
            &pools_store,
            &pool_liquidities_store,
            &tokens_whitelist_pools_store,
            &total_native_value_locked_store,
            &prices_store,
        );
        log::info!(
            "token 1 {} derived eth price: {}",
            token_1.address,
            token1_derived_eth_price
        );

        output.set(
            pool_sqrt_price.ordinal,
            keyer::bundle_eth_price(),
            &Vec::from(bundle_eth_price_usd.to_string()),
        );

        output.set(
            pool_sqrt_price.ordinal,
            keyer::token_eth_price(&token_0.address),
            &Vec::from(token0_derived_eth_price.to_string()),
        );

        output.set(
            pool_sqrt_price.ordinal,
            keyer::token_eth_price(&token_1.address),
            &Vec::from(token1_derived_eth_price.to_string()),
        );
    }
}

#[substreams::handlers::store]
pub fn store_total_value_locked_by_tokens(events: Events, output: StoreAddBigFloat) {
    for event in events.events {
        log::info!("trx_id: {}", event.transaction_id);
        let mut amount0: BigDecimal = BigDecimal::from(0 as i32);
        let mut amount1: BigDecimal = BigDecimal::from(0 as i32);

        match event.r#type.unwrap() {
            BurnEvent(burn) => {
                amount0 = BigDecimal::from_str(burn.amount_0.as_str()).unwrap().neg();
                amount1 = BigDecimal::from_str(burn.amount_1.as_str()).unwrap().neg();
            }
            MintEvent(mint) => {
                amount0 = BigDecimal::from_str(mint.amount_0.as_str()).unwrap();
                amount1 = BigDecimal::from_str(mint.amount_1.as_str()).unwrap();
            }
            SwapEvent(swap) => {
                amount0 = BigDecimal::from_str(swap.amount_0.as_str()).unwrap();
                amount1 = BigDecimal::from_str(swap.amount_1.as_str()).unwrap();
            }
        }

        output.add(
            event.log_ordinal,
            keyer::total_value_locked_by_tokens(
                &event.pool_address,
                &event.token0,
                "token0".to_string(),
            ),
            &amount0,
        );
        output.add(
            event.log_ordinal,
            keyer::total_value_locked_by_tokens(
                &event.pool_address,
                &event.token1,
                "token1".to_string(),
            ),
            &amount1,
        );
    }
}

#[substreams::handlers::store]
pub fn store_total_value_locked(
    native_total_value_locked_deltas: store::Deltas,
    pools_store: StoreGet,
    eth_prices_store: StoreGet,
    output: StoreSet,
) {
    // fixme: @julien: what is the use for the pool aggregator here ?
    let mut pool_aggregator: HashMap<String, (u64, BigDecimal)> = HashMap::from([]);

    // fixme: are we sure we want to unwrap and fail here ? we can't even go over the first block..
    // let eth_price_usd = helper::get_eth_price(&eth_prices_store).unwrap();

    for native_total_value_locked in native_total_value_locked_deltas {
        let eth_price_usd: BigDecimal = match &eth_prices_store.get_last(&keyer::bundle_eth_price())
        {
            None => continue,
            Some(bytes) => math::decimal_from_bytes(&bytes),
        };
        log::debug!(
            "eth_price_usd: {}, native_total_value_locked.key: {}",
            eth_price_usd,
            native_total_value_locked.key
        );
        if let Some(token_addr) = keyer::native_token_from_key(&native_total_value_locked.key) {
            let value = math::decimal_from_bytes(&native_total_value_locked.new_value);
            let token_derive_eth =
                helper::get_token_eth_price(&eth_prices_store, &token_addr).unwrap();

            let total_value_locked_usd = value.mul(token_derive_eth).mul(&eth_price_usd);

            log::info!(
                "token {} total value locked usd: {}",
                token_addr,
                total_value_locked_usd
            );
            output.set(
                native_total_value_locked.ordinal,
                keyer::token_usd_total_value_locked(&token_addr),
                &Vec::from(total_value_locked_usd.to_string()),
            );
        } else if let Some((pool_addr, token_addr)) =
            native_pool_from_key(&native_total_value_locked.key)
        {
            let pool = helper::get_pool(&pools_store, &pool_addr).unwrap();
            // we only want to use the token0
            if pool.token0.as_ref().unwrap().address != token_addr {
                continue;
            }
            let value: BigDecimal = math::decimal_from_bytes(&native_total_value_locked.new_value);
            let token_derive_eth: BigDecimal =
                helper::get_token_eth_price(&eth_prices_store, &token_addr).unwrap();
            let partial_pool_total_value_locked_eth = value.mul(token_derive_eth);
            log::info!(
                "partial pool {} token {} partial total value locked usd: {}",
                pool_addr,
                token_addr,
                partial_pool_total_value_locked_eth,
            );
            let aggregate_key = pool_addr.clone();

            //fixme: @julien: it seems we never actually enter here... as it would only be valid if we have
            // twice a valid event on the same pool
            if let Some(pool_agg) = pool_aggregator.get(&aggregate_key) {
                let count = &pool_agg.0;
                let rolling_sum = &pool_agg.1;
                log::info!("found another partial pool value {} token {} count {} partial total value locked usd: {}",
                    pool_addr,
                    token_addr,
                    count,
                    rolling_sum,
                );
                if count.to_i32().unwrap() >= 2 {
                    panic!(
                        "{}",
                        format!("this is unexpected should only see 2 pool keys")
                    )
                }

                log::info!(
                    "partial_pool_total_value_locked_eth: {} and rolling_sum: {}",
                    partial_pool_total_value_locked_eth,
                    rolling_sum,
                );
                let pool_total_value_locked_eth =
                    partial_pool_total_value_locked_eth.add(rolling_sum);
                let pool_total_value_locked_usd =
                    pool_total_value_locked_eth.clone().mul(&eth_price_usd);
                output.set(
                    native_total_value_locked.ordinal,
                    keyer::pool_eth_total_value_locked(&pool_addr),
                    &Vec::from(pool_total_value_locked_eth.to_string()),
                );
                output.set(
                    native_total_value_locked.ordinal,
                    keyer::pool_usd_total_value_locked(&pool_addr),
                    &Vec::from(pool_total_value_locked_usd.to_string()),
                );

                continue;
            }
            pool_aggregator.insert(
                aggregate_key.clone(),
                (1, partial_pool_total_value_locked_eth),
            );
            log::info!("partial inserted");
        }
    }
}

#[substreams::handlers::store]
pub fn store_ticks(events: Events, output_set: StoreSet) {
    for event in events.events {
        match event.r#type.unwrap() {
            SwapEvent(_) => {}
            BurnEvent(_) => {
                // todo
            }
            MintEvent(mint) => {
                let tick_lower_big_int = BigInt::from_str(&mint.tick_lower.to_string()).unwrap();
                let tick_lower_price0 = math::big_decimal_exponated(
                    BigDecimal::from_f64(1.0001).unwrap().with_prec(100),
                    tick_lower_big_int,
                );
                let tick_lower_price1 =
                    math::safe_div(&BigDecimal::from(1 as i32), &tick_lower_price0);

                let tick_lower: Tick = Tick {
                    pool_address: event.pool_address.to_string(),
                    idx: mint.tick_lower.to_string(),
                    price0: tick_lower_price0.to_string(),
                    price1: tick_lower_price1.to_string(),
                };

                output_set.set(
                    event.log_ordinal,
                    format!(
                        "tick:{}:pool:{}",
                        mint.tick_lower.to_string(),
                        event.pool_address.to_string()
                    ),
                    &proto::encode(&tick_lower).unwrap(),
                );

                let tick_upper_big_int = BigInt::from_str(&mint.tick_upper.to_string()).unwrap();
                let tick_upper_price0 = math::big_decimal_exponated(
                    BigDecimal::from_f64(1.0001).unwrap().with_prec(100),
                    tick_upper_big_int,
                );
                let tick_upper_price1 =
                    math::safe_div(&BigDecimal::from(1 as i32), &tick_upper_price0);
                let tick_upper: Tick = Tick {
                    pool_address: event.pool_address.to_string(),
                    idx: mint.tick_upper.to_string(),
                    price0: tick_upper_price0.to_string(),
                    price1: tick_upper_price1.to_string(),
                };

                output_set.set(
                    event.log_ordinal,
                    format!(
                        "tick:{}:pool:{}",
                        mint.tick_upper.to_string(),
                        event.pool_address.to_string()
                    ),
                    &proto::encode(&tick_upper).unwrap(),
                );
            }
        }
    }
}

// #[substreams::handlers::map]
// pub fn map_fees(block: ethpb::v2::Block) -> Result<pb::uniswap::Fees, Error> {
//     let mut out = pb::uniswap::Fees { fees: vec![] };
//
//     for trx in block.transaction_traces {
//         for call in trx.calls.iter() {
//             if call.state_reverted {
//                 continue;
//             }
//
//             for log in call.logs.iter() {
//                 if !abi::factory::events::FeeAmountEnabled::match_log(&log) {
//                     continue;
//                 }
//
//                 let ev = abi::factory::events::FeeAmountEnabled::decode(&log).unwrap();
//
//                 out.fees.push(pb::uniswap::Fee {
//                     fee: ev.fee.as_u32(),
//                     tick_spacing: ev.tick_spacing.to_i32().unwrap(),
//                 });
//             }
//         }
//     }
//
//     Ok(out)
// }
//
// #[substreams::handlers::store]
// pub fn store_fees(block: ethpb::v2::Block, output: store::StoreSet) {
//     for trx in block.transaction_traces {
//         for call in trx.calls.iter() {
//             if call.state_reverted {
//                 continue;
//             }
//             for log in call.logs.iter() {
//                 if !abi::factory::events::FeeAmountEnabled::match_log(&log) {
//                     continue;
//                 }
//
//                 let event = abi::factory::events::FeeAmountEnabled::decode(&log).unwrap();
//
//                 let fee = pb::uniswap::Fee {
//                     fee: event.fee.as_u32(),
//                     tick_spacing: event.tick_spacing.to_i32().unwrap(),
//                 };
//
//                 output.set(
//                     log.ordinal,
//                     format!("fee:{}:{}", fee.fee, fee.tick_spacing),
//                     &proto::encode(&fee).unwrap(),
//                 );
//             }
//         }
//     }
// }
//
// #[substreams::handlers::map]
// pub fn map_flashes(block: ethpb::v2::Block) -> Result<pb::uniswap::Flashes, Error> {
//     let mut out = pb::uniswap::Flashes { flashes: vec![] };
//
//     for trx in block.transaction_traces {
//         for call in trx.calls.iter() {
//             if call.state_reverted {
//                 continue;
//             }
//             for log in call.logs.iter() {
//                 if abi::pool::events::Swap::match_log(&log) {
//                     log::debug!("log ordinal: {}", log.ordinal);
//                 }
//                 if !abi::pool::events::Flash::match_log(&log) {
//                     continue;
//                 }
//
//                 let flash = abi::pool::events::Flash::decode(&log).unwrap();
//
//                 out.flashes.push(Flash {
//                     sender: Hex(&flash.sender).to_string(),
//                     recipient: Hex(&flash.recipient).to_string(),
//                     amount_0: flash.amount0.as_u64(),
//                     amount_1: flash.amount1.as_u64(),
//                     paid_0: flash.paid0.as_u64(),
//                     paid_1: flash.paid1.as_u64(),
//                     transaction_id: Hex(&trx.hash).to_string(),
//                     log_ordinal: log.ordinal,
//                 });
//             }
//         }
//     }
//
//     Ok(out)
// }

#[substreams::handlers::map]
pub fn map_factory_entities(
    block: Block,
    pool_count_deltas: store::Deltas,
    tx_count_deltas: store::Deltas,
    swaps_volume_deltas: store::Deltas,
    totals_deltas: store::Deltas,
) -> Result<EntitiesChanges, Error> {
    let mut out = EntitiesChanges {
        ..Default::default()
    };

    if block.number == 12369621 {
        out.entity_changes
            .push(db::factory_created_factory_entity_change());
    }

    for delta in pool_count_deltas {
        out.entity_changes
            .push(db::pool_created_factory_entity_change(delta))
    }

    for delta in tx_count_deltas {
        if let Some(change) = db::tx_count_factory_entity_change(delta) {
            out.entity_changes.push(change);
        }
    }

    for delta in swaps_volume_deltas {
        if let Some(change) = db::swap_volume_factory_entity_change(delta) {
            out.entity_changes.push(change);
        }
    }

    for delta in totals_deltas {
        if let Some(change) = db::total_value_locked_factory_entity_change(delta) {
            out.entity_changes.push(change);
        }
    }

    Ok(out)
}

#[substreams::handlers::map]
pub fn map_pool_entities(
    pools_created: Pools,
    pool_sqrt_price_deltas: store::Deltas,
    pool_liquidities_store_deltas: store::Deltas,
    total_value_locked_deltas: store::Deltas,
    total_value_locked_by_tokens_deltas: store::Deltas,
    pool_fee_growth_global_x128_deltas: store::Deltas,
    price_deltas: store::Deltas,
    tx_count_deltas: store::Deltas,
    swaps_volume_deltas: store::Deltas,
) -> Result<EntitiesChanges, Error> {
    let mut out = EntitiesChanges {
        ..Default::default()
    };

    for pool in pools_created.pools {
        out.entity_changes
            .push(db::pools_created_pool_entity_change(pool));
    }

    for delta in pool_sqrt_price_deltas {
        if let Some(change) = db::pool_sqrt_price_entity_change(delta) {
            out.entity_changes.push(change);
        }
    }

    for delta in pool_liquidities_store_deltas {
        out.entity_changes
            .push(db::pool_liquidities_pool_entity_change(delta))
    }

    for delta in total_value_locked_deltas {
        if let Some(change) = db::total_value_locked_pool_entity_change(delta) {
            out.entity_changes.push(change);
        }
    }

    for delta in total_value_locked_by_tokens_deltas {
        if let Some(change) = db::total_value_locked_by_token_pool_entity_change(delta) {
            out.entity_changes.push(change);
        }
    }

    for delta in pool_fee_growth_global_x128_deltas {
        if let Some(change) = db::pool_fee_growth_global_x128_entity_change(delta) {
            out.entity_changes.push(change);
        }
    }

    for delta in price_deltas {
        if let Some(change) = db::price_pool_entity_change(delta) {
            out.entity_changes.push(change);
        }
    }

    for delta in tx_count_deltas {
        if let Some(change) = db::tx_count_pool_entity_change(delta) {
            out.entity_changes.push(change);
        }
    }

    for delta in swaps_volume_deltas {
        if let Some(change) = db::swap_volume_pool_entity_change(delta) {
            out.entity_changes.push(change);
        }
    }

    Ok(out)
}

#[substreams::handlers::map]
pub fn map_tokens_entities(
    pools_created: Pools,
    swaps_volume_deltas: store::Deltas,
    tx_count_deltas: store::Deltas,
    total_value_locked_by_deltas: store::Deltas,
    total_value_locked_deltas: store::Deltas,
    derived_eth_prices_deltas: store::Deltas,
) -> Result<EntitiesChanges, Error> {
    let mut out = EntitiesChanges {
        block_id: vec![],
        block_number: 0,
        prev_block_id: vec![],
        prev_block_number: 0,
        entity_changes: vec![],
    };

    //todo: when a pool is created, we also save the token
    // (id, name, symbol, decimals and total supply)
    // issue here is what if we have multiple pools with t1-t2, t1-t3, t1-t4, etc.
    // we will have t1 generate multiple entity changes for nothings since it has
    // already been emitted -- subgraph doesn't solve this either
    for pool in pools_created.pools {
        out.entity_changes
            .append(&mut db::tokens_created_token_entity_change(pool));
    }

    for delta in swaps_volume_deltas {
        if let Some(change) = db::swap_volume_token_entity_change(delta) {
            out.entity_changes.push(change);
        }
    }

    for delta in tx_count_deltas {
        if let Some(change) = db::tx_count_token_entity_change(delta) {
            out.entity_changes.push(change);
        }
    }

    for delta in total_value_locked_by_deltas {
        out.entity_changes
            .push(db::total_value_locked_by_token_token_entity_change(delta))
    }

    for delta in total_value_locked_deltas {
        if let Some(change) = db::total_value_locked_usd_token_entity_change(delta) {
            out.entity_changes.push(change);
        }
    }

    for delta in derived_eth_prices_deltas {
        if let Some(change) = db::derived_eth_prices_token_entity_change(delta) {
            out.entity_changes.push(change);
        }
    }

    Ok(out)
}

//todo: check the tickLower, tickUpper, amount, amount0, amount1 and amountUSD, for the moment
// they are stored as String values, but shouldn't it be int instead or BigInt in some cases?
#[substreams::handlers::map]
pub fn map_swaps_mints_burns_entities(
    events: Events,
    tx_count_store: StoreGet,
    store_eth_prices: StoreGet,
) -> Result<EntitiesChanges, Error> {
    let mut out = EntitiesChanges {
        block_id: vec![],
        block_number: 0,
        prev_block_id: vec![],
        prev_block_number: 0,
        entity_changes: vec![],
    };

    for event in events.events {
        if event.r#type.is_none() {
            continue;
        }

        if event.r#type.is_some() {
            let transaction_count: i32 =
                match tx_count_store.get_last(keyer::factory_total_tx_count()) {
                    Some(data) => String::from_utf8_lossy(data.as_slice())
                        .to_string()
                        .parse::<i32>()
                        .unwrap(),
                    None => 0,
                };

            let transaction_id: String = format!("{}#{}", event.transaction_id, transaction_count);

            let token0_derived_eth_price =
                match store_eth_prices.get_last(keyer::token_eth_price(&event.token0)) {
                    None => {
                        // initializePool has occurred beforehand so there should always be a price
                        // maybe just ? instead of returning 1 and bubble up the error if there is one
                        BigDecimal::from(0 as u64)
                    }
                    Some(derived_eth_price_bytes) => {
                        utils::decode_bytes_to_big_decimal(derived_eth_price_bytes)
                    }
                };

            let token1_derived_eth_price: BigDecimal =
                match store_eth_prices.get_last(keyer::token_eth_price(&event.token1)) {
                    None => {
                        // initializePool has occurred beforehand so there should always be a price
                        // maybe just ? instead of returning 1 and bubble up the error if there is one
                        BigDecimal::from(0 as u64)
                    }
                    Some(derived_eth_price_bytes) => {
                        utils::decode_bytes_to_big_decimal(derived_eth_price_bytes)
                    }
                };

            let bundle_eth_price: BigDecimal =
                match store_eth_prices.get_last(keyer::bundle_eth_price()) {
                    None => {
                        // initializePool has occurred beforehand so there should always be a price
                        // maybe just ? instead of returning 1 and bubble up the error if there is one
                        BigDecimal::from(1 as u64)
                    }
                    Some(bundle_eth_price_bytes) => {
                        utils::decode_bytes_to_big_decimal(bundle_eth_price_bytes)
                    }
                };

            match event.r#type.unwrap() {
                SwapEvent(swap) => {
                    let amount0: BigDecimal = BigDecimal::from_str(swap.amount_0.as_str()).unwrap();
                    let amount1: BigDecimal = BigDecimal::from_str(swap.amount_1.as_str()).unwrap();

                    let amount_usd: BigDecimal = utils::calculate_amount_usd(
                        &amount0,
                        &amount1,
                        &token0_derived_eth_price,
                        &token1_derived_eth_price,
                        &bundle_eth_price,
                    );

                    out.entity_changes.push(EntityChange {
                        entity: "Swap".to_string(),
                        id: string_field_value!(transaction_id),
                        ordinal: event.log_ordinal,
                        operation: Operation::Create as i32,
                        fields: vec![
                            new_field!(
                                "id",
                                FieldType::String,
                                string_field_value!(transaction_id)
                            ),
                            new_field!(
                                "transaction",
                                FieldType::String,
                                string_field_value!(event.transaction_id)
                            ),
                            new_field!(
                                "timestamp",
                                FieldType::Bigint,
                                big_int_field_value!(event.timestamp.to_string())
                            ),
                            new_field!(
                                "pool",
                                FieldType::String,
                                string_field_value!(event.pool_address)
                            ),
                            new_field!(
                                "token0",
                                FieldType::String,
                                string_field_value!(event.token0)
                            ),
                            new_field!(
                                "token1",
                                FieldType::String,
                                string_field_value!(event.token1)
                            ),
                            new_field!(
                                "sender",
                                FieldType::String,
                                string_field_value!(swap.sender)
                            ),
                            new_field!(
                                "recipient",
                                FieldType::String,
                                string_field_value!(swap.recipient)
                            ),
                            new_field!(
                                "origin",
                                FieldType::String,
                                string_field_value!(swap.origin)
                            ),
                            new_field!(
                                "amount0",
                                FieldType::String,
                                string_field_value!(swap.amount_0)
                            ),
                            new_field!(
                                "amount1",
                                FieldType::String,
                                string_field_value!(swap.amount_1)
                            ),
                            new_field!(
                                "amountUSD",
                                FieldType::String,
                                string_field_value!(amount_usd.to_string())
                            ),
                            new_field!(
                                "sqrtPriceX96",
                                FieldType::Int,
                                string_field_value!(swap.sqrt_price)
                            ),
                            new_field!("tick", FieldType::Int, int_field_value!(swap.tick)),
                            new_field!(
                                "logIndex",
                                FieldType::String,
                                string_field_value!(event.log_ordinal.to_string())
                            ),
                        ],
                    })
                }
                MintEvent(mint) => {
                    let amount0: BigDecimal = BigDecimal::from_str(mint.amount_0.as_str()).unwrap();
                    let amount1: BigDecimal = BigDecimal::from_str(mint.amount_1.as_str()).unwrap();

                    let amount_usd: BigDecimal = utils::calculate_amount_usd(
                        &amount0,
                        &amount1,
                        &token0_derived_eth_price,
                        &token1_derived_eth_price,
                        &bundle_eth_price,
                    );

                    out.entity_changes.push(EntityChange {
                        entity: "Mint".to_string(),
                        id: string_field_value!(transaction_id),
                        ordinal: event.log_ordinal,
                        operation: Operation::Create as i32,
                        fields: vec![
                            new_field!(
                                "id",
                                FieldType::String,
                                string_field_value!(transaction_id)
                            ),
                            new_field!(
                                "transaction",
                                FieldType::String,
                                string_field_value!(event.transaction_id)
                            ),
                            new_field!(
                                "timestamp",
                                FieldType::Bigint,
                                big_int_field_value!(event.timestamp.to_string())
                            ),
                            new_field!(
                                "pool",
                                FieldType::String,
                                string_field_value!(event.pool_address)
                            ),
                            new_field!(
                                "token0",
                                FieldType::String,
                                string_field_value!(event.token0)
                            ),
                            new_field!(
                                "token1",
                                FieldType::String,
                                string_field_value!(event.token1)
                            ),
                            new_field!("owner", FieldType::String, string_field_value!(mint.owner)),
                            new_field!(
                                "sender",
                                FieldType::String,
                                string_field_value!(mint.sender)
                            ),
                            new_field!(
                                "origin",
                                FieldType::String,
                                string_field_value!(mint.origin)
                            ),
                            new_field!(
                                "amount",
                                FieldType::String,
                                string_field_value!(mint.amount)
                            ),
                            new_field!(
                                "amount0",
                                FieldType::String,
                                string_field_value!(mint.amount_0)
                            ),
                            new_field!(
                                "amount1",
                                FieldType::String,
                                string_field_value!(mint.amount_1)
                            ),
                            new_field!(
                                "amountUSD",
                                FieldType::String,
                                string_field_value!(amount_usd.to_string())
                            ),
                            new_field!(
                                "tickLower",
                                FieldType::String,
                                string_field_value!(mint.tick_lower.to_string())
                            ),
                            new_field!(
                                "tickUpper",
                                FieldType::String,
                                string_field_value!(mint.tick_upper.to_string())
                            ),
                            new_field!(
                                "logIndex",
                                FieldType::String,
                                string_field_value!(event.log_ordinal.to_string())
                            ),
                        ],
                    });
                }
                BurnEvent(burn) => {
                    let amount0: BigDecimal = BigDecimal::from_str(burn.amount_0.as_str()).unwrap();
                    let amount1: BigDecimal = BigDecimal::from_str(burn.amount_1.as_str()).unwrap();

                    let amount_usd: BigDecimal = utils::calculate_amount_usd(
                        &amount0,
                        &amount1,
                        &token0_derived_eth_price,
                        &token1_derived_eth_price,
                        &bundle_eth_price,
                    );

                    out.entity_changes.push(EntityChange {
                        entity: "Burn".to_string(),
                        id: string_field_value!(transaction_id),
                        ordinal: event.log_ordinal,
                        operation: Operation::Create as i32,
                        fields: vec![
                            new_field!(
                                "id",
                                FieldType::String,
                                string_field_value!(transaction_id)
                            ),
                            new_field!(
                                "transaction",
                                FieldType::String,
                                string_field_value!(event.transaction_id)
                            ),
                            new_field!(
                                "timestamp",
                                FieldType::Bigint,
                                big_int_field_value!(event.timestamp.to_string())
                            ),
                            new_field!(
                                "pool",
                                FieldType::String,
                                string_field_value!(event.pool_address)
                            ),
                            new_field!(
                                "token0",
                                FieldType::String,
                                string_field_value!(event.token0)
                            ),
                            new_field!(
                                "token1",
                                FieldType::String,
                                string_field_value!(event.token1)
                            ),
                            new_field!("owner", FieldType::String, string_field_value!(burn.owner)),
                            new_field!(
                                "origin",
                                FieldType::String,
                                string_field_value!(burn.origin)
                            ),
                            new_field!(
                                "amount",
                                FieldType::String,
                                string_field_value!(burn.amount_0)
                            ),
                            new_field!(
                                "amount0",
                                FieldType::String,
                                string_field_value!(burn.amount_0)
                            ),
                            new_field!(
                                "amount1",
                                FieldType::String,
                                string_field_value!(burn.amount_1)
                            ),
                            new_field!(
                                "amountUSD",
                                FieldType::String,
                                string_field_value!(amount_usd.to_string())
                            ),
                            new_field!(
                                "tickLower",
                                FieldType::String,
                                string_field_value!(burn.tick_lower.to_string())
                            ),
                            new_field!(
                                "tickUpper",
                                FieldType::String,
                                string_field_value!(burn.tick_upper.to_string())
                            ),
                            new_field!(
                                "logIndex",
                                FieldType::String,
                                string_field_value!(event.log_ordinal.to_string())
                            ),
                        ],
                    })
                }
            }
        }
    }

    Ok(out)
}

#[substreams::handlers::map]
pub fn graph_out(
    block: Block,
    pool_entities: EntitiesChanges,
    token_entities: EntitiesChanges,
    swaps_mints_burns_entities: EntitiesChanges,
) -> Result<EntitiesChanges, Error> {
    let mut out = EntitiesChanges {
        block_id: block.hash,
        block_number: block.number,
        prev_block_id: block.header.unwrap().parent_hash,
        prev_block_number: block.number - 1 as u64,
        entity_changes: vec![],
    };

    //todo: check if we wand to check the block ordinal here and sort by the ordinal
    // or simply stream out all the entity changes

    for change in pool_entities.entity_changes {
        out.entity_changes.push(change);
    }

    for change in token_entities.entity_changes {
        out.entity_changes.push(change);
    }

    for change in swaps_mints_burns_entities.entity_changes {
        out.entity_changes.push(change);
    }

    Ok(out)
}
