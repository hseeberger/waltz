use crate::{
    remote::{endpoint::FailureDetectorFactory, node::Incarnation},
    sync::{lock, read, write},
};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, RwLock, RwLockReadGuard},
    time::{Duration, Instant},
};

/// Decides whether a peer node is still available, fed by heartbeats: every frame received from
/// the node counts as one. One detector instance exists per peer node a watch names, in either
/// direction; the default is [DeadlineFailureDetector].
pub trait FailureDetector
where
    Self: Send + 'static,
{
    /// Record a heartbeat observed at the given instant.
    fn record_heartbeat(&mut self, at: Instant);

    /// Whether the node is to be considered available at the given instant.
    fn available(&self, at: Instant) -> bool;
}

/// The default [FailureDetector]: a node is available as long as the last heartbeat is no older
/// than the given deadline.
#[derive(Debug)]
pub struct DeadlineFailureDetector {
    deadline: Duration,
    last_heartbeat: Option<Instant>,
}

impl DeadlineFailureDetector {
    /// A detector with the given deadline.
    pub fn new(deadline: Duration) -> Self {
        Self {
            deadline,
            last_heartbeat: None,
        }
    }
}

impl FailureDetector for DeadlineFailureDetector {
    fn record_heartbeat(&mut self, at: Instant) {
        self.last_heartbeat = Some(at);
    }

    fn available(&self, at: Instant) -> bool {
        match self.last_heartbeat {
            Some(last_heartbeat) => at.duration_since(last_heartbeat) <= self.deadline,
            None => true,
        }
    }
}

pub(crate) struct Liveness(Mutex<HashMap<Incarnation, PeerLiveness>>);

impl Liveness {
    pub(crate) fn new() -> Self {
        Self(Mutex::new(HashMap::new()))
    }

    /// Idempotent; records an initial heartbeat, so the deadline counts from now. The only way to
    /// obtain a gate: inbound delivery must not be able to run ungated, hence tracking and gating
    /// cannot be separate steps.
    pub(crate) fn track(
        &self,
        incarnation: Incarnation,
        factory: &FailureDetectorFactory,
    ) -> DeliveryGate {
        lock(&self.0)
            .entry(incarnation)
            .or_insert_with(|| {
                let mut detector = factory();
                detector.record_heartbeat(Instant::now());
                PeerLiveness {
                    detector,
                    gate: DeliveryGate::new(),
                }
            })
            .gate
            .clone()
    }

    pub(crate) fn record_heartbeat(&self, incarnation: Incarnation) {
        if let Some(liveness) = lock(&self.0).get_mut(&incarnation) {
            liveness.detector.record_heartbeat(Instant::now());
        }
    }

    pub(crate) fn available(&self, incarnation: Incarnation) -> bool {
        lock(&self.0)
            .get(&incarnation)
            .is_none_or(|liveness| liveness.detector.available(Instant::now()))
    }

    /// Used by the node death sequence between tombstoning and flushing, so no delivery straddles
    /// the flush. Takes the gate only after releasing this map's lock, which a delivery in flight
    /// may take, hence holding it here would deadlock against the very delivery being waited
    /// out.
    pub(crate) fn quiesce(&self, incarnation: Incarnation) {
        let gate = lock(&self.0)
            .get(&incarnation)
            .map(|liveness| liveness.gate.clone());

        if let Some(gate) = gate {
            gate.quiesce();
        }
    }

    pub(crate) fn untrack(&self, incarnation: Incarnation) {
        lock(&self.0).remove(&incarnation);
    }

    /// The kept set must be computed by the closure, under this map's lock: a concurrent
    /// [Liveness::track] blocks on the lock, so its preceding table entry is either seen by the
    /// closure or its insert re-creates what was just removed; a set computed outside would
    /// delete a freshly tracked peer for good. Beyond the kept set, an incarnation an inbound
    /// reader still holds the gate of is kept: a reader clones the gate under this map's lock, so
    /// one still being read from is never observed with the map as its only holder.
    pub(crate) fn retain_with<F>(&self, keep: F)
    where
        F: FnOnce() -> HashSet<Incarnation>,
    {
        let mut entries = lock(&self.0);
        let keep = keep();
        entries
            .retain(|incarnation, liveness| keep.contains(incarnation) || liveness.gate.in_use());
    }
}

/// Held by every inbound delivery, which is what [Liveness::quiesce] waits out; cloned to one
/// reader per stream of a connection, all of which it must gate. The whole gate protocol lives
/// here: creation, entering, quiescing and the in-use probe.
#[must_use = "an inbound delivery must hold the gate for its whole duration"]
#[derive(Clone)]
pub(crate) struct DeliveryGate(Arc<RwLock<()>>);

