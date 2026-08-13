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
use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    mem,
    sync::{Arc, Mutex, MutexGuard},
};
use tracing::{debug, error};

type Watchers = HashMap<ActorId, Watcher>;

/// The local watchers of actors on other nodes.
pub(crate) struct WatcherTable(PeerKeyed<Watchers>);

impl WatcherTable {
    pub(crate) fn new() -> Self {
        Self(PeerKeyed::new())
    }

    /// `true` if this watcher is new for the peer and target, i.e. a `Watch` frame is due. The
    /// wire watch is per watcher, so every watcher registers, not just the first one of a target.
    pub(crate) fn add(&self, peer: NodeId, target: ActorId, watcher: Watcher) -> bool {
        match self
            .0
            .entries()
            .entry(peer)
            .or_default()
            .entry(target)
            .or_default()
            .entry(watcher.watcher_id())
        {
            Entry::Vacant(entry) => {
                entry.insert(watcher);
                true
            }

            Entry::Occupied(_) => false,
        }
    }

    /// `true` if the watcher was registered, i.e. an `Unwatch` frame is due.
    pub(crate) fn remove(&self, peer: NodeId, target: ActorId, watcher_id: ActorId) -> bool {
        let mut peers = self.0.entries();
        let Some(targets) = peers.get_mut(&peer) else {
            return false;
        };
        let Some(watchers) = targets.get_mut(&target) else {
            return false;
        };

        if watchers.remove(&watcher_id).is_none() {
            return false;
        }

        if watchers.is_empty() {
            PeerKeyed::prune(&mut peers, peer, target);
        }
        true
    }

    pub(crate) fn take_watcher(
        &self,
        peer: NodeId,
        target: ActorId,
        watcher_id: ActorId,
    ) -> Option<Watcher> {
        let mut peers = self.0.entries();
        let targets = peers.get_mut(&peer)?;
        let watchers = targets.get_mut(&target)?;

        let watcher = watchers.remove(&watcher_id);
        if watchers.is_empty() {
            PeerKeyed::prune(&mut peers, peer, target);
        }
        watcher
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

    pub(crate) fn watches(&self, peer: NodeId) -> Vec<(ActorId, ActorId)> {
        self.0
            .entries()
            .get(&peer)
            .map(|targets| {
                targets
                    .iter()
                    .flat_map(|(target, watchers)| {
                        watchers.keys().map(|watcher| (*target, *watcher))
                    })
                    .collect()
            })
            .unwrap_or_default()
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
                WireWatch::Registered {
                    wire_watcher_id, ..
                } => Some(RegisteredWatch {
                    target,
                    wire_watcher_id,
                }),

                WireWatch::Pending(_) => None,
            })
            .collect()
    }

    pub(crate) fn peers(&self) -> Vec<NodeId> {
        self.0.peers()
    }

    /// Adds the watcher and reports whether a registration is due, i.e. whether this is the first
    /// watcher of the target on that peer; a repeated `Watch` frame adds nothing and is hence
    /// idempotent.
    fn add(&self, peer: NodeId, target: ActorId, watcher: ActorId) -> bool {
        match self.0.entries().entry(peer).or_default().entry(target) {
            Entry::Occupied(mut entry) => {
                entry.get_mut().watchers().insert(watcher);
                false
            }

            Entry::Vacant(entry) => {
                entry.insert(WireWatch::Pending(HashSet::from([watcher])));
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
            let watchers = mem::take(entry.watchers());
            *entry = WireWatch::Registered {
                wire_watcher_id,
                watchers,
            };
        }
    }

    /// Removes one watcher, reporting the wire watcher to deregister once the last one is gone:
    /// the registration is shared by all watchers of a target on one peer.
    fn remove(&self, peer: NodeId, target: ActorId, watcher: ActorId) -> Option<ActorId> {
        let mut peers = self.0.entries();
        let targets = peers.get_mut(&peer)?;
        let entry = targets.get_mut(&target)?;

        entry.watchers().remove(&watcher);
        if !entry.watchers().is_empty() {
            return None;
        }

        let wire_watcher_id = match entry {
            WireWatch::Registered {
                wire_watcher_id, ..
            } => Some(*wire_watcher_id),
            WireWatch::Pending(_) => None,
        };
        PeerKeyed::prune(&mut peers, peer, target);
        wire_watcher_id
    }

    fn take(&self, peer: NodeId, target: ActorId) -> Option<WireWatch> {
        self.0.take_target(peer, target)
    }
}

