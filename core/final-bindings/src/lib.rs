use lightning_application::app::Application;
use lightning_archive::archive::Archive;
use lightning_blockstore::blockstore::Blockstore;
use lightning_blockstore_server::BlockstoreServer;
use lightning_broadcast::Broadcast;
use lightning_consensus::consensus::Consensus;
use lightning_fetcher::fetcher::Fetcher;
use lightning_forwarder::Forwarder;
use lightning_handshake::handshake::Handshake;
use lightning_indexer::Indexer;
use lightning_interfaces::partial;
use lightning_keystore::Keystore;
use lightning_notifier::Notifier;
use lightning_origin_demuxer::OriginDemuxer;
use lightning_pinger::Pinger;
use lightning_pool::PoolProvider;
use lightning_rep_collector::ReputationAggregator;
use lightning_resolver::resolver::Resolver;
use lightning_rpc::Rpc;
use lightning_service_executor::shim::ServiceExecutor;
use lightning_signer::Signer;
use lightning_syncronizer::syncronizer::Syncronizer;
use lightning_task_broker::TaskBroker;
use lightning_test_utils::consensus::{MockConsensus, MockForwarder};
use lightning_topology::Topology;
use lightning_utils::config::TomlConfigProvider;

partial!(FinalTypes require full {
    ForwarderInterface = Forwarder<Self>;
    ConsensusInterface = Consensus<Self>;
    ConfigProviderInterface = TomlConfigProvider<Self>;
    ApplicationInterface = Application<Self>;
    BlockstoreInterface = Blockstore<Self>;
    BlockstoreServerInterface = BlockstoreServer<Self>;
    SyncronizerInterface = Syncronizer<Self>;
    BroadcastInterface = Broadcast<Self>;
    TopologyInterface = Topology<Self>;
    ArchiveInterface = Archive<Self>;
    HandshakeInterface = Handshake<Self>;
    NotifierInterface = Notifier<Self>;
    OriginProviderInterface = OriginDemuxer<Self>;
    ReputationAggregatorInterface = ReputationAggregator<Self>;
    ResolverInterface = Resolver<Self>;
    RpcInterface = Rpc<Self>;
    ServiceExecutorInterface = ServiceExecutor<Self>;
    TaskBrokerInterface = TaskBroker<Self>;
    KeystoreInterface = Keystore<Self>;
    SignerInterface = Signer<Self>;
    FetcherInterface = Fetcher<Self>;
    PoolInterface = PoolProvider<Self>;
    PingerInterface = Pinger<Self>;
    IndexerInterface = Indexer<Self>;
    DeliveryAcknowledgmentAggregatorInterface = lightning_interfaces::_hacks::Blanket;
});

partial!(UseMockConsensus require full {
    ConsensusInterface = MockConsensus<Self>;
    ForwarderInterface = MockForwarder<Self>;
    ConfigProviderInterface = TomlConfigProvider<Self>;
    ApplicationInterface = Application<Self>;
    BlockstoreInterface = Blockstore<Self>;
    BlockstoreServerInterface = BlockstoreServer<Self>;
    SyncronizerInterface = Syncronizer<Self>;
    BroadcastInterface = Broadcast<Self>;
    TopologyInterface = Topology<Self>;
    ArchiveInterface = Archive<Self>;
    HandshakeInterface = Handshake<Self>;
    NotifierInterface = Notifier<Self>;
    OriginProviderInterface = OriginDemuxer<Self>;
    ReputationAggregatorInterface = ReputationAggregator<Self>;
    ResolverInterface = Resolver<Self>;
    RpcInterface = Rpc<Self>;
    ServiceExecutorInterface = ServiceExecutor<Self>;
    TaskBrokerInterface = TaskBroker<Self>;
    KeystoreInterface = Keystore<Self>;
    SignerInterface = Signer<Self>;
    FetcherInterface = Fetcher<Self>;
    PoolInterface = PoolProvider<Self>;
    PingerInterface = Pinger<Self>;
    IndexerInterface = Indexer<Self>;
    DeliveryAcknowledgmentAggregatorInterface = lightning_interfaces::_hacks::Blanket;
});
