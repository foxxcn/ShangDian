mod config;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use affair::{Socket, Task};
use async_trait::async_trait;
use config::Config;
use draco_application::query_runner::QueryRunner;
use draco_interfaces::{
    common::WithStartAndShutdown,
    config::ConfigConsumer,
    signer::{SignerInterface, SubmitTxSocket},
    types::UpdateMethod,
    MempoolSocket,
};
use fleek_crypto::{
    NodeNetworkingPublicKey, NodeNetworkingSecretKey, NodePublicKey, NodeSecretKey, NodeSignature,
    SecretKey,
};
use tokio::{sync::mpsc, time::interval};

// The signer has to stay in sync with the application.
// If the application has a different nonce then expected, the signer has to react.
// `QUERY_INTERVAL` specifies the interval for querying the application.
const QUERY_INTERVAL: Duration = Duration::from_secs(5);

// If a transaction does not get ordered, the signer will try to resend it.
// `TIMEOUT` specifies the duration the signer will wait before resending transactions to the
// mempool.
const _TIMEOUT: Duration = Duration::from_secs(20);

#[allow(clippy::type_complexity)]
pub struct Signer {
    inner: Arc<SignerInner>,
    shutdown_tx: Arc<Mutex<Option<mpsc::Sender<()>>>>,
    socket: Socket<UpdateMethod, u64>,
    is_running: Arc<Mutex<bool>>,
    // `rx` is only parked here for the time from the call to `ìnit` to the call to `start`,
    // when it is moved into the SignerInner. The only reason it is behind a Arc<Mutex<>> is to
    // ensure that `Signer` is Send and Sync.
    rx: Arc<Mutex<Option<mpsc::Receiver<Task<UpdateMethod, u64>>>>>,
    // `mempool_socket` is only parked here for the time from the call to `provide_mempool` to the
    // call to `start`, when it is moved into SignerInner.
    mempool_socket: Arc<Mutex<Option<MempoolSocket>>>,
    // `mempool_socket` is only parked here for the time from the call to `provide_query_runner` to
    // the call to `start`, when it is moved into SignerInner.
    query_runner: Arc<Mutex<Option<QueryRunner>>>,
}

#[async_trait]
impl WithStartAndShutdown for Signer {
    /// Returns true if this system is running or not.
    fn is_running(&self) -> bool {
        *self.is_running.lock().unwrap()
    }

    /// Start the system, should not do anything if the system is already
    /// started.
    async fn start(&self) {
        if !*self.is_running.lock().unwrap() {
            let (shutdown_tx, shutdown_rx) = mpsc::channel(1);
            let inner = self.inner.clone();
            let rx = self.rx.lock().unwrap().take().unwrap();
            let mempool_socket = self.get_mempool_socket();
            let query_runner = self.get_query_runner();
            tokio::spawn(
                async move { inner.handle(rx, shutdown_rx, mempool_socket, query_runner) },
            );
            *self.shutdown_tx.lock().unwrap() = Some(shutdown_tx);
            *self.is_running.lock().unwrap() = true;
        }
    }

    /// Send the shutdown signal to the system.
    async fn shutdown(&self) {
        let shutdown_tx = self.get_shutdown_tx();
        if let Some(shutdown_tx) = shutdown_tx {
            shutdown_tx.send(()).await.unwrap();
        }
    }
}

#[async_trait]
impl SignerInterface for Signer {
    type SyncQuery = QueryRunner;

    /// Initialize the signature service.
    async fn init(config: Config) -> anyhow::Result<Self> {
        let inner = SignerInner::new(config);
        let (socket, rx) = Socket::raw_bounded(2048);
        Ok(Self {
            inner: Arc::new(inner),
            shutdown_tx: Arc::new(Mutex::new(None)),
            socket,
            is_running: Arc::new(Mutex::new(false)),
            rx: Arc::new(Mutex::new(Some(rx))),
            mempool_socket: Arc::new(Mutex::new(None)),
            query_runner: Arc::new(Mutex::new(None)),
        })
    }

