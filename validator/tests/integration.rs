use std::sync::Arc;

use futures::{future, StreamExt};
use log::LevelFilter::{Debug, Info};

use nimiq_blockchain::AbstractBlockchain;
use nimiq_test_utils::validator::build_validators;

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn four_validators_can_create_an_epoch() {
    simple_logger::SimpleLogger::new()
        .with_level(Info)
        .with_module_level("nimiq_validator", Debug)
        .with_module_level("nimiq_network_libp2p", Info)
        .with_module_level("nimiq_handel", Info)
        .with_module_level("nimiq_tendermint", Debug)
        .with_module_level("nimiq_blockchain", Debug)
        .with_module_level("nimiq_block", Debug)
        .init()
        .ok();

    let validators = build_validators(4).await;

    let blockchain = Arc::clone(&validators.first().unwrap().consensus.blockchain);

    tokio::spawn(future::join_all(validators));

    let events = blockchain.notifier.write().as_stream();

    events.take(130).for_each(|_| future::ready(())).await;

    assert!(blockchain.block_number() >= 130);
    assert_eq!(blockchain.view_number(), 0);
}
