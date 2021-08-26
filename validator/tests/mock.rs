use futures::{future, StreamExt};
use rand::prelude::StdRng;
use rand::SeedableRng;
use tokio::time;

use nimiq_block::{MultiSignature, SignedViewChange, ViewChange};
use nimiq_blockchain::{AbstractBlockchain, BlockchainEvent};
use nimiq_bls::{AggregateSignature, KeyPair};
use nimiq_build_tools::genesis::GenesisBuilder;
use nimiq_collections::BitSet;
use nimiq_handel::update::{LevelUpdate, LevelUpdateMessage};
use nimiq_keys::{Address, SecureGenerate};
use nimiq_network_interface::network::Network;
use nimiq_network_mock::{MockHub, MockNetwork};
use nimiq_primitives::account::ValidatorId;
use nimiq_primitives::coin::Coin;
use nimiq_test_utils::validator::{mock_validator, mock_validators, validator_for_slot};
use nimiq_validator::aggregation::view_change::SignedViewChangeMessage;
use nimiq_vrf::VrfSeed;
use std::sync::Arc;
use std::time::Duration;

fn seeded_rng(seed: u64) -> StdRng {
    StdRng::seed_from_u64(seed)
}

#[tokio::test]
async fn one_validator_can_create_micro_blocks() {
    let mut hub = MockHub::default();

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

    let (validator, mut consensus1) = mock_validator(&mut hub, 1, key, genesis.clone()).await;

    log::debug!("Establishing consensus...");
    consensus1.force_established();
    assert_eq!(consensus1.is_established(), true);

    log::debug!("Spawning validator...");
    tokio::spawn(validator);

    let events1 = consensus1.blockchain.notifier.write().as_stream();
    events1.take(10).for_each(|_| future::ready(())).await;

    assert!(consensus1.blockchain.block_number() >= 10);
}

#[tokio::test]
async fn four_validators_can_create_micro_blocks() {
    let mut hub = MockHub::default();

    let validators = mock_validators(&mut hub, 4).await;

    let blockchain = Arc::clone(&validators.first().unwrap().consensus.blockchain);

    tokio::spawn(future::join_all(validators));

    let events = blockchain.notifier.write().as_stream();
    time::timeout(
        Duration::from_secs(60),
        events.take(30).for_each(|_| future::ready(())),
    )
    .await
    .unwrap();

    assert!(blockchain.block_number() >= 30);
}

#[tokio::test]
async fn four_validators_can_view_change() {
    let mut hub = MockHub::default();

    let validators = mock_validators(&mut hub, 4).await;

    // Disconnect the next block producer.
    let validator = validator_for_slot(&validators, 1, 0);
    validator.consensus.network.disconnect();

    // Listen for blockchain events from the new block producer (after view change).
    let validator = validator_for_slot(&validators, 1, 1);
    let blockchain = Arc::clone(&validator.consensus.blockchain);
    let mut events = blockchain.notifier.write().as_stream();

    // Freeze time to immediately trigger the view change timeout.
    // time::pause();

    tokio::spawn(future::join_all(validators));

    // Wait for the new block producer to create a block.
    events.next().await;

    assert!(blockchain.block_number() >= 1);
    assert_eq!(blockchain.view_number(), 1);
}

fn create_view_change_update(
    block_number: u32,
    new_view_number: u32,
    prev_seed: VrfSeed,
    key_pair: KeyPair,
    validator_id: u16,
    slots: &Vec<u16>,
) -> LevelUpdateMessage<SignedViewChangeMessage, ViewChange> {
    // create view change according to parameters
    let view_change = ViewChange {
        block_number,
        new_view_number,
        prev_seed,
    };

    // get a single signature for this view_change
    let signed_view_change =
        SignedViewChange::from_message(view_change.clone(), &key_pair.secret_key, validator_id);

    // multiply with number of slots to get a signature representing all the slots of this public_key
    let signature = AggregateSignature::from_signatures(&[signed_view_change
        .signature
        .multiply(slots.len() as u16)]);

    // compute the signers bitset (which is just all the slots)
    let mut signers = BitSet::new();
    for slot in slots {
        signers.insert(*slot as usize);
    }

    // the contribution is composed of the signers bitset with the signature already multiplied by the number of slots.
    let contribution = SignedViewChangeMessage {
        view_change: MultiSignature::new(signature, signers),
        previous_proof: None,
    };

    LevelUpdate::new(
        contribution.clone(),
        Some(contribution),
        1,
        validator_id as usize,
    )
    .with_tag(view_change)
}

