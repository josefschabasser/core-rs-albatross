use std::sync::Arc;

use nimiq_blockchain::Blockchain;
use nimiq_build_tools::genesis::GenesisInfo;
use nimiq_consensus::sync::history::HistorySync;
use nimiq_consensus::Consensus as AbstractConsensus;
use nimiq_database::volatile::VolatileEnvironment;
use nimiq_mempool::{Mempool, MempoolConfig};
use nimiq_network_interface::network::Network as NetworkInterface;
use nimiq_network_mock::MockHub;
use nimiq_primitives::networks::NetworkId;
use nimiq_utils::time::OffsetTime;

use crate::test_network::TestNetwork;

pub struct Node<N: NetworkInterface + TestNetwork> {
    pub network: Arc<N>,
    pub blockchain: Arc<Blockchain>,
    pub mempool: Arc<Mempool>,
    pub consensus: Option<AbstractConsensus<N>>,
}

impl<N: NetworkInterface + TestNetwork> Node<N> {
    pub async fn new(peer_id: u64, genesis_info: GenesisInfo, hub: &mut Option<MockHub>) -> Self {
        let env = VolatileEnvironment::new(12).unwrap();
        let clock = Arc::new(OffsetTime::new());
        let blockchain = Arc::new(
            Blockchain::with_genesis(
                env.clone(),
                Arc::clone(&clock),
                NetworkId::UnitAlbatross,
                genesis_info.block,
                genesis_info.accounts,
            )
            .unwrap(),
        );
        let mempool = Mempool::new(Arc::clone(&blockchain), MempoolConfig::default());

        let network = N::build_network(peer_id, genesis_info.hash, hub).await;

        let sync_protocol =
            HistorySync::<N>::new(Arc::clone(&blockchain), network.subscribe_events());
        let consensus = AbstractConsensus::<N>::with_min_peers(
            env,
            Arc::clone(&blockchain),
            Arc::clone(&mempool),
            Arc::clone(&network),
            Box::pin(sync_protocol),
            1,
        )
        .await;

        Node {
            network,
            blockchain,
            mempool,
            consensus: Some(consensus),
        }
    }

    pub fn consume(&mut self) {
        if let Some(consensus) = self.consensus.take() {
            tokio::spawn(consensus);
        }
    }
}
