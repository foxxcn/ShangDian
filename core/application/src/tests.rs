use std::collections::BTreeMap;
use std::net::IpAddr;
use std::time::SystemTime;

use affair::Socket;
use anyhow::{anyhow, Result};
use fleek_crypto::{
    AccountOwnerSecretKey,
    ConsensusPublicKey,
    ConsensusSecretKey,
    EthAddress,
    NodePublicKey,
    NodeSecretKey,
    SecretKey,
};
use hp_fixed::unsigned::HpUfixed;
use lightning_interfaces::infu_collection::Collection;
use lightning_interfaces::types::{
    Block,
    BlockExecutionResponse,
    DeliveryAcknowledgment,
    ExecutionData,
    ExecutionError,
    HandshakePorts,
    NodePorts,
    Participation,
    ProofOfConsensus,
    ProtocolParams,
    ReputationMeasurements,
    Tokens,
    TotalServed,
    TransactionRequest,
    TransactionResponse,
    UpdateMethod,
    UpdatePayload,
    UpdateRequest,
};
use lightning_interfaces::{
    partial,
    ApplicationInterface,
    ExecutionEngineSocket,
    PagingParams,
    SyncQueryRunnerInterface,
    ToDigest,
};
use lightning_test_utils::{random, reputation};

use crate::app::Application;
use crate::config::{Config, Mode, StorageConfig};
use crate::genesis::{Genesis, GenesisNode};
use crate::query_runner::QueryRunner;

partial!(TestBinding {
    ApplicationInterface = Application<Self>;
});

pub struct Params {
    epoch_time: Option<u64>,
    max_inflation: Option<u16>,
    protocol_share: Option<u16>,
    node_share: Option<u16>,
    service_builder_share: Option<u16>,
    max_boost: Option<u16>,
    supply_at_genesis: Option<u64>,
}

// This is a helper struct for keeping track of a node's private keys.
// Many tests require us to submit transactions.
#[derive(Clone)]
struct GenesisCommitteeKeystore {
    _owner_secret_key: AccountOwnerSecretKey,
    node_secret_key: NodeSecretKey,
    _consensus_secret_key: ConsensusSecretKey,
    _worker_secret_key: NodeSecretKey,
}

macro_rules! run_transaction {
    ($tx:expr,$socket:expr) => {{
        let updates = vec![$tx.into()];
        run_updates!(updates, $socket)
    }};
}

macro_rules! run_transactions {
    ($txs:expr,$socket:expr) => {{
        let updates = $txs.into_iter().map(|update| update.into()).collect();
        run_updates!(updates, $socket)
    }};
}

macro_rules! run_updates {
    ($updates:expr,$socket:expr) => {{
        let result = run_transaction($updates, $socket).await;
        assert!(result.is_ok());
        result.unwrap()
    }};
}

macro_rules! expect_tx_success {
    ($tx:expr,$socket:expr,$response:expr) => {{
        let result = run_transaction!($tx, $socket);
        assert_eq!(
            result.txn_receipts[0].response,
            TransactionResponse::Success($response)
        );
    }};
}

macro_rules! expect_tx_revert {
    ($tx:expr,$socket:expr,$revert:expr) => {{
        let result = run_transaction!($tx, $socket);
        assert_eq!(
            result.txn_receipts[0].response,
            TransactionResponse::Revert($revert)
        );
    }};
}

macro_rules! change_epoch {
    ($socket:expr,$secret_key:expr,$account_nonce:expr,$epoch:expr) => {{
        let req = get_update_request_node(
            UpdateMethod::ChangeEpoch { epoch: $epoch },
            $secret_key,
            $account_nonce,
        );
        run_transaction!(req, $socket)
    }};
}

macro_rules! assert_valid_node {
    ($valid_nodes:expr,$query_runner:expr,$node_pk:expr) => {{
        let node_info = $query_runner.get_node_info($node_pk).unwrap();
        // Node registry contains the first valid node
        assert!($valid_nodes.contains(&node_info));
    }};
}

macro_rules! assert_not_valid_node {
    ($valid_nodes:expr,$query_runner:expr,$node_pk:expr) => {{
        let node_info = $query_runner.get_node_info($node_pk).unwrap();
        // Node registry contains the first valid node
        assert!(!$valid_nodes.contains(&node_info));
    }};
}

macro_rules! assert_paging_node_registry {
    ($query_runner:expr,$paging_params:expr, $expected_len:expr) => {{
        let valid_nodes = $query_runner.get_node_registry(Some($paging_params));
        assert_eq!(valid_nodes.len(), $expected_len);
    }};
}

// Helper macro that performs an epoch change.
// In order to submit the `ChangeEpoch` transactions, this function needs access to the committee's
// private keys. These are supplied in the `committee_keystore`.
macro_rules! simple_epoch_change {
    ($socket:expr,$committee_keystore:expr,$query_runner:expr,$epoch:expr) => {{
        let required_signals = calculate_required_signals($committee_keystore.len());
        // make call epoch change for 2/3rd committe members
        for (index, node) in $committee_keystore
            .iter()
            .enumerate()
            .take(required_signals)
        {
            let nonce = $query_runner
                .get_node_info(&node.node_secret_key.to_pk())
                .unwrap()
                .nonce
                + 1;
            let req = prepare_change_epoch_request($epoch, &node.node_secret_key, nonce);

            let res = run_transaction!(req, $socket);
            // check epoch change
            if index == required_signals - 1 {
                assert!(res.change_epoch);
            }
        }
    }};
}

macro_rules! submit_reputation_measurements {
    ($socket:expr,$secret_key:expr,$account_nonce:expr,$measurements:expr) => {{
        let req = get_update_request_node(
            UpdateMethod::SubmitReputationMeasurements {
                measurements: $measurements,
            },
            $secret_key,
            $account_nonce,
        );
        run_transaction!(req, $socket)
    }};
}