#[tokio::test]
async fn validator_can_catch_up() {
    // remove first block producer in order to trigger a view change. Never connect him again
    // remove the second block producer to trigger another view change after the first one (which we want someone to catch up to). Never connect him again
    // third block producer needs to be disconnected as well and then reconnected to catch up to the seconds view change while not having seen the first one,
    // resulting in him producing the first block.
    let mut hub = MockHub::default();

    // In total 8 validator are registered. after 3 validators are taken offline the remaining 5 should not be able to progress on their own
    let mut validators = mock_validators(&mut hub, 8).await;
    // Maintain a collection of the correspponding networks.

    let networks: Vec<Arc<MockNetwork>> = validators
        .iter()
        .map(|v| v.consensus.network.clone())
        .collect();

    // Disconnect the block producers for the next 3 views. remember the one which is supposed to actually create the block (3rd view)
    let (validator, nw) = {
        let validator = validator_for_slot(&mut validators, 1, 0);
        validator.consensus.network.disconnect();
        let id1 = validator.validator_id();
        let validator = validator_for_slot(&mut validators, 1, 1);
        validator.consensus.network.disconnect();
        let id2 = validator.validator_id();
        assert_ne!(id2, id1);

        // ideally we would remove the validators from the vec for them to not even execute.
        // However the implementation does still progress their chains and since they have registered listeners, they would panic.
        // that is confusing, thus they are allowed to execute (with no validator network connection)
        // validators.retain(|v| {
        //     v.validator_id() != id1 && v.validator_id() != id2
        // });

        let validator = validator_for_slot(&validators, 1, 2);
        validator.consensus.network.disconnect();
        assert_ne!(id1, validator.validator_id());
        assert_ne!(id2, validator.validator_id());
        (validator, validator.consensus.network.clone())
    };
    // assert_eq!(validators.len(), 7);

    let blockchain = validator.consensus.blockchain.clone();
    // Listen for blockchain events from the block producer (after two view changes).
    let mut events = blockchain.notifier.write().as_stream();

    let (start, end) = blockchain.current_validators().unwrap().validators
        [validator.validator_id() as usize]
        .slot_range;

    let slots = (start..end).collect();

    // Manually construct a view change for the validator
    let vc = create_view_change_update(
        1,
        1,
        blockchain.head().seed().clone(),
        validator.signing_key(),
        validator.validator_id(),
        &slots,
    );

    // let the validators run.
    tokio::spawn(future::join_all(validators));

    // while waiting for them to run into the view_change_timeout (10s)
    time::sleep(Duration::from_secs(11)).await;
    // At which point the prepared view_change message is broadcast
    // (only a subset of the validators will accept it as it send as level 1 message)
    for network in &networks {
        network.broadcast(&vc).await;
    }

    // wait enough time to complete the view change (it really does not matter how long, as long as the vc completes)
    time::sleep(Duration::from_secs(8)).await;

    // reconnect a validator (who has not seen the proof for the ViewChange to view 1)
    for network in &networks {
        log::warn!("connecting networks");
        nw.dial_mock(network);
    }

    // Wait for the new block producer to create a blockchainEvent (which is always an extended event for block 1) and keep the hash
    if let Some(BlockchainEvent::Extended(hash)) = events.next().await {
        // retrieve the block for height 1
        if let Some(block) = blockchain.get_block_at(1, false, None) {
            // the hash needs to be the one the extended event returned.
            // (the chain itself i.e blockchain.header_hash() might have already progressed further)
            assert_eq!(block.header().hash(), hash);
            // the view of the block needs to be 2
            assert_eq!(block.header().view_number(), 2);
            // now in that case the validator producing this block has progressed the 2nd view change to view 2 without having seen the view change to view 1.
            return;
        }
    }

    assert!(false);
}