impl DeliveryGate {
    pub(crate) fn enter(&self) -> RwLockReadGuard<'_, ()> {
        read(&self.0)
    }

    fn new() -> Self {
        Self(Arc::new(RwLock::new(())))
    }

    /// Returns only once no delivery holds the gate anymore.
    fn quiesce(&self) {
        drop(write(&self.0));
    }

    /// Whether anything beyond the [Liveness] table itself still holds this gate.
    fn in_use(&self) -> bool {
        Arc::strong_count(&self.0) > 1
    }
}

struct PeerLiveness {
    detector: Box<dyn FailureDetector>,
    gate: DeliveryGate,
}

#[cfg(test)]
mod tests {
    use crate::remote::{
        endpoint::FailureDetectorFactory,
        failure::{DeadlineFailureDetector, FailureDetector, Liveness},
        node::{Incarnation, NodeId},
    };
    use std::{
        collections::HashSet,
        sync::{Arc, mpsc},
        thread,
        time::{Duration, Instant},
    };

    const DEADLINE: Duration = Duration::from_secs(5);
    const TICK: Duration = Duration::from_millis(1);
    const HOLD: Duration = Duration::from_millis(50);
    const TIMEOUT: Duration = Duration::from_secs(5);

    fn factory() -> FailureDetectorFactory {
        Arc::new(|| Box::new(DeadlineFailureDetector::new(DEADLINE)))
    }

    fn incarnation() -> Incarnation {
        NodeId::new("127.0.0.1:1234".parse().expect("valid address")).incarnation()
    }

    /// A node which has not been heard from yet is available, so a peer is not declared dead
    /// before it ever had a chance to send a heartbeat.
    #[test]
    fn a_node_without_heartbeats_is_available() {
        let detector = DeadlineFailureDetector::new(DEADLINE);

        assert!(detector.available(Instant::now() + DEADLINE * 2));
    }

    /// The deadline is inclusive: a node is available up to and including it, and unavailable
    /// from the first tick beyond.
    #[test]
    fn the_deadline_is_inclusive() {
        let now = Instant::now();
        let mut detector = DeadlineFailureDetector::new(DEADLINE);
        detector.record_heartbeat(now);

        assert!(detector.available(now));
        assert!(detector.available(now + DEADLINE));
        assert!(!detector.available(now + DEADLINE + TICK));
    }

    /// A heartbeat restarts the deadline, which is what keeps a live but quiet node alive.
    #[test]
    fn a_heartbeat_restarts_the_deadline() {
        let now = Instant::now();
        let mut detector = DeadlineFailureDetector::new(DEADLINE);
        detector.record_heartbeat(now);
        assert!(!detector.available(now + DEADLINE + TICK));

        detector.record_heartbeat(now + DEADLINE);

        assert!(detector.available(now + DEADLINE + TICK));
        assert!(!detector.available(now + DEADLINE * 2 + TICK));
    }

    /// An incarnation whose gate an inbound reader still holds survives retention even when the
    /// keep set omits it, and an incarnation in the keep set survives without any held gate; only
    /// an unheld, unkept one is dropped.
    #[test]
    fn retention_keeps_held_gates_and_the_keep_set() {
        let liveness = Liveness::new();
        let incarnation = incarnation();

        let gate = liveness.track(incarnation, &factory());
        let entry = Arc::downgrade(&gate.0);

        liveness.retain_with(HashSet::new);
        assert!(
            entry.upgrade().is_some(),
            "a held gate must survive retention"
        );

        drop(gate);
        liveness.retain_with(|| HashSet::from([incarnation]));
        assert!(
            entry.upgrade().is_some(),
            "a kept incarnation must survive retention"
        );

        liveness.retain_with(HashSet::new);
        assert!(
            entry.upgrade().is_none(),
            "an unheld, unkept incarnation must be dropped"
        );
    }

    /// Quiescing waits out every delivery holding the gate and returns once the last guard is
    /// dropped.
    #[test]
    fn quiesce_waits_out_deliveries() {
        let liveness = Liveness::new();
        let incarnation = incarnation();
        let gate = liveness.track(incarnation, &factory());

        let guard = gate.enter();
        thread::scope(|scope| {
            let (quiesced_tx, quiesced_rx) = mpsc::channel();
            let liveness = &liveness;
            scope.spawn(move || {
                liveness.quiesce(incarnation);
                let _ = quiesced_tx.send(());
            });

            assert!(
                quiesced_rx.recv_timeout(HOLD).is_err(),
                "quiesce returned while a delivery held the gate"
            );

            drop(guard);
            quiesced_rx
                .recv_timeout(TIMEOUT)
                .expect("quiesce did not return after the gate was released");
        });
    }
}