macro_rules! assert_rep_measurements_update {
    ($query_runner:expr,$update:expr,$reporting_node_index:expr) => {{
        let rep_measurements = $query_runner.get_rep_measurements(&$update.0);
        assert_eq!(rep_measurements.len(), 1);
        assert_eq!(rep_measurements[0].reporting_node, $reporting_node_index);
        assert_eq!(rep_measurements[0].measurements, $update.1);
    }};
}

macro_rules! deposit {
    ($socket:expr,$secret_key:expr,$account_nonce:expr,$amount:expr) => {{
        let req = prepare_deposit_update($amount, $secret_key, $account_nonce);
        expect_tx_success!(req, $socket, ExecutionData::None)
    }};
}

macro_rules! stake {
    ($socket:expr,$secret_key:expr,$nonce:expr,$amount:expr,$node_pk:expr,$consensus_key:expr) => {{
        let req = prepare_initial_stake_update(
            $amount,
            $node_pk,
            $consensus_key,
            "127.0.0.1".parse().unwrap(),
            [0; 32].into(),
            "127.0.0.1".parse().unwrap(),
            NodePorts::default(),
            $secret_key,
            $nonce,
        );

        expect_tx_success!(req, $socket, ExecutionData::None)
    }};
}

macro_rules! stake_lock {
    ($socket:expr,$secret_key:expr,$nonce:expr,$node_pk:expr,$locked_for:expr) => {{
        let req = prepare_stake_lock_request($locked_for, $node_pk, $secret_key, $nonce);
        expect_tx_success!(req, $socket, ExecutionData::None)
    }};
}

fn init_app(config: Option<Config>) -> (ExecutionEngineSocket, QueryRunner) {
    let config = config.or(Some(Config {
        genesis: None,
        mode: Mode::Dev,
        testnet: false,
        storage: StorageConfig::InMemory,
        db_path: None,
        db_options: None,
    }));
    do_init_app(config.unwrap())
}

fn do_init_app(config: Config) -> (ExecutionEngineSocket, QueryRunner) {
    let app = Application::<TestBinding>::init(config, Default::default()).unwrap();

    (app.transaction_executor(), app.sync_query())
}
fn test_genesis() -> Genesis {
    Genesis::load().expect("Failed to load genesis from file.")
}

fn test_init_app(committee: Vec<GenesisNode>) -> (ExecutionEngineSocket, QueryRunner) {
    let mut genesis = test_genesis();
    genesis.node_info = committee;
    init_app(Some(test_config(genesis)))
}

fn init_app_with_genesis(genesis: &Genesis) -> (ExecutionEngineSocket, QueryRunner) {
    init_app(Some(test_config(genesis.clone())))
}

fn init_app_with_params(
    params: Params,
    committee: Option<Vec<GenesisNode>>,
) -> (ExecutionEngineSocket, QueryRunner) {
    let mut genesis = test_genesis();

    if let Some(committee) = committee {
        genesis.node_info = committee;
    }

    genesis.epoch_start = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    if let Some(epoch_time) = params.epoch_time {
        genesis.epoch_time = epoch_time;
    }

    if let Some(max_inflation) = params.max_inflation {
        genesis.max_inflation = max_inflation;
    }

    if let Some(protocol_share) = params.protocol_share {
        genesis.protocol_share = protocol_share;
    }

    if let Some(node_share) = params.node_share {
        genesis.node_share = node_share;
    }

    if let Some(service_builder_share) = params.service_builder_share {
        genesis.service_builder_share = service_builder_share;
    }

    if let Some(max_boost) = params.max_boost {
        genesis.max_boost = max_boost;
    }

    if let Some(supply_at_genesis) = params.supply_at_genesis {
        genesis.supply_at_genesis = supply_at_genesis;
    }
    let config = Config {
        genesis: Some(genesis),
        mode: Mode::Test,
        testnet: false,
        storage: StorageConfig::InMemory,
        db_path: None,
        db_options: None,
    };

    init_app(Some(config))
}

fn test_config(genesis: Genesis) -> Config {
    Config {
        genesis: Some(genesis),
        mode: Mode::Test,
        testnet: false,
        storage: StorageConfig::InMemory,
        db_path: None,
        db_options: None,
    }
}
fn test_reputation_measurements(uptime: u8) -> ReputationMeasurements {
    ReputationMeasurements {
        latency: None,
        interactions: None,
        inbound_bandwidth: None,
        outbound_bandwidth: None,
        bytes_received: None,
        bytes_sent: None,
        uptime: Some(uptime),
        hops: None,
    }
}

fn calculate_required_signals(committee_size: usize) -> usize {
    2 * committee_size / 3 + 1
}

// Helper function to create a genesis committee.
// This is useful for tests where we need to seed the application state with nodes.
fn create_genesis_committee(
    num_members: usize,
) -> (Vec<GenesisNode>, Vec<GenesisCommitteeKeystore>) {
    let mut keystore = Vec::new();
    let mut committee = Vec::new();
    (0..num_members as u16).for_each(|i| {
        let node_secret_key = NodeSecretKey::generate();
        let consensus_secret_key = ConsensusSecretKey::generate();
        let owner_secret_key = AccountOwnerSecretKey::generate();
        let node = create_committee_member(
            &owner_secret_key,
            &node_secret_key,
            &consensus_secret_key,
            i,
        );
        committee.push(node);
        keystore.push(GenesisCommitteeKeystore {
            _owner_secret_key: owner_secret_key,
            _worker_secret_key: node_secret_key.clone(),
            node_secret_key,
            _consensus_secret_key: consensus_secret_key,
        });
    });
    (committee, keystore)
}

