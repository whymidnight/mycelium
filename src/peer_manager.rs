use crate::connection::Quic;
use crate::endpoint::{Endpoint, Protocol};
use crate::peer::{Peer, PeerRef};
use crate::router::Router;
use crate::router_id::RouterId;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use log::{debug, error, info, trace, warn};
use quinn::{EndpointConfig, ServerConfig};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::net::{TcpListener, UdpSocket};
use tokio::time::MissedTickBehavior;

/// Magic bytes to identify a multicast UDP packet used in link local peer discovery.
const MYCELIUM_MULTICAST_DISCOVERY_MAGIC: &[u8; 8] = b"mycelium";
/// Size of a peer discovery beacon.
const PEER_DISCOVERY_BEACON_SIZE: usize = 8 + 2 + 40;
/// Link local peer discovery group joined by the UDP listener.
const LL_PEER_DISCOVERY_GROUP: &str = "ff02::cafe";
/// The time between sending consecutive link local discovery beacons.
const LL_PEER_DISCOVERY_BEACON_INTERVAL: Duration = Duration::from_secs(60);
/// The time between checking known peer liveness and trying to reconnect.
const PEER_CONNECT_INTERVAL: Duration = Duration::from_secs(5);

/// The PeerManager creates new peers by connecting to configured addresses, and setting up the
/// connection. Once a connection is established, the created [`Peer`] is handed over to the
/// [`Router`].
#[derive(Clone)]
pub struct PeerManager {
    inner: Arc<Inner>,
}

/// Details how the PeerManager learned about a remote.
#[derive(PartialEq, Eq)]
enum PeerType {
    /// Statically configured peer.
    Static,
    /// Peer found through link local discovery.
    LinkLocalDiscovery,
    /// A remote which initiated a connection to us.
    Inbound,
}

/// Local info about a peer.
struct PeerInfo {
    /// Details how we found out about this peer.
    pt: PeerType,
    /// Are we currently connecting to this peer?
    connecting: bool,
    /// The [`PeerRef`] used to check liveliness.
    pr: PeerRef,
}

struct Inner {
    /// Router is unfortunately wrapped in a Mutex, because router is not Sync.
    router: Mutex<Router>,
    peers: Mutex<HashMap<Endpoint, PeerInfo>>,
    /// Listen port for new peer connections
    tcp_listen_port: u16,
    quic_listen_port: u16,
}

impl PeerManager {
    pub fn new(
        router: Router,
        static_peers_sockets: Vec<Endpoint>,
        tcp_listen_port: u16,
        quic_listen_port: u16,
        peer_discovery_port: u16,
        disable_peer_discovery: bool,
    ) -> Self {
        let peer_manager = PeerManager {
            inner: Arc::new(Inner {
                router: Mutex::new(router),
                peers: Mutex::new(
                    static_peers_sockets
                        .into_iter()
                        // These peers are not alive, but we say they are because the reconnect
                        // loop will perform the actual check and figure out they are dead, then
                        // (re)connect.
                        .map(|s| {
                            (
                                // For now we only support TCP, so just extract the address.
                                s,
                                PeerInfo {
                                    pt: PeerType::Static,
                                    connecting: false,
                                    pr: PeerRef::new(),
                                },
                            )
                        })
                        .collect(),
                ),
                tcp_listen_port,
                quic_listen_port,
            }),
        };

        // Start listeners for inbound connections.
        tokio::spawn(peer_manager.inner.clone().tcp_listener());
        tokio::spawn(peer_manager.inner.clone().quic_listener());

        // Start (re)connecting to outbound/local peers
        tokio::spawn(peer_manager.inner.clone().connect_to_peers());

        // Discover local peers, this does not actually connect to them. That is handle by the
        // connect_to_peers task.
        if !disable_peer_discovery {
            tokio::spawn(
                peer_manager
                    .inner
                    .clone()
                    .local_discovery(peer_discovery_port),
            );
        }

        peer_manager
    }
}

