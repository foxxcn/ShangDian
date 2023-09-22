use affair::Socket;
use async_trait::async_trait;
use lightning_types::{FetcherRequest, FetcherResponse};

use crate::infu_collection::Collection;
use crate::{
    BlockStoreInterface,
    ConfigConsumer,
    ConfigProviderInterface,
    OriginProviderInterface,
    PoolInterface,
    ResolverInterface,
    WithStartAndShutdown,
};

pub type FetcherSocket = Socket<FetcherRequest, FetcherResponse>;

#[async_trait]
#[infusion::service]
pub trait FetcherInterface<C: Collection>:
    WithStartAndShutdown + ConfigConsumer + Sized + Send + Sync
{
    fn _init(
        config: ::ConfigProviderInterface,
        blockstore: ::BlockStoreInterface,
        resolver: ::ResolverInterface,
        origin: ::OriginProviderInterface,
        pool: ::PoolInterface,
    ) {
        Self::init(
            config.get::<Self>(),
            blockstore.clone(),
            resolver.clone(),
            origin,
            pool,
        )
    }

    /// Initialize the fetcher.
    fn init(
        config: Self::Config,
        blockstore: C::BlockStoreInterface,
        resolver: C::ResolverInterface,
        origin: &C::OriginProviderInterface,
        pool: &C::PoolInterface,
    ) -> anyhow::Result<Self>;

    /// Returns a socket that can be used to submit requests to the fetcher.
    fn get_socket(&self) -> FetcherSocket;
}
