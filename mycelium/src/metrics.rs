//! This module is used for collection of runtime metrics of a `mycelium` system. The  main item of
//! interest is the [`Metrics`] trait. Users can provide their own implementation of this, or use
//! the default provided implementation to disable gathering metrics.

use crate::peer_manager::PeerType;

/// The collection of all metrics exported by a [`mycelium node`](crate::Node). It is up to the
/// user to provide an implementation which implements the methods for metrics they are interested
/// in. All methods have a default implementation, so if the user is not interested in any metrics,
/// a NOOP handler can be implemented as follows:
///
/// ```rust
/// use mycelium::metrics::Metrics;
///
/// #[derive(Clone)]
/// struct NoMetrics;
/// impl Metrics for NoMetrics {}
/// ```
pub trait Metrics {
    /// The [`Router`](crate::router::Router) received a new Hello TLV from a peer.
    #[inline]
    fn router_incoming_hello(&self) {}

    /// The [`Router`](crate::router::Router) received a new IHU TLV from a peer.
    #[inline]
    fn router_incoming_ihu(&self) {}

    /// The [`Router`](crate::router::Router) received a new Seqno request TLV from a peer.
    #[inline]
    fn router_incoming_seqno_request(&self) {}

    /// The [`Router`](crate::router::Router) received a new Route request TLV from a peer.
    /// Additionally, it is recorded if this is a wildcard request (route table dump request)
    /// or a request for a specific subnet.
    #[inline]
    fn router_incoming_route_request(&self, _wildcard: bool) {}

    /// The [`Router`](crate::router::Router) received a new Update TLV from a peer.
    #[inline]
    fn router_incoming_update(&self) {}

    /// The [`Router`](crate::router::Router) received a new TLV from a peer, but the type is unknown.
    #[inline]
    fn router_incoming_unknown_tlv(&self) {}

    /// A [`Peer`](crate::peer::Peer) was added to the [`Router`](crate::router::Router).
    #[inline]
    fn router_peer_added(&self) {}

    /// A [`Peer`](crate::peer::Peer) was removed from the [`Router`](crate::router::Router).
    #[inline]
    fn router_peer_removed(&self) {}

    /// A [`Peer`](crate::peer::Peer) informed the [`Router`](crate::router::Router) it died, or
    /// the router otherwise noticed the Peer is dead.
    #[inline]
    fn router_peer_died(&self) {}

    /// The [`Router`](crate::router::Router) ran a route selection procedure.
    #[inline]
    fn router_route_selection_ran(&self) {}

    /// A [`SourceKey`](crate::source_table::SourceKey) expired and got cleaned up by the [`Router`](crate::router::Router).
    #[inline]
    fn router_source_key_expired(&self) {}

    /// A [`RouteKey`](crate::routing_table::RouteKey) expired, and the router either set the
    /// [`Metric`](crate::metric::Metric) of the route to infinity, or cleaned up the route entry
    /// altogether.
    #[inline]
    fn router_route_key_expired(&self, _removed: bool) {}

    /// A route which expired was actually the selected route for the
    /// [`Subnet`](crate::subnet::Subnet). Note that [`Self::router_route_key_expired`] will
    /// also have been called.
    #[inline]
    fn router_selected_route_expired(&self) {}

    /// The [`Router`](crate::router::Router) sends a "triggered" update to it's peers.
    #[inline]
    fn router_triggered_update(&self) {}

    /// The [`Router`](crate::router::Router) got a packet to route.
    #[inline]
    fn router_route_packet(&self) {}

    /// The [`Router`](crate::router::Router) extracted a packet for the local subnet.
    #[inline]
    fn router_route_packet_local(&self) {}

    /// The [`Router`](crate::router::Router) dropped a packet it was routing because it's TTL
    /// reached 0.
    #[inline]
    fn router_route_packet_ttl_expired(&self) {}

    /// The [`Router`](crate::router::Router) dropped a packet it was routing because there was no
    /// route for the destination IP.
    #[inline]
    fn router_route_packet_no_route(&self) {}

    /// A new [`Peer`](crate::peer::Peer) was added to the
    /// [`PeerManager`](crate::peer_manager::PeerManager) while it is running.
    #[inline]
    fn peer_manager_peer_added(&self, _pt: PeerType) {}

    /// Sets the amount of [`Peers`](crate::peer::Peer) known by the
    /// [`PeerManager`](crate::peer_manager::PeerManager).
    #[inline]
    fn peer_manager_known_peers(&self, _amount: usize) {}

    /// The [`PeerManager`](crate::peer_manager::PeerManager) started an attempt to connect to a
    /// remote endpoint.
    #[inline]
    fn peer_manager_connection_attempted(&self) {}

    /// The [`PeerManager`](crate::peer_manager::PeerManager) finished an attempt to connect to a
    /// remote endpoint. The connection could have failed.
    #[inline]
    fn peer_manager_connection_finished(&self) {}
}