    /// Provide the signer service with the mempool socket after initialization, this function
    /// should only be called once.
    fn provide_mempool(&mut self, mempool: MempoolSocket) {
        // TODO(matthias): I think the receiver can be &self here.
        *self.mempool_socket.lock().unwrap() = Some(mempool);
    }

    /// Provide the signer service with the query runner after initialization, this function
    /// should only be called once.
    fn provide_query_runner(&self, query_runner: Self::SyncQuery) {
        *self.query_runner.lock().unwrap() = Some(query_runner);
    }

    /// Returns the `BLS` public key of the current node.
    fn get_bls_pk(&self) -> NodePublicKey {
        self.inner.node_public_key
    }

    /// Returns the `Ed25519` (network) public key of the current node.
    fn get_ed25519_pk(&self) -> NodeNetworkingPublicKey {
        self.inner.network_public_key
    }

    /// Returns the loaded secret key material.
    ///
    /// # Safety
    ///
    /// Just like any other function which deals with secret material this function should
    /// be used with the greatest caution.
    fn get_sk(&self) -> (NodeNetworkingSecretKey, NodeSecretKey) {
        (self.inner.network_secret_key, self.inner.node_secret_key)
    }

    /// Returns a socket that can be used to submit transactions to the mempool, these
    /// transactions are signed by the node and a proper nonce is assigned by the
    /// implementation.
    ///
    /// # Panics
    ///
    /// This function can panic if there has not been a prior call to `provide_mempool`.
    fn get_socket(&self) -> SubmitTxSocket {
        self.socket.clone()
    }

    /// Sign the provided raw digest and return a signature.
    ///
    /// # Safety
    ///
    /// This function is unsafe to use without proper reasoning, which is trivial since
    /// this function is responsible for signing arbitrary messages from other parts of
    /// the system.
    fn sign_raw_digest(&self, _digest: &[u8; 32]) -> NodeSignature {
        todo!()
    }
}

impl Signer {
    fn get_shutdown_tx(&self) -> Option<mpsc::Sender<()>> {
        self.shutdown_tx.lock().unwrap().take()
    }

    fn get_mempool_socket(&self) -> MempoolSocket {
        self.mempool_socket
            .lock()
            .unwrap()
            .take()
            .expect("Mempool socket must be provided before starting the signer service.")
    }

    fn get_query_runner(&self) -> QueryRunner {
        self.query_runner
            .lock()
            .unwrap()
            .take()
            .expect("Query runner must be provided before starting the signer serivce.")
    }
}

struct SignerInner {
    node_secret_key: NodeSecretKey,
    node_public_key: NodePublicKey,
    network_secret_key: NodeNetworkingSecretKey,
    network_public_key: NodeNetworkingPublicKey,
}

impl SignerInner {
    fn new(_config: Config) -> Self {
        // TODO: load private keys from file if they exist
        let node_secret_key = NodeSecretKey::generate();
        let node_public_key = node_secret_key.to_pk();
        let network_secret_key = NodeNetworkingSecretKey::generate();
        let network_public_key = network_secret_key.to_pk();
        Self {
            node_secret_key,
            node_public_key,
            network_secret_key,
            network_public_key,
        }
    }

    async fn handle(
        self: Arc<Self>,
        mut rx: mpsc::Receiver<Task<UpdateMethod, u64>>,
        mut shutdown_rx: mpsc::Receiver<()>,
        _mempool_socket: MempoolSocket,
        _query_runner: QueryRunner,
    ) {
        let mut query_interval = interval(QUERY_INTERVAL);
        loop {
            tokio::select! {
                _update_method = rx.recv() => {
                    // TODO: send to mempool
                }
                _ = query_interval.tick() => {

                }
                _ = shutdown_rx.recv() => break,
            }
        }
    }
}

impl ConfigConsumer for Signer {
    const KEY: &'static str = "signer";

    type Config = Config;
}
