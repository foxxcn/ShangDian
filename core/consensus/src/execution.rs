use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use lightning_interfaces::types::{Block, UpdateRequest};
use lightning_interfaces::{ExecutionEngineSocket, PubSub};
use log::info;
use narwhal_executor::ExecutionState;
use narwhal_types::{Batch, BatchAPI, ConsensusOutput};
use tokio::sync::Notify;

use crate::consensus::PubSubMsg;

pub struct Execution<P: PubSub<PubSubMsg>> {
    /// Managing certificates generated by narwhal.
    executor: ExecutionEngineSocket,
    reconfigure_notify: Arc<Notify>,
    new_block_notify: Arc<Notify>,
    pub_sub: P,
    is_committee: AtomicBool,
}

impl<P: PubSub<PubSubMsg>> Execution<P> {
    pub fn new(
        executor: ExecutionEngineSocket,
        reconfigure_notify: Arc<Notify>,
        new_block_notify: Arc<Notify>,
        pub_sub: P,
    ) -> Self {
        Self {
            executor,
            reconfigure_notify,
            new_block_notify,
            pub_sub,
            is_committee: AtomicBool::new(false),
        }
    }

    async fn submit_batch(&self, batch: Vec<Batch>) {
        let mut change_epoch = false;
        for batch in batch {
            let block = Block {
                transactions: batch
                    .transactions()
                    .iter()
                    .filter_map(|txn| bincode::deserialize::<UpdateRequest>(txn).ok())
                    .collect(),
            };
            info!("Consensus submitted new block to application");
            // Unfailable
            let results = self.executor.run(block).await.unwrap();
            if results.change_epoch {
                change_epoch = true;
            }
        }
        self.new_block_notify.notify_waiters();
        if change_epoch {
            self.reconfigure_notify.notify_waiters();
        }
    }

    pub fn set_committee_status(&self, on_committee: bool) {
        self.is_committee.store(on_committee, Ordering::Relaxed)
    }
}

#[async_trait]
impl<P: PubSub<PubSubMsg>> ExecutionState for Execution<P> {
    async fn handle_consensus_output(&self, consensus_output: ConsensusOutput) {
        for (certificate, batches) in consensus_output.batches {
            // If node is on committee they should broadcast this certificate and batches through
            // gossip
            if self.is_committee.load(Ordering::Relaxed) {
                self.pub_sub.send(&certificate.into()).await;

                for batch in &batches {
                    // todo(dalton): Find a way to not clone batches here
                    self.pub_sub.send(&batch.clone().into()).await;
                }
            }

            self.submit_batch(batches).await
        }
    }

    async fn last_executed_sub_dag_index(&self) -> u64 {
        0
    }
}