fn create_committee_member(
    owner_secret_key: &AccountOwnerSecretKey,
    node_secret_key: &NodeSecretKey,
    consensus_secret_key: &ConsensusSecretKey,
    index: u16,
) -> GenesisNode {
    let node_public_key = node_secret_key.to_pk();
    let consensus_public_key = consensus_secret_key.to_pk();
    let owner_public_key = owner_secret_key.to_pk();
    GenesisNode::new(
        owner_public_key.into(),
        node_public_key,
        "127.0.0.1".parse().unwrap(),
        consensus_public_key,
        "127.0.0.1".parse().unwrap(),
        node_public_key,
        NodePorts {
            primary: 8000 + index,
            worker: 9000 + index,
            mempool: 7000 + index,
            rpc: 6000 + index,
            pool: 5000 + index,
            dht: 4000 + index,
            pinger: 2000 + index,
            handshake: HandshakePorts {
                http: 5000 + index,
                webrtc: 6000 + index,
                webtransport: 7000 + index,
            },
        },
        None,
        true,
    )
}

// Helper function to create an update request from a update method.
fn get_update_request_node(
    method: UpdateMethod,
    secret_key: &NodeSecretKey,
    nonce: u64,
) -> UpdateRequest {
    let payload = UpdatePayload {
        sender: secret_key.to_pk().into(),
        nonce,
        method,
    };
    let digest = payload.to_digest();
    let signature = secret_key.sign(&digest);
    UpdateRequest {
        signature: signature.into(),
        payload,
    }
}

// Passing the private key around like this should only be done for
// testing.
fn get_update_request_account(
    method: UpdateMethod,
    secret_key: &AccountOwnerSecretKey,
    nonce: u64,
) -> UpdateRequest {
    let payload = UpdatePayload {
        sender: secret_key.to_pk().into(),
        nonce,
        method,
    };
    let digest = payload.to_digest();
    let signature = secret_key.sign(&digest);
    UpdateRequest {
        signature: signature.into(),
        payload,
    }
}

fn prepare_deposit_update(
    amount: &HpUfixed<18>,
    secret_key: &AccountOwnerSecretKey,
    nonce: u64,
) -> UpdateRequest {
    get_update_request_account(
        UpdateMethod::Deposit {
            proof: ProofOfConsensus {},
            token: Tokens::FLK,
            amount: amount.clone(),
        },
        secret_key,
        nonce,
    )
}

fn prepare_regular_stake_update(
    amount: &HpUfixed<18>,
    node_public_key: &NodePublicKey,
    secret_key: &AccountOwnerSecretKey,
    nonce: u64,
) -> UpdateRequest {
    get_update_request_account(
        UpdateMethod::Stake {
            amount: amount.clone(),
            node_public_key: *node_public_key,
            consensus_key: None,
            node_domain: None,
            worker_public_key: None,
            worker_domain: None,
            ports: None,
        },
        secret_key,
        nonce,
    )
}

#[allow(clippy::too_many_arguments)]
fn prepare_initial_stake_update(
    amount: &HpUfixed<18>,
    node_public_key: &NodePublicKey,
    consensus_key: ConsensusPublicKey,
    node_domain: IpAddr,
    worker_pub_key: NodePublicKey,
    worker_domain: IpAddr,
    ports: NodePorts,
    secret_key: &AccountOwnerSecretKey,
    nonce: u64,
) -> UpdateRequest {
    get_update_request_account(
        UpdateMethod::Stake {
            amount: amount.clone(),
            node_public_key: *node_public_key,
            consensus_key: Some(consensus_key),
            node_domain: Some(node_domain),
            worker_public_key: Some(worker_pub_key),
            worker_domain: Some(worker_domain),
            ports: Some(ports),
        },
        secret_key,
        nonce,
    )
}

fn prepare_unstake_update(
    amount: &HpUfixed<18>,
    node_public_key: &NodePublicKey,
    secret_key: &AccountOwnerSecretKey,
    nonce: u64,
) -> UpdateRequest {
    get_update_request_account(
        UpdateMethod::Unstake {
            amount: amount.clone(),
            node: *node_public_key,
        },
        secret_key,
        nonce,
    )
}

fn prepare_withdraw_unstaked_update(
    node_public_key: &NodePublicKey,
    recipient: Option<EthAddress>,
    secret_key: &AccountOwnerSecretKey,
    nonce: u64,
) -> UpdateRequest {
    get_update_request_account(
        UpdateMethod::WithdrawUnstaked {
            node: *node_public_key,
            recipient,
        },
        secret_key,
        nonce,
    )
}

fn prepare_stake_lock_update(
    node_public_key: &NodePublicKey,
    locked_for: u64,
    secret_key: &AccountOwnerSecretKey,
    nonce: u64,
) -> UpdateRequest {
    get_update_request_account(
        UpdateMethod::StakeLock {
            node: *node_public_key,
            locked_for,
        },
        secret_key,
        nonce,
    )
}

// Helper methods for tests
// Passing the private key around like this should only be done for
// testing.
fn prepare_pod_request(
    commodity: u128,
    service_id: u32,
    secret_key: &NodeSecretKey,
    nonce: u64,
) -> UpdateRequest {
    get_update_request_node(
        UpdateMethod::SubmitDeliveryAcknowledgmentAggregation {
            commodity,  // units of data served
            service_id, // service 0 serving bandwidth
            proofs: vec![DeliveryAcknowledgment],
            metadata: None,
        },
        secret_key,
        nonce,
    )
}

fn prepare_stake_lock_request(
    locked_for: u64,
    node: &NodePublicKey,
    secret_key: &AccountOwnerSecretKey,
    nonce: u64,
) -> UpdateRequest {
    // Deposit some FLK into account 1
    get_update_request_account(
        UpdateMethod::StakeLock {
            node: *node,
            locked_for,
        },
        secret_key,
        nonce,
    )
}

fn prepare_change_epoch_request(
    epoch: u64,
    secret_key: &NodeSecretKey,
    nonce: u64,
) -> UpdateRequest {
    get_update_request_node(UpdateMethod::ChangeEpoch { epoch }, secret_key, nonce)
}

