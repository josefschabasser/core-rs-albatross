use std::{sync::Arc, time::Duration};

use beserial::Deserialize;
use nimiq_block_production::BlockProducer;
use nimiq_blockchain::AbstractBlockchain;
use nimiq_bls::{KeyPair, SecretKey};
use nimiq_build_tools::genesis::GenesisBuilder;
use nimiq_keys::{Address, SecureGenerate};
use nimiq_network_mock::{MockHub, MockNetwork};
use nimiq_primitives::account::ValidatorId;
use nimiq_primitives::coin::Coin;
use nimiq_test_utils::blockchain::produce_macro_blocks;
use nimiq_test_utils::node::Node;
use nimiq_test_utils::validator::seeded_rng;

/// Secret key of validator. Tests run with `network-primitives/src/genesis/unit-albatross.toml`
const SECRET_KEY: &str =
    "196ffdb1a8acc7cbd76a251aeac0600a1d68b3aba1eba823b5e4dc5dbdcdc730afa752c05ab4f6ef8518384ad514f403c5a088a22b17bf1bc14f8ff8decc2a512c0a200f68d7bdf5a319b30356fe8d1d75ef510aed7a8660968c216c328a0000";

#[ignore]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_request_component() {
    //simple_logger::init_by_env();

    let mut hub = Some(MockHub::default());

    // Generate genesis block.
    let key = KeyPair::generate(&mut seeded_rng(0));

    let genesis = GenesisBuilder::default()
        .with_genesis_validator(
            ValidatorId::default(),
            key.public_key,
            Address::default(),
            Coin::from_u64_unchecked(10000),
        )
        .generate()
        .unwrap();

    let mut node1 = Node::<MockNetwork>::new(1, genesis.clone(), &mut hub).await;
    let mut node2 = Node::<MockNetwork>::new(2, genesis.clone(), &mut hub).await;

    let keypair1 =
        KeyPair::from(SecretKey::deserialize_from_vec(&hex::decode(SECRET_KEY).unwrap()).unwrap());
    let producer1 = BlockProducer::new(
        Arc::clone(&node1.blockchain),
        Arc::clone(&node1.mempool),
        keypair1,
    );

    //let num_macro_blocks = (policy::BATCHES_PER_EPOCH + 1) as usize;
    //produce_macro_blocks(num_macro_blocks, &producer1, &node1.blockchain);

    node1.consume();
    node2.consume();

    // let node1 produce blocks again
    {
        let prod_blockchain = Arc::clone(&node1.blockchain);
        tokio::spawn(async move {
            loop {
                produce_macro_blocks(1, &producer1, &prod_blockchain);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
    }

    let mut connected = false;
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    loop {
        if node1.blockchain.block_number() > 200 && !connected {
            log::info!("Connecting node2 to node 1");
            node2.network.dial_mock(&node1.network);
            connected = true;
        }

        log::info!(
            "Node1: at #{} - {}",
            node1.blockchain.block_number(),
            node1.blockchain.head_hash()
        );
        log::info!(
            "Node2: at #{} - {}",
            node2.blockchain.block_number(),
            node2.blockchain.head_hash()
        );

        interval.tick().await;
    }
}
