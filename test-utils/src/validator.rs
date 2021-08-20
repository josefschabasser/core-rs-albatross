use futures::{future, StreamExt};
use rand::prelude::StdRng;
use rand::SeedableRng;
use std::sync::Arc;
use tokio_stream::wrappers::BroadcastStream;

use crate::consensus::consensus;
use nimiq_blockchain::{AbstractBlockchain, Blockchain};
use nimiq_bls::KeyPair;
use nimiq_build_tools::genesis::{GenesisBuilder, GenesisInfo};
use nimiq_consensus::sync::history::HistorySync;
use nimiq_consensus::{Consensus as AbstractConsensus, ConsensusEvent};
use nimiq_database::volatile::VolatileEnvironment;
use nimiq_hash::{Blake2bHash, Hash};
use nimiq_keys::{Address, SecureGenerate};
use nimiq_mempool::{Mempool, MempoolConfig};
use nimiq_network_interface::network::Network as NetworkInterface;
use nimiq_network_libp2p::libp2p::core::multiaddr::multiaddr;
use nimiq_network_libp2p::Network;
use nimiq_network_mock::{MockHub, MockNetwork};
use nimiq_primitives::coin::Coin;
use nimiq_primitives::networks::NetworkId;
use nimiq_utils::time::OffsetTime;
use nimiq_validator::validator::Validator as AbstractValidator;
use nimiq_validator_network::network_impl::ValidatorNetworkImpl;

type Consensus = AbstractConsensus<Network>;
type Validator = AbstractValidator<Network, ValidatorNetworkImpl<Network>>;

type MockConsensus = AbstractConsensus<MockNetwork>;
type MockValidator = AbstractValidator<MockNetwork, ValidatorNetworkImpl<MockNetwork>>;

fn seeded_rng(seed: u64) -> StdRng {
    StdRng::seed_from_u64(seed)
}

async fn build_validator(
    peer_id: u64,
    signing_key: KeyPair,
    genesis_info: GenesisInfo,
) -> (Validator, Consensus) {
    let consensus = consensus(peer_id, genesis_info).await;
    let validator_network = Arc::new(ValidatorNetworkImpl::new(consensus.network.clone()));
    (
        Validator::new(&consensus, validator_network, signing_key, None),
        consensus,
    )
}

pub async fn build_validators(num_validators: usize) -> Vec<Validator> {
    // Generate validator key pairs.
    let mut rng = seeded_rng(0);
    let keys: Vec<KeyPair> = (0..num_validators)
        .map(|_| KeyPair::generate(&mut rng))
        .collect();

    // Generate genesis block.
    let mut genesis_builder = GenesisBuilder::default();
    for key in &keys {
        genesis_builder.with_genesis_validator(
            key.public_key.hash::<Blake2bHash>().as_slice()[0..20].into(),
            key.public_key,
            Address::default(),
            Coin::from_u64_unchecked(10000),
        );
    }
    let genesis = genesis_builder.generate().unwrap();

    // Instantiate validators.
    let mut validators = vec![];
    let mut consensus = vec![];
    for (id, key) in keys.into_iter().enumerate() {
        let (v, c) = build_validator((id + 1) as u64, key, genesis.clone()).await;
        log::info!(
            "Validator #{}: {}",
            v.validator_id(),
            c.network.local_peer_id()
        );
        validators.push(v);
        consensus.push(c);
    }

    // Start consensus.
    for consensus in consensus {
        // Tell the network to connect to seed nodes
        let seed = multiaddr![Memory(1u64)];
        log::debug!("Dialing seed: {:?}", seed);
        consensus
            .network
            .dial_address(seed)
            .await
            .expect("Failed to dial seed");

        tokio::spawn(consensus);
    }

    validators
}

async fn mock_consensus(
    hub: &mut MockHub,
    peer_id: u64,
    genesis_info: GenesisInfo,
) -> MockConsensus {
    let env = VolatileEnvironment::new(12).unwrap();
    let time = Arc::new(OffsetTime::new());
    let blockchain = Arc::new(
        Blockchain::with_genesis(
            env.clone(),
            time,
            NetworkId::UnitAlbatross,
            genesis_info.block,
            genesis_info.accounts,
        )
        .unwrap(),
    );
    let mempool = Mempool::new(Arc::clone(&blockchain), MempoolConfig::default());
    let network = Arc::new(hub.new_network_with_address(peer_id));
    let sync_protocol =
        HistorySync::<MockNetwork>::new(Arc::clone(&blockchain), network.subscribe_events());
    MockConsensus::from_network(env, blockchain, mempool, network, Box::pin(sync_protocol)).await
}

pub async fn mock_validator(
    hub: &mut MockHub,
    peer_id: u64,
    signing_key: KeyPair,
    genesis_info: GenesisInfo,
) -> (MockValidator, MockConsensus) {
    let consensus = mock_consensus(hub, peer_id, genesis_info).await;
    let validator_network = Arc::new(ValidatorNetworkImpl::new(consensus.network.clone()));
    (
        MockValidator::new(&consensus, validator_network, signing_key, None),
        consensus,
    )
}

pub async fn mock_validators(hub: &mut MockHub, num_validators: usize) -> Vec<MockValidator> {
    // Generate validator key pairs.
    let mut rng = seeded_rng(0);
    let keys: Vec<KeyPair> = (0..num_validators)
        .map(|_| KeyPair::generate(&mut rng))
        .collect();

    // Generate genesis block.
    let mut genesis_builder = GenesisBuilder::default();
    for key in &keys {
        genesis_builder.with_genesis_validator(
            key.public_key.hash::<Blake2bHash>().as_slice()[0..20].into(),
            key.public_key,
            Address::default(),
            Coin::from_u64_unchecked(10000),
        );
    }
    let genesis = genesis_builder.generate().unwrap();

    // Instantiate validators.
    let mut validators = vec![];
    let mut consensus = vec![];
    for (id, key) in keys.into_iter().enumerate() {
        let (v, c) = mock_validator(hub, id as u64, key, genesis.clone()).await;
        validators.push(v);
        consensus.push(c);
    }

    // Connect validators to each other.
    for id in 0..num_validators {
        let validator = validators.get(id).unwrap();
        for other_id in (id + 1)..num_validators {
            let other_validator = validators.get(other_id).unwrap();
            validator
                .consensus
                .network
                .dial_mock(&other_validator.consensus.network);
        }
    }

    // Wait until validators are connected.
    let mut events: Vec<BroadcastStream<ConsensusEvent>> =
        consensus.iter().map(|v| v.subscribe_events()).collect();

    // Start consensus.
    for consensus in consensus {
        tokio::spawn(consensus);
    }

    future::join_all(events.iter_mut().map(|e| e.next())).await;

    validators
}

pub fn validator_for_slot(
    validators: &Vec<MockValidator>,
    block_number: u32,
    view_number: u32,
) -> &MockValidator {
    let consensus = &validators.first().unwrap().consensus;

    let (slot, _) = consensus
        .blockchain
        .get_slot_owner_at(block_number, view_number, None)
        .expect("Couldn't find slot owner!");

    validators
        .iter()
        .find(|validator| {
            &validator.signing_key().public_key.compress() == slot.public_key.compressed()
        })
        .unwrap()
}