fn prepare_transfer_request(
    amount: &HpUfixed<18>,
    to: &EthAddress,
    secret_key: &AccountOwnerSecretKey,
    nonce: u64,
) -> UpdateRequest {
    get_update_request_account(
        UpdateMethod::Transfer {
            amount: amount.clone(),
            token: Tokens::FLK,
            to: *to,
        },
        secret_key,
        nonce,
    )
}

fn prepare_change_protocol_param_request(
    param: &ProtocolParams,
    value: &u128,
    secret_key: &AccountOwnerSecretKey,
    nonce: u64,
) -> UpdateRequest {
    get_update_request_account(
        UpdateMethod::ChangeProtocolParam {
            param: param.clone(),
            value: *value,
        },
        secret_key,
        nonce,
    )
}

// Helper function that submits a transaction to the application.
async fn run_transaction(
    requests: Vec<TransactionRequest>,
    update_socket: &Socket<Block, BlockExecutionResponse>,
) -> Result<BlockExecutionResponse> {
    let res = update_socket
        .run(Block {
            transactions: requests,
            digest: [0; 32],
        })
        .await
        .map_err(|r| anyhow!(format!("{r:?}")))?;
    Ok(res)
}

fn update_reputation_measurements(
    query_runner: &QueryRunner,
    map: &mut BTreeMap<u32, ReputationMeasurements>,
    peer: NodePublicKey,
    measurements: ReputationMeasurements,
) -> (u32, ReputationMeasurements) {
    let peer_index = query_runner.pubkey_to_index(peer).unwrap();
    map.insert(peer_index, measurements.clone());
    (peer_index, measurements)
}

fn paging_params(ignore_stake: bool, start: u32, limit: usize) -> PagingParams {
    PagingParams {
        ignore_stake,
        start,
        limit,
    }
}
//////////////////////////////////////////////////////////////////////////////////
////////////////// This is where the actual tests are defined ////////////////////
//////////////////////////////////////////////////////////////////////////////////

#[tokio::test]
async fn test_epoch_change() {
    // Create a genesis committee and seed the application state with it.
    let committee_size = 4;
    let (committee, keystore) = create_genesis_committee(committee_size);
    let (update_socket, query_runner) = test_init_app(committee);
    let required_signals = calculate_required_signals(committee_size);

    let epoch = 0;
    let nonce = 1;

    // Have (required_signals - 1) say they are ready to change epoch
    // make sure the epoch doesnt change each time someone signals
    for node in keystore.iter().take(required_signals - 1) {
        // Make sure epoch didnt change
        let res = change_epoch!(&update_socket, &node.node_secret_key, nonce, epoch);
        assert!(!res.change_epoch);
    }
    // check that the current epoch is still 0
    assert_eq!(query_runner.get_epoch_info().epoch, 0);

    // Have the last needed committee member signal the epoch change and make sure it changes
    let res = change_epoch!(
        &update_socket,
        &keystore[required_signals].node_secret_key,
        nonce,
        epoch
    );
    assert!(res.change_epoch);

    // Query epoch info and make sure it incremented to new epoch
    assert_eq!(query_runner.get_epoch_info().epoch, 1);
}

#[tokio::test]
async fn test_submit_rep_measurements() {
    let committee_size = 4;
    let (committee, keystore) = create_genesis_committee(committee_size);
    let (update_socket, query_runner) = test_init_app(committee);
    let mut rng = random::get_seedable_rng();

    let mut map = BTreeMap::new();
    let update1 = update_reputation_measurements(
        &query_runner,
        &mut map,
        keystore[1].node_secret_key.to_pk(),
        reputation::generate_reputation_measurements(&mut rng, 0.1),
    );
    let update2 = update_reputation_measurements(
        &query_runner,
        &mut map,
        keystore[2].node_secret_key.to_pk(),
        reputation::generate_reputation_measurements(&mut rng, 0.1),
    );

    let reporting_node_key = keystore[0].node_secret_key.to_pk();
    let reporting_node_index = query_runner.pubkey_to_index(reporting_node_key).unwrap();

    submit_reputation_measurements!(&update_socket, &keystore[0].node_secret_key, 1, map);

    assert_rep_measurements_update!(&query_runner, update1, reporting_node_index);
    assert_rep_measurements_update!(&query_runner, update2, reporting_node_index);
}

#[tokio::test]
async fn test_submit_rep_measurements_twice() {
    let committee_size = 4;
    let (committee, keystore) = create_genesis_committee(committee_size);
    let (update_socket, query_runner) = test_init_app(committee);

    let mut rng = random::get_seedable_rng();

    let mut map = BTreeMap::new();
    let _ = update_reputation_measurements(
        &query_runner,
        &mut map,
        keystore[1].node_secret_key.to_pk(),
        reputation::generate_reputation_measurements(&mut rng, 0.1),
    );

    // Submit the reputation measurements
    let req = get_update_request_node(
        UpdateMethod::SubmitReputationMeasurements {
            measurements: map.clone(),
        },
        &keystore[0].node_secret_key,
        1,
    );

    expect_tx_success!(req, &update_socket, ExecutionData::None);

    // Attempt to submit reputation measurements twice per epoch.
    // This transaction should revert because each node only can submit its reputation measurements
    // once per epoch.
    let req = get_update_request_node(
        UpdateMethod::SubmitReputationMeasurements { measurements: map },
        &keystore[0].node_secret_key,
        2,
    );

    expect_tx_revert!(
        req,
        &update_socket,
        ExecutionError::AlreadySubmittedMeasurements
    );
}

