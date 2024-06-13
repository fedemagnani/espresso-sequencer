use std::{num::NonZeroUsize, time::Duration};

use super::{
    fs,
    options::{Options, Query},
    sql, AccountQueryData, BlocksFrontier,
};
use crate::{
    network,
    persistence::{self, SequencerPersistence},
    ChainConfig, PubKey, SeqTypes, Transaction,
};
use anyhow::bail;
use async_trait::async_trait;
use committable::Commitment;
use ethers::prelude::Address;
use futures::future::Future;
use hotshot_query_service::{
    availability::AvailabilityDataSource,
    data_source::{MetricsDataSource, UpdateDataSource, VersionedDataSource},
    fetching::provider::{AnyProvider, QueryServiceProvider},
    node::NodeDataSource,
    status::StatusDataSource,
};
use hotshot_types::{
    data::ViewNumber, light_client::StateSignatureRequestBody, ExecutionType, HotShotConfig,
    PeerConfig, ValidatorConfig,
};

use serde::Serialize;
use tide_disco::Url;
use vbs::version::StaticVersionType;
use vec1::Vec1;

pub trait DataSourceOptions: persistence::PersistenceOptions {
    type DataSource: SequencerDataSource<Options = Self>;

    fn enable_query_module(&self, opt: Options, query: Query) -> Options;
}

impl DataSourceOptions for persistence::sql::Options {
    type DataSource = sql::DataSource;

    fn enable_query_module(&self, opt: Options, query: Query) -> Options {
        opt.query_sql(query, self.clone())
    }
}

impl DataSourceOptions for persistence::fs::Options {
    type DataSource = fs::DataSource;

    fn enable_query_module(&self, opt: Options, query: Query) -> Options {
        opt.query_fs(query, self.clone())
    }
}

/// A data source with sequencer-specific functionality.
///
/// This trait extends the generic [`AvailabilityDataSource`] with some additional data needed to
/// provided sequencer-specific endpoints.
#[async_trait]
pub trait SequencerDataSource:
    AvailabilityDataSource<SeqTypes>
    + NodeDataSource<SeqTypes>
    + StatusDataSource
    + UpdateDataSource<SeqTypes>
    + VersionedDataSource
    + Sized
{
    type Options: DataSourceOptions<DataSource = Self>;

    /// Instantiate a data source from command line options.
    async fn create(opt: Self::Options, provider: Provider, reset: bool) -> anyhow::Result<Self>;
}

/// Provider for fetching missing data for the query service.
pub type Provider = AnyProvider<SeqTypes>;

/// Create a provider for fetching missing data from a list of peer query services.
pub fn provider<Ver: StaticVersionType + 'static>(
    peers: impl IntoIterator<Item = Url>,
    bind_version: Ver,
) -> Provider {
    let mut provider = Provider::default();
    for peer in peers {
        tracing::info!("will fetch missing data from {peer}");
        provider = provider.with_provider(QueryServiceProvider::new(peer, bind_version));
    }
    provider
}

pub(crate) trait SubmitDataSource<N: network::Type, P: SequencerPersistence> {
    fn submit(&self, tx: Transaction) -> impl Send + Future<Output = anyhow::Result<()>>;
}

pub(crate) trait HotShotConfigDataSource {
    fn get_config(&self) -> impl Send + Future<Output = PublicHotShotConfig>;
}

#[async_trait]
pub(crate) trait StateSignatureDataSource<N: network::Type> {
    async fn get_state_signature(&self, height: u64) -> Option<StateSignatureRequestBody>;
}

pub(crate) trait CatchupDataSource {
    /// Get the state of the requested `account`.
    ///
    /// The state is fetched from a snapshot at the given height and view, which _must_ correspond!
    /// `height` is provided to simplify lookups for backends where data is not indexed by view.
    /// This function is intended to be used for catchup, so `view` should be no older than the last
    /// decided view.
    fn get_account(
        &self,
        _height: u64,
        _view: ViewNumber,
        _account: Address,
    ) -> impl Send + Future<Output = anyhow::Result<AccountQueryData>> {
        // Merklized state catchup is only supported by persistence backends that provide merklized
        // state storage. This default implementation is overridden for those that do. Otherwise,
        // catchup can still be provided by fetching undecided merklized state from consensus
        // memory.
        async {
            bail!("merklized state catchup is not supported for this data source");
        }
    }

    /// Get the blocks Merkle tree frontier.
    ///
    /// The state is fetched from a snapshot at the given height and view, which _must_ correspond!
    /// `height` is provided to simplify lookups for backends where data is not indexed by view.
    /// This function is intended to be used for catchup, so `view` should be no older than the last
    /// decided view.
    fn get_frontier(
        &self,
        _height: u64,
        _view: ViewNumber,
    ) -> impl Send + Future<Output = anyhow::Result<BlocksFrontier>> {
        // Merklized state catchup is only supported by persistence backends that provide merklized
        // state storage. This default implementation is overridden for those that do. Otherwise,
        // catchup can still be provided by fetching undecided merklized state from consensus
        // memory.
        async {
            bail!("merklized state catchup is not supported for this data source");
        }
    }

