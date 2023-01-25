#![allow(clippy::integer_arithmetic)]

pub mod nonblocking;
pub mod quic_client;

#[macro_use]
extern crate solana_metrics;

use {
    crate::{
        nonblocking::quic_client::{
            QuicClient, QuicClientCertificate,
            QuicClientConnection as NonblockingQuicClientConnection, QuicLazyInitializedEndpoint,
        },
        quic_client::QuicClientConnection as BlockingQuicClientConnection,
    },
    quinn::Endpoint,
    solana_connection_cache::{
        client_connection::ClientConnection as BlockingClientConnection,
        connection_cache::{
            BaseClientConnection, ClientError, ConnectionManager, ConnectionPool,
            ConnectionPoolError, NewConnectionConfig,
        },
        connection_cache_stats::ConnectionCacheStats,
        nonblocking::client_connection::ClientConnection as NonblockingClientConnection,
    },
    solana_sdk::{pubkey::Pubkey, quic::QUIC_PORT_OFFSET, signature::Keypair},
    solana_streamer::{
        nonblocking::quic::{compute_max_allowed_uni_streams, ConnectionPeerType},
        streamer::StakedNodes,
        tls_certificates::new_self_signed_tls_certificate,
    },
    std::{
        any::Any,
        error::Error,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        sync::{Arc, RwLock},
    },
    thiserror::Error,
};

#[derive(Error, Debug)]
pub enum QuicClientError {
    #[error("Certificate error: {0}")]
    CertificateError(String),
}

pub struct QuicPool {
    connections: Vec<Arc<Box<dyn BaseClientConnection>>>,
    endpoint: Arc<QuicLazyInitializedEndpoint>,
}
impl ConnectionPool for QuicPool {
    fn add_connection(&mut self, config: &dyn NewConnectionConfig, addr: &SocketAddr) {
        let connection = Arc::new(self.create_pool_entry(config, addr));
        self.connections.push(connection);
    }

    fn num_connections(&self) -> usize {
        self.connections.len()
    }

    fn get(&self, index: usize) -> Result<Arc<Box<dyn BaseClientConnection>>, ConnectionPoolError> {
        self.connections
            .get(index)
            .cloned()
            .ok_or(ConnectionPoolError::IndexOutOfRange)
    }

    fn create_pool_entry(
        &self,
        config: &dyn NewConnectionConfig,
        addr: &SocketAddr,
    ) -> Box<dyn BaseClientConnection> {
        let config: &QuicConfig = match config.as_any().downcast_ref::<QuicConfig>() {
            Some(b) => b,
            None => panic!("Expecting a QuicConfig!"),
        };
        Box::new(Quic(Arc::new(QuicClient::new(
            self.endpoint.clone(),
            *addr,
            config.compute_max_parallel_streams(),
        ))))
    }
}

pub struct QuicConfig {
    client_certificate: Arc<QuicClientCertificate>,
    maybe_staked_nodes: Option<Arc<RwLock<StakedNodes>>>,
    maybe_client_pubkey: Option<Pubkey>,

    // The optional specified endpoint for the quic based client connections
    // If not specified, the connection cache will create as needed.
    client_endpoint: Option<Endpoint>,
}

impl NewConnectionConfig for QuicConfig {
    fn new() -> Result<Self, ClientError> {
        let (cert, priv_key) =
            new_self_signed_tls_certificate(&Keypair::new(), IpAddr::V4(Ipv4Addr::UNSPECIFIED))
                .map_err(|err| ClientError::CertificateError(err.to_string()))?;
        Ok(Self {
            client_certificate: Arc::new(QuicClientCertificate {
                certificate: cert,
                key: priv_key,
            }),
            maybe_staked_nodes: None,
            maybe_client_pubkey: None,
            client_endpoint: None,
        })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_mut_any(&mut self) -> &mut dyn Any {
        self
    }
}

impl QuicConfig {
    fn create_endpoint(&self) -> QuicLazyInitializedEndpoint {
        QuicLazyInitializedEndpoint::new(
            self.client_certificate.clone(),
            self.client_endpoint.as_ref().cloned(),
        )
    }

    fn compute_max_parallel_streams(&self) -> usize {
        let (client_type, stake, total_stake) =
            self.maybe_client_pubkey
                .map_or((ConnectionPeerType::Unstaked, 0, 0), |pubkey| {
                    self.maybe_staked_nodes.as_ref().map_or(
                        (ConnectionPeerType::Unstaked, 0, 0),
                        |stakes| {
                            let rstakes = stakes.read().unwrap();
                            rstakes.pubkey_stake_map.get(&pubkey).map_or(
                                (ConnectionPeerType::Unstaked, 0, rstakes.total_stake),
                                |stake| (ConnectionPeerType::Staked, *stake, rstakes.total_stake),
                            )
                        },
                    )
                });
        compute_max_allowed_uni_streams(client_type, stake, total_stake)
    }

    pub fn update_client_certificate(
        &mut self,
        keypair: &Keypair,
        ipaddr: IpAddr,
    ) -> Result<(), Box<dyn Error>> {
        let (cert, priv_key) = new_self_signed_tls_certificate(keypair, ipaddr)?;
        self.client_certificate = Arc::new(QuicClientCertificate {
            certificate: cert,
            key: priv_key,
        });
        Ok(())
    }

