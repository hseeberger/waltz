use crate::{
    ActorId,
    mailbox::{ActorTerminated, TerminatedSink, Watcher},
    remote::{
        endpoint::{self, EndpointInner},
        frame::Frame,
        node::NodeId,
    },
    sync::lock,
};
use log::{debug, error, warn};
use std::{
    collections::{HashMap, hash_map::Entry},
    sync::{Arc, Mutex, MutexGuard},
};

type Watchers = HashMap<ActorId, Watcher>;

/// The local watchers of actors on other nodes.
pub(crate) struct WatcherTable(PeerKeyed<Watchers>);

impl WatcherTable {
    pub(crate) fn new() -> Self {
        Self(PeerKeyed::new())
    }

    /// `true` if it is the first watcher for the peer and target, i.e. a `Watch` frame is due.
    pub(crate) fn add(&self, peer: NodeId, target: ActorId, watcher: Watcher) -> bool {
        match self.0.entries().entry(peer).or_default().entry(target) {
            Entry::Vacant(entry) => {
                entry.insert(HashMap::from([(watcher.watcher_id(), watcher)]));
                true
            }

            Entry::Occupied(mut entry) => {
                entry
                    .get_mut()
                    .entry(watcher.watcher_id())
                    .or_insert(watcher);
                false
            }
        }
    }

    /// `true` if it was the last watcher for the peer and target, i.e. an `Unwatch` frame is due.
    pub(crate) fn remove(&self, peer: NodeId, target: ActorId, watcher_id: ActorId) -> bool {
        let mut peers = self.0.entries();
        let Some(targets) = peers.get_mut(&peer) else {
            return false;
        };
        let Some(watchers) = targets.get_mut(&target) else {
            return false;
        };

        if watchers.remove(&watcher_id).is_none() || !watchers.is_empty() {
            return false;
        }

        PeerKeyed::prune(&mut peers, peer, target);
        true
    }

    pub(crate) fn take_target(&self, peer: NodeId, target: ActorId) -> Vec<Watcher> {
        self.0
            .take_target(peer, target)
            .map(|watchers| watchers.into_values().collect())
            .unwrap_or_default()
    }

    pub(crate) fn take_peer(&self, peer: NodeId) -> Vec<(ActorId, Vec<Watcher>)> {
        self.0
            .take_peer(peer)
            .into_iter()
            .map(|(target, watchers)| (target, watchers.into_values().collect()))
            .collect()
    }

    pub(crate) fn peers(&self) -> Vec<NodeId> {
        self.0.peers()
    }

    pub(crate) fn targets(&self, peer: NodeId) -> Vec<ActorId> {
        self.0.targets(peer)
    }
}

/// The wire watches other nodes hold on local actors.
pub(crate) struct WireWatchTable(PeerKeyed<WireWatch>);

impl WireWatchTable {
    pub(crate) fn new() -> Self {
        Self(PeerKeyed::new())
    }

    /// Only the registered watches have a watcher to deregister.
    pub(crate) fn take_peer(&self, peer: NodeId) -> Vec<RegisteredWatch> {
        self.0
            .take_peer(peer)
            .into_iter()
            .filter_map(|(target, watch)| match watch {
                WireWatch::Registered(wire_watcher_id) => Some(RegisteredWatch {
                    target,
                    wire_watcher_id,
                }),

                WireWatch::Pending => None,
            })
            .collect()
    }

    pub(crate) fn peers(&self) -> Vec<NodeId> {
        self.0.peers()
    }

    /// `false` if the wire watch already exists, making repeated `Watch` frames idempotent.
    fn add_pending(&self, peer: NodeId, target: ActorId) -> bool {
        match self.0.entries().entry(peer).or_default().entry(target) {
            Entry::Occupied(_) => false,

            Entry::Vacant(entry) => {
                entry.insert(WireWatch::Pending);
                true
            }
        }
    }

    fn confirm(&self, peer: NodeId, target: ActorId, wire_watcher_id: ActorId) {
        if let Some(entry) = self
            .0
            .entries()
            .get_mut(&peer)
            .and_then(|targets| targets.get_mut(&target))
        {
            *entry = WireWatch::Registered(wire_watcher_id);
        }
    }

    fn take(&self, peer: NodeId, target: ActorId) -> Option<ActorId> {
        match self.0.take_target(peer, target) {
            Some(WireWatch::Registered(wire_watcher_id)) => Some(wire_watcher_id),
            Some(WireWatch::Pending) | None => None,
        }
    }
}

