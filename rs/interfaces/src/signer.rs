use affair::Socket;
use async_trait::async_trait;

use crate::{
    config::ConfigConsumer,
    consensus::MempoolPort,
    identity::{BlsPublicKey, Ed25519PublicKey, Signature},
    types::UpdateMethod,
};

/// A port that is responsible to submit a transaction to the consensus from our node,
/// implementation of this port needs to assure the consistency and increment of the
/// nonce (which we also refer to as the counter).
pub type SubmitTxPort = Socket<UpdateMethod, u64>;

/// The signature provider is responsible for signing messages using the private key of
/// the node.
#[async_trait]
pub trait SignerInterface: ConfigConsumer + Sized {
    /// Internal type that is used for the Ed25519 secret key.
    type Ed25519SecretKey;

    /// Internal type that is used for the BLS secret key.
    type BlsSecretKey;

    /// Initialize the signature service.
    async fn init(config: Self::Config) -> anyhow::Result<Self>;

    /// Provide the signer service with the mempool port after initialization, this function
    /// should only be called once.
    fn provide_mempool(&mut self, mempool: MempoolPort);

    /// Returns the `BLS` public key of the current node.
    fn get_bls_pk(&self) -> BlsPublicKey;

    /// Returns the `Ed25519` (network) public key of the current node.
    fn get_ed25519_pk(&self) -> Ed25519PublicKey;

    /// Returns the loaded secret key material.
    ///
    /// # Safety
    ///
    /// Just like any other function which deals with secret material this function should
    /// be used with the greatest caution.
    fn get_sk(&self) -> (Self::Ed25519SecretKey, Self::BlsSecretKey);

    /// Returns a port that can be used to submit transactions to the mempool, these
    /// transactions are signed by the node and a proper nonce is assigned by the
    /// implementation.
    ///
    /// # Panics
    ///
    /// This function can panic if there has not been a prior call to `provide_mempool`.
    fn get_port(&self) -> SubmitTxPort;

    /// Sign the provided raw digest and return a signature.
    ///
    /// # Safety
    ///
    /// This function is unsafe to use without proper reasoning, which is trivial since
    /// this function is responsible for signing arbitrary messages from other parts of
    /// the system.
    fn sign_raw_digest(&self, digest: &[u8; 32]) -> Signature;
}