#[tokio::test]
async fn test_rep_scores() {
    let committee_size = 4;
    let (committee, keystore) = create_genesis_committee(committee_size);
    let (update_socket, query_runner) = test_init_app(committee);
    let required_signals = calculate_required_signals(committee_size);

    let mut rng = random::get_seedable_rng();

    let peer1 = keystore[2].node_secret_key.to_pk();
    let peer2 = keystore[3].node_secret_key.to_pk();
    let nonce = 1;

    let mut map = BTreeMap::new();
    let _ = update_reputation_measurements(
        &query_runner,
        &mut map,
        peer1,
        reputation::generate_reputation_measurements(&mut rng, 0.1),
    );
    let _ = update_reputation_measurements(
        &query_runner,
        &mut map,
        peer2,
        reputation::generate_reputation_measurements(&mut rng, 0.1),
    );
    submit_reputation_measurements!(&update_socket, &keystore[0].node_secret_key, nonce, map);

    let mut map = BTreeMap::new();
    let (peer_idx_1, _) = update_reputation_measurements(
        &query_runner,
        &mut map,
        peer1,
        reputation::generate_reputation_measurements(&mut rng, 0.1),
    );
    let (peer_idx_2, _) = update_reputation_measurements(
        &query_runner,
        &mut map,
        peer2,
        reputation::generate_reputation_measurements(&mut rng, 0.1),
    );
    submit_reputation_measurements!(&update_socket, &keystore[1].node_secret_key, nonce, map);

    let epoch = 0;
    // Change epoch so that rep scores will be calculated from the measurements.
    for (i, node) in keystore.iter().enumerate().take(required_signals) {
        // Not the prettiest solution but we have to keep track of the nonces somehow.
        let nonce = if i < 2 { 2 } else { 1 };
        change_epoch!(&update_socket, &node.node_secret_key, nonce, epoch);
    }

    assert!(query_runner.get_reputation(&peer_idx_1).is_some());
    assert!(query_runner.get_reputation(&peer_idx_2).is_some());
}

#[tokio::test]
async fn test_uptime_participation() {
    let committee_size = 4;
    let (mut committee, keystore) = create_genesis_committee(committee_size);
    committee[0].reputation = Some(40);
    committee[1].reputation = Some(80);
    let (update_socket, query_runner) = test_init_app(committee);

    let required_signals = calculate_required_signals(committee_size);

    let peer_1 = keystore[2].node_secret_key.to_pk();
    let peer_2 = keystore[3].node_secret_key.to_pk();
    let nonce = 1;

    let mut map = BTreeMap::new();
    let _ = update_reputation_measurements(
        &query_runner,
        &mut map,
        peer_1,
        test_reputation_measurements(5),
    );
    let _ = update_reputation_measurements(
        &query_runner,
        &mut map,
        peer_2,
        test_reputation_measurements(20),
    );

    submit_reputation_measurements!(&update_socket, &keystore[0].node_secret_key, nonce, map);

    let mut map = BTreeMap::new();
    let _ = update_reputation_measurements(
        &query_runner,
        &mut map,
        peer_1,
        test_reputation_measurements(9),
    );

    let _ = update_reputation_measurements(
        &query_runner,
        &mut map,
        peer_2,
        test_reputation_measurements(25),
    );
    submit_reputation_measurements!(&update_socket, &keystore[1].node_secret_key, nonce, map);

    let epoch = 0;
    // Change epoch so that rep scores will be calculated from the measurements.
    for (i, node) in keystore.iter().enumerate().take(required_signals) {
        // Not the prettiest solution but we have to keep track of the nonces somehow.
        let nonce = if i < 2 { 2 } else { 1 };
        change_epoch!(&update_socket, &node.node_secret_key, nonce, epoch);
    }

    let node_info1 = query_runner.get_node_info(&peer_1).unwrap();
    let node_info2 = query_runner.get_node_info(&peer_2).unwrap();

    assert_eq!(node_info1.participation, Participation::False);
    assert_eq!(node_info2.participation, Participation::True);
}

#[tokio::test]
async fn test_stake() {
    let committee_size = 4;
    let (committee, _keystore) = create_genesis_committee(committee_size);
    let (update_socket, query_runner) = test_init_app(committee);

    let owner_secret_key = AccountOwnerSecretKey::generate();
    let peer_pub_key = NodeSecretKey::generate().to_pk();

    // Deposit some FLK into account 1
    let deposit = 1000_u64.into();
    let update1 = prepare_deposit_update(&deposit, &owner_secret_key, 1);
    let update2 = prepare_deposit_update(&deposit, &owner_secret_key, 2);

    // Put 2 of the transaction in the block just to also test block exucution a bit
    let _ = run_transactions!(vec![update1, update2], &update_socket);

    // check that he has 2_000 flk balance
    assert_eq!(
        query_runner.get_flk_balance(&owner_secret_key.to_pk().into()),
        (HpUfixed::<18>::from(2u16) * deposit)
    );

    // Test staking on a new node
    let stake_amount = 1000u64.into();
    // First check that trying to stake without providing all the node info reverts
    let update = prepare_regular_stake_update(&stake_amount, &peer_pub_key, &owner_secret_key, 3);
    expect_tx_revert!(
        update,
        &update_socket,
        ExecutionError::InsufficientNodeDetails
    );

    // Now try with the correct details for a new node
    let update = prepare_initial_stake_update(
        &stake_amount,
        &peer_pub_key,
        [0; 96].into(),
        "127.0.0.1".parse().unwrap(),
        [0; 32].into(),
        "127.0.0.1".parse().unwrap(),
        NodePorts::default(),
        &owner_secret_key,
        4,
    );

    expect_tx_success!(update, &update_socket, ExecutionData::None);

    // Query the new node and make sure he has the proper stake
    assert_eq!(query_runner.get_staked(&peer_pub_key), stake_amount);

    // Stake 1000 more but since it is not a new node we should be able to leave the optional
    // paramaters out without a revert
    let update = prepare_regular_stake_update(&stake_amount, &peer_pub_key, &owner_secret_key, 5);

    expect_tx_success!(update, &update_socket, ExecutionData::None);

    // Node should now have 2_000 stake
    assert_eq!(
        query_runner.get_staked(&peer_pub_key),
        (HpUfixed::<18>::from(2u16) * stake_amount.clone())
    );

    // Now test unstake and make sure it moves the tokens to locked status
    let update = prepare_unstake_update(&stake_amount, &peer_pub_key, &owner_secret_key, 6);
    run_transaction!(update, &update_socket);

    // Check that his locked is 1000 and his remaining stake is 1000
    assert_eq!(query_runner.get_staked(&peer_pub_key), stake_amount);
    assert_eq!(query_runner.get_locked(&peer_pub_key), stake_amount);

    // Since this test starts at epoch 0 locked_until will be == lock_time
    assert_eq!(
        query_runner.get_locked_time(&peer_pub_key),
        test_genesis().lock_time
    );

    // Try to withdraw the locked tokens and it should revert
    let update = prepare_withdraw_unstaked_update(&peer_pub_key, None, &owner_secret_key, 7);

    expect_tx_revert!(update, &update_socket, ExecutionError::TokensLocked);
}