/// The tombstone must be read again after registering, not only before: a node death in between
/// has already taken that node's watchers, so a watcher registered behind it would never be
/// signaled. Liveness is tracked only past that second read, else a dead incarnation is tracked
/// again and never untracked.
pub(crate) fn watch_remote(peer: NodeId, target: ActorId, watcher: Watcher) {
    let Some(endpoint) = endpoint::get() else {
        error!(
            watcher_id:% = watcher.watcher_id(),
            other_id:% = target;
            "cannot watch a remote actor, remoting endpoint not started"
        );
        return;
    };

    if endpoint.tombstoned(peer.incarnation()) {
        fire(&watcher, target);
        return;
    }

    let first = endpoint.watchers().add(peer, target, watcher);

    if endpoint.tombstoned(peer.incarnation()) {
        fire_all(endpoint.watchers().take_target(peer, target), target);
        return;
    }

    endpoint.track_liveness(peer);

    if first && endpoint.send(peer, Frame::Watch { target }).is_err() {
        fire_all(endpoint.watchers().take_target(peer, target), target);
    }
}

pub(crate) fn unwatch_remote(peer: NodeId, target: ActorId, watcher_id: ActorId) {
    let Some(endpoint) = endpoint::get() else {
        return;
    };

    if endpoint.watchers().remove(peer, target, watcher_id)
        && let Err(error) = endpoint.send(peer, Frame::Unwatch { target })
    {
        debug!(peer:% = peer, actor_id:% = target, error:%; "cannot revert the wire watch");
    }
}

/// Called synchronously by the reader task, so the signal stays ordered behind the message
/// frames delivered before it.
pub(crate) fn on_terminated(endpoint: &EndpointInner, peer: NodeId, target: ActorId) {
    fire_all(endpoint.watchers().take_target(peer, target), target);
}

/// An unknown or already terminated target is answered with a `Terminated` frame right away,
/// mirroring the local race-free registration.
pub(crate) fn on_watch(endpoint: &'static EndpointInner, peer: NodeId, target: ActorId) {
    if !endpoint.wire_watches().add_pending(peer, target) {
        return;
    }

    let registered = endpoint
        .registry()
        .watcher_registry(target)
        .and_then(|watcher_registry| {
            let wire_watcher_id = ActorId::new();
            let sink = Arc::new(WireTerminatedSink { endpoint, peer });
            watcher_registry
                .add(Watcher::new(wire_watcher_id, sink))
                .ok()
                .map(|()| wire_watcher_id)
        });

    match registered {
        Some(wire_watcher_id) => {
            endpoint
                .wire_watches()
                .confirm(peer, target, wire_watcher_id);
        }

        None => {
            endpoint.wire_watches().take(peer, target);
            if let Err(error) = endpoint.send(peer, Frame::Terminated { target }) {
                warn!(
                    peer:% = peer,
                    actor_id:% = target,
                    error:%;
                    "cannot answer watch for an already terminated actor"
                );
            }
        }
    }
}

pub(crate) fn on_unwatch(endpoint: &EndpointInner, peer: NodeId, target: ActorId) {
    if let Some(wire_watcher_id) = endpoint.wire_watches().take(peer, target)
        && let Some(watcher_registry) = endpoint.registry().watcher_registry(target)
    {
        watcher_registry.remove(wire_watcher_id);
    }
}

pub(crate) struct RegisteredWatch {
    pub(crate) target: ActorId,
    pub(crate) wire_watcher_id: ActorId,
}

/// Keyed by peer, then target, with no empty per-peer level: every removal prunes a peer whose
/// last target went away, which is what lets [PeerKeyed::peers] shrink and heartbeating stop.
struct PeerKeyed<V>(Mutex<HashMap<NodeId, HashMap<ActorId, V>>>);

impl<V> PeerKeyed<V> {
    fn new() -> Self {
        Self(Mutex::new(HashMap::new()))
    }

    fn entries(&self) -> MutexGuard<'_, HashMap<NodeId, HashMap<ActorId, V>>> {
        lock(&self.0)
    }

    fn peers(&self) -> Vec<NodeId> {
        self.entries().keys().copied().collect()
    }

    fn targets(&self, peer: NodeId) -> Vec<ActorId> {
        self.entries()
            .get(&peer)
            .map(|targets| targets.keys().copied().collect())
            .unwrap_or_default()
    }

    fn take_target(&self, peer: NodeId, target: ActorId) -> Option<V> {
        let mut peers = self.entries();
        let targets = peers.get_mut(&peer)?;

        let value = targets.remove(&target);
        if targets.is_empty() {
            peers.remove(&peer);
        }
        value
    }

    fn take_peer(&self, peer: NodeId) -> Vec<(ActorId, V)> {
        self.entries()
            .remove(&peer)
            .map(|targets| targets.into_iter().collect())
            .unwrap_or_default()
    }

    fn prune(peers: &mut HashMap<NodeId, HashMap<ActorId, V>>, peer: NodeId, target: ActorId) {
        if let Some(targets) = peers.get_mut(&peer) {
            targets.remove(&target);
            if targets.is_empty() {
                peers.remove(&peer);
            }
        }
    }
}