/// The tombstone must be read again after registering, not only before: a node death in between
/// has already taken that node's watchers, so a watcher registered behind it would never be
/// signaled. Liveness is tracked only past that second read, else a dead incarnation is tracked
/// again and never untracked.
pub(crate) fn watch_remote(peer: NodeId, target: ActorId, watcher: Watcher) {
    let Some(endpoint) = endpoint::get() else {
        error!(
            watcher_id = %watcher.watcher_id(),
            other_id = %target,
            "cannot watch a remote actor, remoting endpoint not started"
        );
        return;
    };

    if endpoint.tombstoned(peer.incarnation()) {
        fire(&watcher, target);
        return;
    }

    let watcher_id = watcher.watcher_id();
    let watch_due = endpoint.watchers().add(peer, target, watcher);

    if endpoint.tombstoned(peer.incarnation()) {
        fire_all(endpoint.watchers().take_target(peer, target), target);
        return;
    }

    // No inbound delivery to gate here: watching only arms the failure detection.
    let _ = endpoint.track_liveness(peer);

    if watch_due
        && endpoint
            .send(
                peer,
                Frame::Watch {
                    target,
                    watcher: watcher_id,
                },
            )
            .is_err()
    {
        fire_all(endpoint.watchers().take_target(peer, target), target);
    }
}

pub(crate) fn unwatch_remote(peer: NodeId, target: ActorId, watcher_id: ActorId) {
    let Some(endpoint) = endpoint::get() else {
        return;
    };

    if endpoint.watchers().remove(peer, target, watcher_id)
        && let Err(error) = endpoint.send(
            peer,
            Frame::Unwatch {
                target,
                watcher: watcher_id,
            },
        )
    {
        debug!(%peer, actor_id = %target, %error, "cannot revert the wire watch");
    }
}

/// Called synchronously by the reader task, so the signal stays ordered behind the message
/// frames delivered before it.
pub(crate) fn on_terminated(
    endpoint: &EndpointInner,
    peer: NodeId,
    target: ActorId,
    watcher_id: ActorId,
) {
    if let Some(watcher) = endpoint.watchers().take_watcher(peer, target, watcher_id) {
        fire(&watcher, target);
    }
}

/// An unknown or already terminated target is answered with a `Terminated` frame right away,
/// mirroring the local race-free registration.
pub(crate) fn on_watch(
    endpoint: &'static EndpointInner,
    peer: NodeId,
    target: ActorId,
    watcher_id: ActorId,
) {
    if !endpoint.wire_watches().add(peer, target, watcher_id) {
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
            // An unanswered watcher is compensated by its node re-asserting the watch.
            let watchers = endpoint.wire_watches().take(peer, target);
            let _ = answer_terminated(endpoint, peer, target, watchers);
        }
    }
}

pub(crate) fn on_unwatch(
    endpoint: &EndpointInner,
    peer: NodeId,
    target: ActorId,
    watcher_id: ActorId,
) {
    if let Some(wire_watcher_id) = endpoint.wire_watches().remove(peer, target, watcher_id)
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

    fn take_target(&self, peer: NodeId, target: ActorId) -> Option<V> {
        Self::prune(&mut self.entries(), peer, target)
    }

    fn take_peer(&self, peer: NodeId) -> Vec<(ActorId, V)> {
        self.entries()
            .remove(&peer)
            .map(|targets| targets.into_iter().collect())
            .unwrap_or_default()
    }

    fn prune(
        peers: &mut HashMap<NodeId, HashMap<ActorId, V>>,
        peer: NodeId,
        target: ActorId,
    ) -> Option<V> {
        let targets = peers.get_mut(&peer)?;

        let value = targets.remove(&target);
        if targets.is_empty() {
            peers.remove(&peer);
        }
        value
    }
}

/// The two states are distinct, since only a registered wire watch has a watcher. Both carry the
/// remote watchers, which one registration serves: a terminated signal names the watcher it is
/// meant for, so it takes one frame per watcher.
enum WireWatch {
    Pending(HashSet<ActorId>),
    Registered {
        wire_watcher_id: ActorId,
        watchers: HashSet<ActorId>,
    },
}

impl WireWatch {
    fn watchers(&mut self) -> &mut HashSet<ActorId> {
        match self {
            WireWatch::Registered { watchers, .. } => watchers,
            WireWatch::Pending(watchers) => watchers,
        }
    }

    fn into_watchers(self) -> HashSet<ActorId> {
        match self {
            WireWatch::Registered { watchers, .. } => watchers,
            WireWatch::Pending(watchers) => watchers,
        }
    }
}

fn fire_all(watchers: Vec<Watcher>, target: ActorId) {
    for watcher in watchers {
        fire(&watcher, target);
    }
}

fn fire(watcher: &Watcher, target: ActorId) {
    if let Err(error) = watcher.send_terminated(target) {
        debug!(
            watcher_id = %watcher.watcher_id(),
            other_id = %target,
            %error,
            "cannot send terminated signal"
        );
    }
}