#[tokio::test]
async fn test_stake_lock() {
    let (update_socket, query_runner) = init_app(None);

    let owner_secret_key = AccountOwnerSecretKey::generate();
    let node_pub_key = NodeSecretKey::generate().to_pk();
    let amount: HpUfixed<18> = 1_000u64.into();

    deposit!(&update_socket, &owner_secret_key, 1, &amount);
    assert_eq!(
        query_runner.get_flk_balance(&owner_secret_key.to_pk().into()),
        amount
    );

    stake!(
        &update_socket,
        &owner_secret_key,
        2,
        &amount,
        &node_pub_key,
        [0; 96].into()
    );
    assert_eq!(query_runner.get_staked(&node_pub_key), amount);

    let locked_for = 365;
    let stake_lock_req = prepare_stake_lock_update(&node_pub_key, locked_for, &owner_secret_key, 3);

    expect_tx_success!(stake_lock_req, &update_socket, ExecutionData::None);

    assert_eq!(
        query_runner.get_stake_locked_until(&node_pub_key),
        locked_for
    );

    let unstake_req: UpdateRequest =
        prepare_unstake_update(&amount, &node_pub_key, &owner_secret_key, 4);
    expect_tx_revert!(
        unstake_req,
        &update_socket,
        ExecutionError::LockedTokensUnstakeForbidden
    );
}

#[tokio::test]
async fn test_pod_without_proof() {
    let committee_size = 4;
    let (committee, keystore) = create_genesis_committee(committee_size);
    let (update_socket, query_runner) = test_init_app(committee);

    let bandwidth_commodity = 1000;
    let compute_commodity = 2000;
    let bandwidth_pod =
        prepare_pod_request(bandwidth_commodity, 0, &keystore[0].node_secret_key, 1);
    let compute_pod = prepare_pod_request(compute_commodity, 1, &keystore[0].node_secret_key, 2);

    // run the delivery ack transaction
    run_transactions!(vec![bandwidth_pod, compute_pod], &update_socket);

    assert_eq!(
        query_runner
            .get_node_served(&keystore[0].node_secret_key.to_pk())
            .served,
        vec![bandwidth_commodity, compute_commodity]
    );

    assert_eq!(
        query_runner.get_total_served(0),
        TotalServed {
            served: vec![bandwidth_commodity, compute_commodity],
            reward_pool: (0.1 * bandwidth_commodity as f64 + 0.2 * compute_commodity as f64).into()
        }
    );
}

#[tokio::test]
async fn test_revert_self_transfer() {
    let (update_socket, query_runner) = init_app(None);

    let owner_secret_key = AccountOwnerSecretKey::generate();
    let owner: EthAddress = owner_secret_key.to_pk().into();

    let balance = 1_000u64.into();

    deposit!(&update_socket, &owner_secret_key, 1, &balance);
    assert_eq!(query_runner.get_flk_balance(&owner), balance);

    // Check that trying to transfer funds to yourself reverts
    let update = prepare_transfer_request(&10_u64.into(), &owner, &owner_secret_key, 2);
    expect_tx_revert!(update, &update_socket, ExecutionError::CantSendToYourself);
}

#[tokio::test]
async fn test_is_valid_node() {
    let (update_socket, query_runner) = init_app(None);

    let owner_secret_key = AccountOwnerSecretKey::generate();
    let node_pub_key = NodeSecretKey::generate().to_pk();

    // Stake minimum required amount.
    let minimum_stake_amount = query_runner.get_staking_amount().into();
    deposit!(&update_socket, &owner_secret_key, 1, &minimum_stake_amount);
    stake!(
        &update_socket,
        &owner_secret_key,
        2,
        &minimum_stake_amount,
        &node_pub_key,
        [0; 96].into()
    );

    // Make sure that this node is a valid node.
    assert!(query_runner.is_valid_node(&node_pub_key));

    // Generate new keys for a different node.
    let owner_secret_key = AccountOwnerSecretKey::generate();
    let node_pub_key = NodeSecretKey::generate().to_pk();

    // Stake less than the minimum required amount.
    let less_than_minimum_skate_amount = minimum_stake_amount / HpUfixed::<18>::from(2u16);
    deposit!(
        &update_socket,
        &owner_secret_key,
        1,
        &less_than_minimum_skate_amount
    );
    stake!(
        &update_socket,
        &owner_secret_key,
        2,
        &less_than_minimum_skate_amount,
        &node_pub_key,
        [1; 96].into()
    );
    // Make sure that this node is not a valid node.
    assert!(!query_runner.is_valid_node(&node_pub_key));
}