/// The two states are distinct, since only a registered wire watch has a watcher.
enum WireWatch {
    Pending,
    Registered(ActorId),
}

fn fire_all(watchers: Vec<Watcher>, target: ActorId) {
    for watcher in watchers {
        fire(&watcher, target);
    }
}

fn fire(watcher: &Watcher, target: ActorId) {
    if let Err(error) = watcher.send_terminated(target) {
        debug!(
            watcher_id:% = watcher.watcher_id(),
            other_id:% = target,
            error:%;
            "cannot send terminated signal"
        );
    }
}

struct WireTerminatedSink {
    endpoint: &'static EndpointInner,
    peer: NodeId,
}

impl TerminatedSink for WireTerminatedSink {
    fn send_terminated(&self, actor_id: ActorId) -> Result<(), ActorTerminated> {
        let target = actor_id;
        self.endpoint.wire_watches().take(self.peer, target);
        self.endpoint
            .send(self.peer, Frame::Terminated { target })
            .map_err(|error| {
                debug!(peer:% = self.peer, actor_id:% = target, error:%; "cannot send terminated signal to node");
                ActorTerminated
            })
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        ActorId, MailboxCapacity,
        mailbox::{Watcher, make_mailbox},
        remote::{node::NodeId, watch::WatcherTable},
    };

    fn watcher(id: ActorId) -> Watcher {
        let (mailbox_handle, _mailbox) = make_mailbox::<()>(MailboxCapacity::Unbounded);
        Watcher::new(id, mailbox_handle.terminated_sink())
    }

    fn peer() -> NodeId {
        NodeId::new("127.0.0.1:1234".parse().expect("valid address"))
    }

    /// Only the first watcher of a target makes a `Watch` frame due, and only the last unwatch an
    /// `Unwatch` frame: these booleans are what keep the wire watch in sync with the local ones.
    #[test]
    fn only_the_first_and_last_watcher_move_the_wire_watch() {
        let table = WatcherTable::new();
        let peer = peer();
        let target = ActorId::new();
        let (first, second) = (ActorId::new(), ActorId::new());

        assert!(table.add(peer, target, watcher(first)));
        assert!(!table.add(peer, target, watcher(second)));

        assert!(!table.remove(peer, target, first));
        assert!(table.remove(peer, target, second));
    }

    /// Registering the same watcher twice registers it once, so its terminated signal is sent
    /// once, mirroring the local watcher registry.
    #[test]
    fn adding_a_watcher_twice_registers_once() {
        let table = WatcherTable::new();
        let peer = peer();
        let target = ActorId::new();
        let id = ActorId::new();

        assert!(table.add(peer, target, watcher(id)));
        assert!(!table.add(peer, target, watcher(id)));

        assert_eq!(table.take_target(peer, target).len(), 1);
    }

    /// Removing an unknown watcher, target or peer reports that nothing is due, so no stray
    /// `Unwatch` frame is sent.
    #[test]
    fn removing_unknown_entries_is_a_noop() {
        let table = WatcherTable::new();
        let peer = peer();
        let target = ActorId::new();

        assert!(!table.remove(peer, target, ActorId::new()));
        assert!(table.take_target(peer, target).is_empty());
        assert!(table.take_peer(peer).is_empty());

        assert!(table.add(peer, target, watcher(ActorId::new())));
        assert!(!table.remove(peer, ActorId::new(), ActorId::new()));
    }

    /// Taking a target or a peer takes its watchers and forgets it, so the peer is no longer
    /// heartbeated once nothing is watched on it.
    #[test]
    fn taking_watchers_forgets_the_peer() {
        let table = WatcherTable::new();
        let peer = peer();
        let (first, second) = (ActorId::new(), ActorId::new());

        assert!(table.add(peer, first, watcher(ActorId::new())));
        assert!(table.add(peer, second, watcher(ActorId::new())));
        assert_eq!(table.peers(), vec![peer]);
        assert_eq!(table.targets(peer).len(), 2);

        assert_eq!(table.take_target(peer, first).len(), 1);
        assert_eq!(table.take_peer(peer).len(), 1);
        assert!(table.peers().is_empty());
    }
}