impl Inner {
    /// Connect and if needed reconnect to known peers.
    async fn connect_to_peers(self: Arc<Self>) {
        let mut peer_check_interval = tokio::time::interval(PEER_CONNECT_INTERVAL);
        // Avoid trying to spam connections. Since we track if we are connecting to a peer this
        // won't be that bad, but this avoid unnecessary lock contention.
        peer_check_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        // A list of pending connection futures. Like this, we don't have to spawn a new future for
        // every connection task.
        let mut connection_futures = FuturesUnordered::new();

        loop {
            tokio::select! {
                // We don't care about the none case, that can happen when we aren't connecting to
                // any peers.
                Some((endpoint, maybe_new_peer)) = connection_futures.next() => {
                    // Only insert the possible new peer if we actually still care about it
                    if let Some(pi) = self.peers.lock().unwrap().get_mut(&endpoint) {
                        // Regardless of what happened, we are no longer connecting.
                        pi.connecting = false;
                        if let Some(peer) = maybe_new_peer {
                            // We did find a new Peer, insert into router and keep track of it
                            // Use fully qualified call to aid compiler in type inference.
                            pi.pr = Peer::refer(&peer);
                            self.router.lock().unwrap().add_peer_interface(peer);
                        }
                    }
                }
                _ = peer_check_interval.tick() => {
                    // Remove dead inbound peers
                    self.peers.lock().unwrap().retain(|_, v| v.pt != PeerType::Inbound || v.pr.alive());
                    debug!("Looking for dead peers");
                    // check if there is an entry for the peer in the router's peer list
                    for (endpoint, pi) in self.peers.lock().unwrap().iter_mut() {
                        if !pi.connecting && !pi.pr.alive() {
                            debug!("Found dead peer {endpoint}");
                            if pi.pt == PeerType::Inbound {
                                debug!("Refusing to reconnect to inbound peer");
                                continue
                            }
                            // Mark that we are connecting to the peer.
                            pi.connecting = true;
                            connection_futures.push(self.clone().connect_peer(*endpoint));
                        }
                    }
                }
            }
        }
    }

    /// Create a new connection to a remote peer
    async fn connect_peer(self: Arc<Self>, endpoint: Endpoint) -> (Endpoint, Option<Peer>) {
        debug!("Connecting to {endpoint}");
        match endpoint.proto() {
            Protocol::Tcp => self.connect_tcp_peer(endpoint).await,
            Protocol::Quic => self.connect_quic_peer(endpoint).await,
        }
    }

    async fn connect_tcp_peer(self: Arc<Self>, endpoint: Endpoint) -> (Endpoint, Option<Peer>) {
        match TcpStream::connect(endpoint.address()).await {
            Ok(peer_stream) => {
                debug!("Opened connection to {endpoint}");
                // Make sure Nagle's algorithm is disabeld as it can cause latency spikes.
                if let Err(e) = peer_stream.set_nodelay(true) {
                    error!("Couldn't disable Nagle's algorithm on stream {e}");
                    return (endpoint, None);
                }

                // Scope the MutexGuard, if we don't do this the future won't be Send
                match {
                    let router = self.router.lock().unwrap();
                    let router_data_tx = router.router_data_tx();
                    let router_control_tx = router.router_control_tx();
                    let dead_peer_sink = router.dead_peer_sink().clone();

                    Peer::new(
                        router_data_tx,
                        router_control_tx,
                        peer_stream,
                        dead_peer_sink,
                    )
                } {
                    Ok(new_peer) => {
                        info!("Connected to new peer {}", endpoint);
                        (endpoint, Some(new_peer))
                    }
                    Err(e) => {
                        error!("Failed to spawn peer: {e}");
                        (endpoint, None)
                    }
                }
            }
            Err(e) => {
                error!("Couldn't connect to to remote {e}");
                (endpoint, None)
            }
        }
    }

    async fn connect_quic_peer(self: Arc<Self>, endpoint: Endpoint) -> (Endpoint, Option<Peer>) {
        todo!();
    }

    async fn tcp_listener(self: Arc<Self>) {
        // Take a copy of every channel here first so we avoid lock contention in the loop later.
        let router_data_tx = self.router.lock().unwrap().router_data_tx();
        let router_control_tx = self.router.lock().unwrap().router_control_tx();
        let dead_peer_sink = self.router.lock().unwrap().dead_peer_sink().clone();

        match TcpListener::bind(("::", self.tcp_listen_port)).await {
            Ok(listener) => loop {
                match listener.accept().await {
                    Ok((stream, remote)) => {
                        let new_peer = match Peer::new(
                            router_data_tx.clone(),
                            router_control_tx.clone(),
                            stream,
                            dead_peer_sink.clone(),
                        ) {
                            Ok(peer) => peer,
                            Err(e) => {
                                error!("Failed to spawn peer: {e}");
                                continue;
                            }
                        };
                        info!("Accepted new inbound peer {}", remote);
                        self.add_peer(
                            Endpoint::new(Protocol::Tcp, remote),
                            PeerType::Inbound,
                            Some(new_peer),
                        );
                    }
                    Err(e) => {
                        error!("Error accepting connection: {}", e);
                    }
                }
            },
            Err(e) => {
                error!("Error starting listener: {}", e);
            }
        }
    }