#[tokio::test]
async fn test_change_protocol_params() {
    let governance_secret_key = AccountOwnerSecretKey::generate();
    let governance_public_key = governance_secret_key.to_pk();

    let mut genesis = test_genesis();
    genesis.governance_address = governance_public_key.into();

    let (update_socket, query_runner) = init_app_with_genesis(&genesis);

    let param = ProtocolParams::LockTime;
    let new_value = 5;
    let update =
        prepare_change_protocol_param_request(&param, &new_value, &governance_secret_key, 1);
    run_transaction!(update, &update_socket);
    assert_eq!(query_runner.get_protocol_params(param.clone()), new_value);

    let new_value = 8;
    let update =
        prepare_change_protocol_param_request(&param, &new_value, &governance_secret_key, 2);
    run_transaction!(update, &update_socket);
    assert_eq!(query_runner.get_protocol_params(param.clone()), new_value);

    // Make sure that another private key cannot change protocol parameters.
    let some_secret_key = AccountOwnerSecretKey::generate();
    let minimum_stake_amount = query_runner.get_staking_amount().into();
    deposit!(&update_socket, &some_secret_key, 1, &minimum_stake_amount);

    let malicious_value = 1;
    let update =
        prepare_change_protocol_param_request(&param, &malicious_value, &some_secret_key, 2);
    expect_tx_revert!(update, &update_socket, ExecutionError::OnlyGovernance);
    // Lock time should still be 8.
    assert_eq!(query_runner.get_protocol_params(param), new_value)
}

#[tokio::test]
async fn test_validate_txn() {
    let committee_size = 4;
    let (committee, keystore) = create_genesis_committee(committee_size);
    let (update_socket, query_runner) = test_init_app(committee);

    // Submit a ChangeEpoch transaction that will revert (EpochHasNotStarted) and ensure that the
    // `validate_txn` method of the query runner returns the same response as the update runner.
    let invalid_epoch = 1;
    let req = prepare_change_epoch_request(invalid_epoch, &keystore[0].node_secret_key, 1);
    let res = run_transaction!(req, &update_socket);

    let req = prepare_change_epoch_request(invalid_epoch, &keystore[0].node_secret_key, 2);
    assert_eq!(
        res.txn_receipts[0].response,
        query_runner.validate_txn(req.into())
    );

    // Submit a ChangeEpoch transaction that will succeed and ensure that the
    // `validate_txn` method of the query runner returns the same response as the update runner.
    let epoch = 0;
    let req = prepare_change_epoch_request(epoch, &keystore[0].node_secret_key, 2);

    let res = run_transaction!(req, &update_socket);
    let req = prepare_change_epoch_request(epoch, &keystore[1].node_secret_key, 1);

    assert_eq!(
        res.txn_receipts[0].response,
        query_runner.validate_txn(req.into())
    );
}

#[tokio::test]
async fn test_distribute_rewards() {
    let committee_size = 4;
    let (committee, keystore) = create_genesis_committee(committee_size);

    let max_inflation = 10;
    let protocol_part = 10;
    let node_part = 80;
    let service_part = 10;
    let boost = 4;
    let supply_at_genesis = 1_000_000;
    let (update_socket, query_runner) = init_app_with_params(
        Params {
            epoch_time: None,
            max_inflation: Some(max_inflation),
            protocol_share: Some(protocol_part),
            node_share: Some(node_part),
            service_builder_share: Some(service_part),
            max_boost: Some(boost),
            supply_at_genesis: Some(supply_at_genesis),
        },
        Some(committee),
    );

    // get params for emission calculations
    let percentage_divisor: HpUfixed<18> = 100_u16.into();
    let supply_at_year_start: HpUfixed<18> = supply_at_genesis.into();
    let inflation: HpUfixed<18> = HpUfixed::from(max_inflation) / &percentage_divisor;
    let node_share = HpUfixed::from(node_part) / &percentage_divisor;
    let protocol_share = HpUfixed::from(protocol_part) / &percentage_divisor;
    let service_share = HpUfixed::from(service_part) / &percentage_divisor;

    let owner_secret_key1 = AccountOwnerSecretKey::generate();
    let node_secret_key1 = NodeSecretKey::generate();
    let owner_secret_key2 = AccountOwnerSecretKey::generate();
    let node_secret_key2 = NodeSecretKey::generate();

    let deposit_amount = 10_000_u64.into();
    let locked_for = 1460;
    // deposit FLK tokens and stake it
    deposit!(&update_socket, &owner_secret_key1, 1, &deposit_amount);
    stake!(
        &update_socket,
        &owner_secret_key1,
        2,
        &deposit_amount,
        &node_secret_key1.to_pk(),
        [0; 96].into()
    );
    deposit!(&update_socket, &owner_secret_key2, 1, &deposit_amount);
    stake!(
        &update_socket,
        &owner_secret_key2,
        2,
        &deposit_amount,
        &node_secret_key2.to_pk(),
        [1; 96].into()
    );
    stake_lock!(
        &update_socket,
        &owner_secret_key2,
        3,
        &node_secret_key2.to_pk(),
        locked_for
    );

    // submit pods for usage
    let commodity_10 = 12_800;
    let commodity_11 = 3_600;
    let commodity_21 = 5000;
    let pod_10 = prepare_pod_request(commodity_10, 0, &node_secret_key1, 1);
    let pod_11 = prepare_pod_request(commodity_11, 1, &node_secret_key1, 2);
    let pod_21 = prepare_pod_request(commodity_21, 1, &node_secret_key2, 1);

    let node_1_usd = 0.1 * (commodity_10 as f64) + 0.2 * (commodity_11 as f64); // 2_000 in revenue
    let node_2_usd = 0.2 * (commodity_21 as f64); // 1_000 in revenue
    let reward_pool: HpUfixed<6> = (node_1_usd + node_2_usd).into();

    let node_1_proportion: HpUfixed<18> = HpUfixed::from(2000_u64) / HpUfixed::from(3000_u64);
    let node_2_proportion: HpUfixed<18> = HpUfixed::from(1000_u64) / HpUfixed::from(3000_u64);

    let service_proportions: Vec<HpUfixed<18>> = vec![
        HpUfixed::from(1280_u64) / HpUfixed::from(3000_u64),
        HpUfixed::from(1720_u64) / HpUfixed::from(3000_u64),
    ];

    // run the delivery ack transaction
    run_transactions!(vec![pod_10, pod_11, pod_21], &update_socket);

    // call epoch change that will trigger distribute rewards
    simple_epoch_change!(&update_socket, &keystore, &query_runner, 0);

    // assert stable balances
    assert_eq!(
        query_runner.get_stables_balance(&owner_secret_key1.to_pk().into()),
        HpUfixed::<6>::from(node_1_usd) * node_share.convert_precision()
    );
    assert_eq!(
        query_runner.get_stables_balance(&owner_secret_key2.to_pk().into()),
        HpUfixed::<6>::from(node_2_usd) * node_share.convert_precision()
    );

    let total_share =
        &node_1_proportion * HpUfixed::from(1_u64) + &node_2_proportion * HpUfixed::from(4_u64);

    // calculate emissions per unit
    let emissions: HpUfixed<18> = (inflation * supply_at_year_start) / &365.0.into();
    let emissions_for_node = &emissions * &node_share;

    // assert flk balances node 1
    assert_eq!(
        // node_flk_balance1
        query_runner.get_flk_balance(&owner_secret_key1.to_pk().into()),
        // node_flk_rewards1
        (&emissions_for_node * &node_1_proportion) / &total_share
    );

    // assert flk balances node 2
    assert_eq!(
        // node_flk_balance2
        query_runner.get_flk_balance(&owner_secret_key2.to_pk().into()),
        // node_flk_rewards2
        (&emissions_for_node * (&node_2_proportion * HpUfixed::from(4_u64))) / &total_share
    );

    // assert protocols share
    let protocol_account = query_runner.get_protocol_fund_address();
    let protocol_balance = query_runner.get_flk_balance(&protocol_account);
    let protocol_rewards = &emissions * &protocol_share;
    assert_eq!(protocol_balance, protocol_rewards);

    let protocol_stables_balance = query_runner.get_stables_balance(&protocol_account);
    assert_eq!(
        &reward_pool * &protocol_share.convert_precision(),
        protocol_stables_balance
    );

    // assert service balances with service id 0 and 1
    for s in 0..2 {
        let service_owner = query_runner.get_service_info(s).owner;
        let service_balance = query_runner.get_flk_balance(&service_owner);
        assert_eq!(
            service_balance,
            &emissions * &service_share * &service_proportions[s as usize]
        );
        let service_stables_balance = query_runner.get_stables_balance(&service_owner);
        assert_eq!(
            service_stables_balance,
            &reward_pool
                * &service_share.convert_precision()
                * &service_proportions[s as usize].convert_precision()
        );
    }
}

