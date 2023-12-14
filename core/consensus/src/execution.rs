use std::sync::Arc;

use async_trait::async_trait;
use fastcrypto::hash::HashFunction;
use fleek_blake3 as blake3;
use lightning_interfaces::types::{Block, Epoch, IndexRequest, NodeIndex, TransactionRequest};
use lightning_interfaces::{
    ExecutionEngineSocket,
    IndexSocket,
    SyncQueryRunnerInterface,
    ToDigest,
    TranscriptBuilder,
};
use narwhal_crypto::DefaultHashFunction;
use narwhal_executor::ExecutionState;
use narwhal_types::{BatchAPI, BatchDigest, ConsensusOutput, Transaction};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Notify};
use tracing::{error, info};

pub type Digest = [u8; 32];

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AuthenticStampedParcel {
    pub transactions: Vec<Transaction>,
    pub last_executed: Digest,
    pub epoch: Epoch,
}

impl ToDigest for AuthenticStampedParcel {
    fn transcript(&self) -> TranscriptBuilder {
        panic!("We don't need this here");
    }

    fn to_digest(&self) -> Digest {
        let batch_digest =
            BatchDigest::new(DefaultHashFunction::digest_iterator(self.transactions.iter()).into());

        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(self.transactions.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&batch_digest.0);
        bytes.extend_from_slice(&self.last_executed);

        blake3::hash(&bytes).into()
    }
}

/// A message an authority sends out attest that an Authentic stamp parcel is accurate. When an edge
/// node gets 2f+1 of these it commmits the transactions in the parcel
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CommitteeAttestation {
    /// The digest we are attesting is correct
    pub digest: Digest,
    /// We send random bytes with this messsage so it gives it a unique hash and differentiates it
    /// from the other committee members attestation broadcasts
    pub node_index: NodeIndex,
    pub epoch: Epoch,
}

pub struct Execution<Q: SyncQueryRunnerInterface> {
    /// Managing certificates generated by narwhal.
    executor: ExecutionEngineSocket,
    /// Used to signal internal consensus proccesses that it is time to reconfigure for a new epoch
    reconfigure_notify: Arc<Notify>,
    /// Notifier that notifies everytime a block is executed on application state
    new_block_notify: Arc<Notify>,
    /// Used to send payloads to the edge node consensus to broadcast out to other nodes
    tx_narwhal_batches: mpsc::Sender<(AuthenticStampedParcel, bool)>,
    /// Query runner to check application state, mainly used to make sure the last executed block
    /// is up to date from time we were an edge node
    query_runner: Q,
    /// If this socket is present it means the node is in archive node and should send all blocks
    /// and transactions it executes to the archiver to be indexed
    index_socket: Option<IndexSocket>,
}

impl<Q: SyncQueryRunnerInterface> Execution<Q> {
    pub fn new(
        executor: ExecutionEngineSocket,
        reconfigure_notify: Arc<Notify>,
        new_block_notify: Arc<Notify>,
        tx_narwhal_batches: mpsc::Sender<(AuthenticStampedParcel, bool)>,
        query_runner: Q,
        index_socket: Option<IndexSocket>,
    ) -> Self {
        Self {
            executor,
            reconfigure_notify,
            new_block_notify,
            tx_narwhal_batches,
            query_runner,
            index_socket,
        }
    }

    // Returns true if the epoch changed
    pub(crate) async fn submit_batch(&self, payload: Vec<Transaction>, digest: Digest) -> bool {
        let mut change_epoch = false;

        let transactions = payload
            .into_iter()
            .filter_map(|txn| TransactionRequest::try_from(txn.as_ref()).ok())
            .collect::<Vec<_>>();

        if transactions.is_empty() {
            return false;
        }

        let block = Block {
            transactions,
            digest,
        };

        let archive_block = if self.index_socket.is_some() {
            Some(block.clone())
        } else {
            None
        };

        // Unfailable
        let results = self.executor.run(block).await.unwrap();
        info!("Consensus submitted new block to application");

        if results.change_epoch {
            change_epoch = true;
        }

        // If we have the archive socket that means our node is in archive node and we should send
        // the block and the reciept to be indexed
        if let (Some(block), Some(socket)) = (archive_block, &self.index_socket) {
            if let Err(e) = socket
                .run(IndexRequest {
                    block,
                    receipt: results,
                })
                .await
            {
                error!("We could not send a message to the archiver: {e}");
            }
        }

        self.new_block_notify.notify_waiters();

        change_epoch
    }
}

#[async_trait]
impl<Q: SyncQueryRunnerInterface> ExecutionState for Execution<Q> {
    async fn handle_consensus_output(&self, consensus_output: ConsensusOutput) {
        for (cert, batches) in consensus_output.batches {
            let current_epoch = self.query_runner.get_current_epoch();
            if cert.epoch() != current_epoch {
                // If the certificate epoch does not match the current epoch in the application
                // state do not execute this transaction, This could only happen in
                // certain race conditions at the end of an epoch and we need this to ensure all
                // nodes execute the same transactions
                continue;
            }

            if !batches.is_empty() {
                let mut batch_payload =
                    Vec::with_capacity(batches.iter().fold(0, |acc, batch| acc + batch.size()));

                for batch in batches {
                    for tx_bytes in batch.transactions() {
                        if let Ok(tx) = TransactionRequest::try_from(tx_bytes.as_ref()) {
                            if !self.query_runner.has_executed_digest(tx.hash()) {
                                batch_payload.push(tx_bytes.to_owned());
                            }
                        }
                    }
                }

                if batch_payload.is_empty() {
                    continue;
                }

                // We have batches in the payload send them over broadcast along with an attestion
                // of them
                let last_executed = self.query_runner.get_last_block();
                let parcel = AuthenticStampedParcel {
                    transactions: batch_payload.clone(),
                    last_executed,
                    epoch: current_epoch,
                };

                let epoch_changed = self.submit_batch(batch_payload, parcel.to_digest()).await;

                if let Err(e) = self.tx_narwhal_batches.send((parcel, epoch_changed)).await {
                    // This shouldnt ever happen. But if it does there is no critical tasks
                    // happening on the other end of this that would require a
                    // panic
                    error!("Narwhal failed to send batch payload to edge consensus: {e:?}");
                }

                // Submit the batches to application layer and if the epoch changed reset last
                // executed
                if epoch_changed {
                    self.reconfigure_notify.notify_waiters();
                }
            }
        }
    }

    async fn last_executed_sub_dag_index(&self) -> u64 {
        0
    }
}