/// Fails if any watcher was left unanswered; one registration serves all watchers of a target
/// on one peer, but each needs its own frame to ride its own stream.
fn answer_terminated(
    endpoint: &EndpointInner,
    peer: NodeId,
    target: ActorId,
    watch: Option<WireWatch>,
) -> Result<(), ActorTerminated> {
    let mut answered = Ok(());

    for watcher in watch.map(WireWatch::into_watchers).unwrap_or_default() {
        if let Err(error) = endpoint.send(peer, Frame::Terminated { target, watcher }) {
            debug!(
                %peer,
                actor_id = %target,
                watcher_id = %watcher,
                %error,
                "cannot send terminated signal to node"
            );
            answered = Err(ActorTerminated);
        }
    }

    answered
}

struct WireTerminatedSink {
    endpoint: &'static EndpointInner,
    peer: NodeId,
}

impl TerminatedSink for WireTerminatedSink {
    fn send_terminated(&self, actor_id: ActorId) -> Result<(), ActorTerminated> {
        let target = actor_id;
        let watch = self.endpoint.wire_watches().take(self.peer, target);
        answer_terminated(self.endpoint, self.peer, target, watch)
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        ActorId, MailboxCapacity,
        mailbox::{Watcher, make_mailbox},
        remote::{
            node::NodeId,
            watch::{WatcherTable, WireWatchTable},
        },
    };

    fn watcher(id: ActorId) -> Watcher {
        let (mailbox_handle, _mailbox) = make_mailbox::<()>(MailboxCapacity::Unbounded);
        Watcher::new(id, mailbox_handle.terminated_sink())
    }

    fn peer() -> NodeId {
        NodeId::new("127.0.0.1:1234".parse().expect("valid address"))
    }

    /// A watcher arriving between `add` and `confirm` must survive the pending-to-registered
    /// transition, else it is silently lost and never signaled.
    #[test]
    fn confirm_keeps_watchers_added_while_pending() {
        let table = WireWatchTable::new();
        let peer = peer();
        let target = ActorId::new();
        let (first, second) = (ActorId::new(), ActorId::new());

        assert!(table.add(peer, target, first));
        assert!(!table.add(peer, target, second));

        let wire_watcher_id = ActorId::new();
        table.confirm(peer, target, wire_watcher_id);

        assert_eq!(table.remove(peer, target, first), None);
        assert_eq!(table.remove(peer, target, second), Some(wire_watcher_id));
    }

    /// Taking a registered wire watch hands back all its watchers, so each gets its own
    /// terminated frame.
    #[test]
    fn take_hands_back_all_watchers() {
        let table = WireWatchTable::new();
        let peer = peer();
        let target = ActorId::new();
        let (first, second) = (ActorId::new(), ActorId::new());

        table.add(peer, target, first);
        table.add(peer, target, second);
        table.confirm(peer, target, ActorId::new());

        let watchers = table
            .take(peer, target)
            .expect("the wire watch is registered")
            .into_watchers();
        assert_eq!(watchers.len(), 2);
        assert!(watchers.contains(&first));
        assert!(watchers.contains(&second));

        assert!(table.take(peer, target).is_none());
    }

    /// Every watcher of a target makes a `Watch` frame due and every unwatch an `Unwatch` frame:
    /// the wire watch is per watcher, since a terminated signal must name the watcher to ride its
    /// stream.
    #[test]
    fn every_watcher_moves_the_wire_watch() {
        let table = WatcherTable::new();
        let peer = peer();
        let target = ActorId::new();
        let (first, second) = (ActorId::new(), ActorId::new());

        assert!(table.add(peer, target, watcher(first)));
        assert!(table.add(peer, target, watcher(second)));

        assert!(table.remove(peer, target, first));
        assert!(table.remove(peer, target, second));
        assert!(!table.remove(peer, target, second));
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
        assert_eq!(table.watches(peer).len(), 2);

        assert_eq!(table.take_target(peer, first).len(), 1);
        assert_eq!(table.take_peer(peer).len(), 1);
        assert!(table.peers().is_empty());
    }

    /// A terminated signal takes only the watcher it names, so the other watchers of the same
    /// target keep waiting for theirs.
    #[test]
    fn taking_one_watcher_keeps_the_others() {
        let table = WatcherTable::new();
        let peer = peer();
        let target = ActorId::new();
        let (first, second) = (ActorId::new(), ActorId::new());

        table.add(peer, target, watcher(first));
        table.add(peer, target, watcher(second));

        assert!(table.take_watcher(peer, target, first).is_some());
        assert!(table.take_watcher(peer, target, first).is_none());
        assert_eq!(table.watches(peer), vec![(target, second)]);

        assert!(table.take_watcher(peer, target, second).is_some());
        assert!(table.peers().is_empty());
    }
}