    fn get_chain_config(
        &self,
        _commitment: Commitment<ChainConfig>,
    ) -> impl Send + Future<Output = anyhow::Result<ChainConfig>> {
        async {
            bail!("chain config catchup is not supported for this data source");
        }
    }
}

impl CatchupDataSource for MetricsDataSource {}

/// This struct defines the public Hotshot validator configuration.
/// Private key and state key pairs are excluded for security reasons.

#[derive(Debug, Serialize)]
pub struct PublicValidatorConfig {
    pub public_key: PubKey,
    pub stake_value: u64,
    pub is_da: bool,
    pub private_key: &'static str,
    pub state_public_key: String,
    pub state_key_pair: &'static str,
}

impl From<ValidatorConfig<PubKey>> for PublicValidatorConfig {
    fn from(v: ValidatorConfig<PubKey>) -> Self {
        let ValidatorConfig::<PubKey> {
            public_key,
            private_key: _,
            stake_value,
            state_key_pair,
            is_da,
        } = v;

        let state_public_key = state_key_pair.ver_key();

        Self {
            public_key,
            stake_value,
            is_da,
            state_public_key: state_public_key.to_string(),
            private_key: "*****",
            state_key_pair: "*****",
        }
    }
}

/// This struct defines the public Hotshot configuration parameters.
/// Our config module features a GET endpoint accessible via the route `/hotshot` to display the hotshot config parameters.
/// Hotshot config has sensitive information like private keys and such fields are excluded from this struct.
#[derive(Debug, Serialize)]
pub struct PublicHotShotConfig {
    pub execution_type: ExecutionType,
    pub start_threshold: (u64, u64),
    pub num_nodes_with_stake: NonZeroUsize,
    pub num_nodes_without_stake: usize,
    pub known_nodes_with_stake: Vec<PeerConfig<PubKey>>,
    pub known_da_nodes: Vec<PeerConfig<PubKey>>,
    pub known_nodes_without_stake: Vec<PubKey>,
    pub my_own_validator_config: PublicValidatorConfig,
    pub da_staked_committee_size: usize,
    pub da_non_staked_committee_size: usize,
    pub fixed_leader_for_gpuvid: usize,
    pub next_view_timeout: u64,
    pub view_sync_timeout: Duration,
    pub timeout_ratio: (u64, u64),
    pub round_start_delay: u64,
    pub start_delay: u64,
    pub num_bootstrap: usize,
    pub builder_timeout: Duration,
    pub data_request_delay: Duration,
    pub builder_urls: Vec1<Url>,
    pub start_proposing_view: u64,
    pub stop_proposing_view: u64,
    pub start_voting_view: u64,
    pub stop_voting_view: u64,
}

impl From<HotShotConfig<PubKey>> for PublicHotShotConfig {
    fn from(v: HotShotConfig<PubKey>) -> Self {
        // Destructure all fields from HotShotConfig to return an error
        // if new fields are added to HotShotConfig. This makes sure that we handle
        // all fields appropriately and do not miss any updates.
        let HotShotConfig::<PubKey> {
            execution_type,
            start_threshold,
            num_nodes_with_stake,
            num_nodes_without_stake,
            known_nodes_with_stake,
            known_da_nodes,
            known_nodes_without_stake,
            my_own_validator_config,
            da_staked_committee_size,
            da_non_staked_committee_size,
            fixed_leader_for_gpuvid,
            next_view_timeout,
            view_sync_timeout,
            timeout_ratio,
            round_start_delay,
            start_delay,
            num_bootstrap,
            builder_timeout,
            data_request_delay,
            builder_urls,
            start_proposing_view,
            stop_proposing_view,
            start_voting_view,
            stop_voting_view,
        } = v;

        Self {
            execution_type,
            start_threshold,
            num_nodes_with_stake,
            num_nodes_without_stake,
            known_nodes_with_stake,
            known_da_nodes,
            known_nodes_without_stake,
            my_own_validator_config: my_own_validator_config.into(),
            da_staked_committee_size,
            da_non_staked_committee_size,
            fixed_leader_for_gpuvid,
            next_view_timeout,
            view_sync_timeout,
            timeout_ratio,
            round_start_delay,
            start_delay,
            num_bootstrap,
            builder_timeout,
            data_request_delay,
            builder_urls,
            start_proposing_view,
            stop_proposing_view,
            start_voting_view,
            stop_voting_view,
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::super::Options;
    use super::*;

    #[async_trait]
    pub(crate) trait TestableSequencerDataSource: SequencerDataSource {
        type Storage: Sync;

        async fn create_storage() -> Self::Storage;
        fn persistence_options(storage: &Self::Storage) -> Self::Options;
        fn options(storage: &Self::Storage, opt: Options) -> Options;
    }
}