    pub fn set_staked_nodes(
        &mut self,
        staked_nodes: &Arc<RwLock<StakedNodes>>,
        client_pubkey: &Pubkey,
    ) {
        self.maybe_staked_nodes = Some(staked_nodes.clone());
        self.maybe_client_pubkey = Some(*client_pubkey);
    }

    pub fn update_client_endpoint(&mut self, client_endpoint: Endpoint) {
        self.client_endpoint = Some(client_endpoint);
    }
}

pub struct Quic(Arc<QuicClient>);
impl BaseClientConnection for Quic {
    fn new_blocking_connection(
        &self,
        _addr: SocketAddr,
        stats: Arc<ConnectionCacheStats>,
    ) -> Arc<Box<dyn BlockingClientConnection>> {
        Arc::new(Box::new(BlockingQuicClientConnection::new_with_client(
            self.0.clone(),
            stats,
        )))
    }

    fn new_nonblocking_connection(
        &self,
        _addr: SocketAddr,
        stats: Arc<ConnectionCacheStats>,
    ) -> Arc<Box<dyn NonblockingClientConnection>> {
        Arc::new(Box::new(NonblockingQuicClientConnection::new_with_client(
            self.0.clone(),
            stats,
        )))
    }
}

#[derive(Default)]
pub struct QuicConnectionManager {
    connection_config: Option<Box<dyn NewConnectionConfig>>,
}

impl ConnectionManager for QuicConnectionManager {
    fn new_connection_pool(&self) -> Box<dyn ConnectionPool> {
        Box::new(QuicPool {
            connections: Vec::default(),
            endpoint: Arc::new(self.connection_config.as_ref().map_or(
                QuicLazyInitializedEndpoint::default(),
                |config| {
                    let config: &QuicConfig = match config.as_any().downcast_ref::<QuicConfig>() {
                        Some(b) => b,
                        None => panic!("Expecting a QuicConfig!"),
                    };

                    config.create_endpoint()
                },
            )),
        })
    }

    fn new_connection_config(&self) -> Box<dyn NewConnectionConfig> {
        Box::new(QuicConfig::new().unwrap())
    }

    fn get_port_offset(&self) -> u16 {
        QUIC_PORT_OFFSET
    }
}

impl QuicConnectionManager {
    pub fn new_with_connection_config(config: QuicConfig) -> Self {
        Self {
            connection_config: Some(Box::new(config)),
        }
    }
}
#[cfg(test)]
mod tests {
    use {
        super::*,
        solana_connection_cache::connection_cache::{
            ConnectionCache, DEFAULT_CONNECTION_POOL_SIZE,
        },
        solana_sdk::quic::{
            QUIC_MAX_UNSTAKED_CONCURRENT_STREAMS, QUIC_MIN_STAKED_CONCURRENT_STREAMS,
            QUIC_TOTAL_STAKED_CONCURRENT_STREAMS,
        },
    };

    #[test]
    fn test_connection_cache_max_parallel_chunks() {
        solana_logger::setup();
        let connection_manager = Box::<QuicConnectionManager>::default();
        let connection_cache =
            ConnectionCache::new(connection_manager, DEFAULT_CONNECTION_POOL_SIZE).unwrap();
        let mut connection_config = connection_cache.connection_config;

        let connection_config: &mut QuicConfig =
            match connection_config.as_mut_any().downcast_mut::<QuicConfig>() {
                Some(b) => b,
                None => panic!("Expecting a QuicConfig!"),
            };

        assert_eq!(
            connection_config.compute_max_parallel_streams(),
            QUIC_MAX_UNSTAKED_CONCURRENT_STREAMS
        );

        let staked_nodes = Arc::new(RwLock::new(StakedNodes::default()));
        let pubkey = Pubkey::new_unique();
        connection_config.set_staked_nodes(&staked_nodes, &pubkey);
        assert_eq!(
            connection_config.compute_max_parallel_streams(),
            QUIC_MAX_UNSTAKED_CONCURRENT_STREAMS
        );

        staked_nodes.write().unwrap().total_stake = 10000;
        assert_eq!(
            connection_config.compute_max_parallel_streams(),
            QUIC_MAX_UNSTAKED_CONCURRENT_STREAMS
        );

        staked_nodes
            .write()
            .unwrap()
            .pubkey_stake_map
            .insert(pubkey, 1);

        let delta =
            (QUIC_TOTAL_STAKED_CONCURRENT_STREAMS - QUIC_MIN_STAKED_CONCURRENT_STREAMS) as f64;

        assert_eq!(
            connection_config.compute_max_parallel_streams(),
            (QUIC_MIN_STAKED_CONCURRENT_STREAMS as f64 + (1f64 / 10000f64) * delta) as usize
        );

        staked_nodes
            .write()
            .unwrap()
            .pubkey_stake_map
            .remove(&pubkey);
        staked_nodes
            .write()
            .unwrap()
            .pubkey_stake_map
            .insert(pubkey, 1000);
        assert_ne!(
            connection_config.compute_max_parallel_streams(),
            QUIC_MIN_STAKED_CONCURRENT_STREAMS
        );
    }
}