    async fn quic_listener(self: Arc<Self>) {
        // Take a copy of every channel here first so we avoid lock contention in the loop later.
        let router_data_tx = self.router.lock().unwrap().router_data_tx();
        let router_control_tx = self.router.lock().unwrap().router_control_tx();
        let dead_peer_sink = self.router.lock().unwrap().dead_peer_sink().clone();

        // Generate self signed certificate certificate.
        // TODO: sign with router keys
        let cert = rcgen::generate_simple_self_signed(vec![format!(
            "{}",
            self.router.lock().unwrap().router_id()
        )])
        .expect("Can generate a self signed certificate; qed");
        let certificate_der = cert
            .serialize_der()
            .expect("Can serialize self signed certificate to DER format; qed");
        let private_key_der = cert.serialize_private_key_der();
        let private_key = rustls::PrivateKey(private_key_der);
        let certificate_chain = vec![rustls::Certificate(certificate_der)];

        let mut server_config = ServerConfig::with_single_cert(certificate_chain, private_key)
            .expect("Can generate server config with self signed certificate; qed");
        // We can unwrap this since it's the only current instance.
        let transport_config = Arc::get_mut(&mut server_config.transport).unwrap();
        // Larger than needed for now
        transport_config.max_concurrent_uni_streams(3_u8.into());
        transport_config.max_concurrent_bidi_streams(5_u8.into());
        // TODO: further tweak this.

        let client_crypto = rustls::ClientConfig::builder()
            .with_safe_defaults()
            .with_custom_certificate_verifier(SkipServerVerification::new())
            .with_no_client_auth();

        let socket = std::net::UdpSocket::bind(("[::]", self.quic_listen_port)).expect("TODO");

        //TODO
        let endpoint = quinn::Endpoint::new(
            quinn::EndpointConfig::default(),
            Some(server_config),
            socket,
            quinn::default_runtime()
                .expect("We are inside the tokio-runtime so this always returns Some(); qed"),
        )
        .expect("Can construct quinn endpoint; qed");

        loop {
            let con = if let Some(con) = endpoint.accept().await {
                match con.await {
                    Ok(con) => con,
                    Err(e) => {
                        debug!("Failed to accept quic connection: {e}");
                        continue;
                    }
                }
            } else {
                // Con is closed
                info!("Shutting down closed quic listener");
                return error!("Failed to accept quic connection");
            };

            let q = match con.accept_bi().await {
                Ok((tx, rx)) => Quic::new(tx, rx, con.remote_address()),
                Err(e) => {
                    error!("Failed to accept bidirectional quic stream: {e}");
                    continue;
                }
            };

            let new_peer = match Peer::new(
                router_data_tx.clone(),
                router_control_tx.clone(),
                q,
                dead_peer_sink.clone(),
            ) {
                Ok(peer) => peer,
                Err(e) => {
                    error!("Failed to spawn peer: {e}");
                    continue;
                }
            };
            info!("Accepted new inbound peer {}", con.remote_address());
            self.add_peer(
                Endpoint::new(Protocol::Quic, con.remote_address()),
                PeerType::Inbound,
                Some(new_peer),
            );
        }
    }

    /// Add a new peer identifier we discovered.
    fn add_peer(&self, endpoint: Endpoint, discovery_type: PeerType, peer: Option<Peer>) {
        let mut peers = self.peers.lock().unwrap();
        // Only if we don't know it yet.
        if let Entry::Vacant(e) = peers.entry(endpoint) {
            e.insert(PeerInfo {
                pt: discovery_type,
                connecting: false,
                pr: if let Some(p) = &peer {
                    p.refer()
                } else {
                    PeerRef::new()
                },
            });
            if let Some(p) = peer {
                self.router.lock().unwrap().add_peer_interface(p);
            }
            info!("Added new peer {endpoint}");
        } else {
            debug!("Ignoring request to add {endpoint} as it already exists");
        }
    }