#[tokio::test]
async fn test_get_node_registry() {
    let committee_size = 4;
    let (committee, keystore) = create_genesis_committee(committee_size);
    let (update_socket, query_runner) = test_init_app(committee);

    let owner_secret_key1 = AccountOwnerSecretKey::generate();
    let node_secret_key1 = NodeSecretKey::generate();

    // Stake minimum required amount.
    let minimum_stake_amount = query_runner.get_staking_amount().into();
    deposit!(&update_socket, &owner_secret_key1, 1, &minimum_stake_amount);
    stake!(
        &update_socket,
        &owner_secret_key1,
        2,
        &minimum_stake_amount,
        &node_secret_key1.to_pk(),
        [0; 96].into()
    );

    // Generate new keys for a different node.
    let owner_secret_key2 = AccountOwnerSecretKey::generate();
    let node_secret_key2 = NodeSecretKey::generate();

    // Stake less than the minimum required amount.
    let less_than_minimum_skate_amount = minimum_stake_amount.clone() / HpUfixed::<18>::from(2u16);
    deposit!(
        &update_socket,
        &owner_secret_key2,
        1,
        &less_than_minimum_skate_amount
    );
    stake!(
        &update_socket,
        &owner_secret_key2,
        2,
        &less_than_minimum_skate_amount,
        &node_secret_key2.to_pk(),
        [1; 96].into()
    );

    // Generate new keys for a different node.
    let owner_secret_key3 = AccountOwnerSecretKey::generate();
    let node_secret_key3 = NodeSecretKey::generate();

    // Stake minimum required amount.
    deposit!(&update_socket, &owner_secret_key3, 1, &minimum_stake_amount);
    stake!(
        &update_socket,
        &owner_secret_key3,
        2,
        &minimum_stake_amount,
        &node_secret_key3.to_pk(),
        [3; 96].into()
    );

    let valid_nodes = query_runner.get_node_registry(None);
    // We added two valid nodes, so the node registry should contain 2 nodes plus the committee.
    assert_eq!(valid_nodes.len(), 2 + keystore.len());
    assert_valid_node!(&valid_nodes, &query_runner, &node_secret_key1.to_pk());
    // Node registry doesn't contain the invalid node
    assert_not_valid_node!(&valid_nodes, &query_runner, &node_secret_key2.to_pk());
    assert_valid_node!(&valid_nodes, &query_runner, &node_secret_key3.to_pk());

    // We added 3 nodes, so the node registry should contain 3 nodes plus the committee.
    assert_paging_node_registry!(
        &query_runner,
        paging_params(true, 0, keystore.len() + 3),
        3 + keystore.len()
    );
    // We added 2 valid nodes, so the node registry should contain 2 nodes plus the committee.
    assert_paging_node_registry!(
        &query_runner,
        paging_params(false, 0, keystore.len() + 3),
        2 + keystore.len()
    );

    // We get the first 4 nodes.
    assert_paging_node_registry!(
        &query_runner,
        paging_params(true, 0, keystore.len()),
        keystore.len()
    );

    // The first 4 nodes are the committee and we added 3 nodes.
    assert_paging_node_registry!(&query_runner, paging_params(true, 4, keystore.len()), 3);

    // The first 4 nodes are the committee and we added 2 valid nodes.
    assert_paging_node_registry!(
        &query_runner,
        paging_params(false, keystore.len() as u32, keystore.len()),
        2
    );

    // The first 4 nodes are the committee and we added 3 nodes.
    assert_paging_node_registry!(
        &query_runner,
        paging_params(false, keystore.len() as u32, 1),
        1
    );
}