    /// Use multicast discovery to find local peers.
    async fn local_discovery(self: Arc<Self>, peer_discovery_port: u16) {
        let rid = self.router.lock().unwrap().router_id();

        let multicast_destination = LL_PEER_DISCOVERY_GROUP
            .parse()
            .expect("Link local discovery group address is properly defined");
        let sock = match UdpSocket::bind(SocketAddr::new(
            "::".parse().expect("Valid all interface IPv6 designator"),
            peer_discovery_port,
        ))
        .await
        {
            Ok(sock) => sock,
            Err(e) => {
                // We won't participate in link local discovery
                error!("Failed to bind multicast discovery socket: {e}");
                warn!("Link local peer discovery disabled");
                return;
            }
        };

        info!(
            "Bound multicast discovery interface to {}",
            sock.local_addr().expect("can look up our own address")
        );
        if let Err(e) = sock.join_multicast_v6(&multicast_destination, 0) {
            error!("Failed to join multicast group {e}");
            warn!("Link local peer discovery disabled");
            return;
        }

        let mut beacon = [0; PEER_DISCOVERY_BEACON_SIZE];
        beacon[..8].copy_from_slice(MYCELIUM_MULTICAST_DISCOVERY_MAGIC);
        beacon[8..10].copy_from_slice(&self.tcp_listen_port.to_be_bytes());
        beacon[10..50].copy_from_slice(&rid.as_bytes());

        let mut send_timer = tokio::time::interval(LL_PEER_DISCOVERY_BEACON_INTERVAL);
        send_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            let mut buf = [0; PEER_DISCOVERY_BEACON_SIZE];
            tokio::select! {
                _ = send_timer.tick() => {
                    if let Err(e) = sock.send_to(
                        &beacon,
                        SocketAddr::new(multicast_destination.into(), peer_discovery_port),
                    )
                    .await {
                        error!("Could not send multicast discovery beacon {e}");
                    }
                },
                recv_res = sock.recv_from(&mut buf) => {
                    match recv_res {
                        Err(e) => {
                            warn!("Failed to receive multicast message {e}");
                            continue;
                        }
                        Ok((n, remote)) => {
                            trace!("Received {n} bytes from {remote}");
                            self.handle_discovery_packet(&buf[..n], remote);
                        }
                    }
                }
            }
        }
    }

    /// Validates an incoming discovery packet. If the packet is valid, the peer is added to the
    /// `PeerManager`.
    fn handle_discovery_packet(&self, packet: &[u8], mut remote: SocketAddr) {
        if let IpAddr::V6(ip) = remote.ip() {
            // Dumb subnet validation, we only want to discover link local addresses,
            // i.e. part of fe80::/64
            if ip.octets()[..8] != [0xfe, 0x80, 0, 0, 0, 0, 0, 0] {
                trace!("Ignoring non link local IPv6 discovery packet from {remote}");
                return;
            }
        } else {
            trace!("Ignoring non IPv6 discovery packet from {remote}");
            return;
        }
        if packet.len() != PEER_DISCOVERY_BEACON_SIZE {
            trace!("Ignore invalid sized multicast announcement");
            return;
        }
        if &packet[..8] != MYCELIUM_MULTICAST_DISCOVERY_MAGIC {
            trace!("Ignore announcement with invalid multicast magic");
            return;
        }
        let port = u16::from_be_bytes(
            packet[8..10]
                .try_into()
                .expect("Slice size is valid for u16"),
        );
        let remote_rid = RouterId::from(
            <&[u8] as TryInto<[u8; 40]>>::try_into(&packet[10..50])
                .expect("Slice size is valid for RouterId"),
        );
        let rid = self.router.lock().unwrap().router_id();
        if remote_rid == rid {
            debug!("Ignore discovery beacon we sent earlier");
            return;
        }
        // Override the port. Care must be taken since link local IPv6 expects the
        // scope_id to be set.
        remote.set_port(port);
        self.add_peer(
            Endpoint::new(Protocol::Tcp, remote),
            PeerType::LinkLocalDiscovery,
            None,
        );
    }
}

/// Dummy certificate verifier that treats any certificate as valid.
struct SkipServerVerification;

impl SkipServerVerification {
    fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl rustls::client::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::Certificate,
        _intermediates: &[rustls::Certificate],
        _server_name: &rustls::ServerName,
        _scts: &mut dyn Iterator<Item = &[u8]>,
        _ocsp_response: &[u8],
        _now: std::time::SystemTime,
    ) -> Result<rustls::client::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::ServerCertVerified::assertion())
    }
}
