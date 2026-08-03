# Remoting

This document explains how waltz remoting (the `remote` feature) works and, most importantly,
which of the core guarantees from [actors.md](actors.md) carry over the network, which weaken,
and why. The implementation lives in [`waltz/src/remote`](../waltz/src/remote).

## Overview

Remoting makes actors on different nodes message each other through the same API as local ones:

- `ActorRef` is location transparent: it implements `Serialize` and `Deserialize`, both failing
  with `RefError::EndpointNotStarted` until the endpoint is started, so message types embed
  reference fields, e.g. `reply_to: ActorRef<Reply>`, and work unchanged no matter where their
  counterpart lives.
- Each process runs at most one remoting endpoint, started via `remote::start` with a
  `Transport` (the provided one is QUIC via quinn, TLS included) and an `EndpointConfig`. The
  separate `remote-dev` feature adds `QuicTransport::dev`, which skips certificate
  verification and hence exists only where it is asked for.
- Bootstrap is out of band: `serialize_ref` and `deserialize_ref` exchange the first reference,
  e.g. via configuration; every further reference travels inside messages.
- Death watch works across nodes through the ordinary `ActorContext::watch`, with the weakened
  contract below.

A node's identity is its advertised address plus an *incarnation*, a UUID minted per process
start, so a restarted node is distinguishable from its predecessor.

## The wire model

All frames from one node towards another ride one FIFO lane: an outbound queue drained by one
connection. Sending enqueues synchronously at `tell` time; the receiving endpoint injects into
local mailboxes in arrival order. The queue is unbounded underneath with a reservation counter
in front, exactly like a local mailbox: ordinary messages are subject to the capacity,
system frames (watch registration, terminated signals, heartbeats) bypass it but ride the same
queue, since a terminated signal must never be dropped and must never overtake.

A lost connection is reconnected with exponential backoff; frames queued while the link is down
are delivered after the reconnect, in order. There is no replay of frames already handed to a
dead connection: delivery stays at-most-once, per-sender FIFO becomes "in order, with gaps". A
node no watch names in either direction is given up after `max_connect_attempts` failed attempts,
turning its queued frames into dead letters; a node a watch names is retried until the failure
detector declares it dead. Giving up is not final: a later message dials again. A
connection's reader is aborted before the next one is dialed, so two readers for one peer can
never interleave a frame buffered on the dead connection behind a frame from the new one.

A lane belongs to an address but serves one incarnation: the handshake names the peer it is
connected to, and from then on a frame addressed to any other incarnation at that address is a
local dead letter rather than traffic written onto its successor's connection. A reference which
outlived the node it names hence fails fast, close to the sender, instead of being dropped as an
unknown target on the far side.

## Guarantees

- **The tell contract extends verbatim.** Fire-and-forget, at-most-once; an unreachable node, a
  full outbound queue and an undecodable payload all become logged dead letters.
- **Per-sender FIFO holds** for messages from one sender to one target, "with gaps" across
  reconnects as above.
- **Remote death watch has two tiers**, indistinguishable in the API:
  - A *real termination*, delivered over the wire, keeps the full local contract: the
    terminated signal arrives behind all messages the terminated actor delivered to the
    watcher, and it proves the actor's destructors have run. This is because the wire watcher
    fires inside the target's local termination sequence and its `Terminated` frame rides the
    same FIFO lane as the messages the actor sent before.
  - A *synthesized signal*, flushed when the watched actor's node is declared dead, proves
    none of that: the actor may be alive across a network partition and its destructors may
    never run. It guarantees exactly one thing, made true by construction rather than
    observation: **after the signal, no message from that actor is ever delivered through this
    endpoint again.** The node death sequence tombstones the incarnation and stops all
    delivery from it before the signals are flushed, and a later handshake from a tombstoned
    incarnation is refused. Distinguishing a crashed node from an unreachable one is
    impossible in an asynchronous network; this weakening is fundamental, not an
    implementation gap.
- **Unwatch stays absolute.** It is enforced on the watcher's side in the run loop, which does
  not know or care whether a signal came from the local or a remote actor.
- **Watching is race-free.** A `Watch` for an already terminated (or never bound) actor is
  answered with an immediate terminated signal by the watched node, mirroring the local
  atomic registration close.

## Failure detection and node death

Peers a watch names in *either* direction are heartbeated: those this node watches actors on, and
those watching actors here. `Ping` frames ride the lane at the configured interval and every
inbound frame counts as a heartbeat; no reply frame is needed, because the heartbeated set is
symmetric by construction: a watch puts the watched node into the watcher's watcher table and the
watcher into the watched node's wire watch table, so each side's own pings keep the other
alive. A pluggable
`FailureDetector` (deadline based by default) declares a node dead once heartbeats stop. Node
death also triggers when a handshake reveals a new incarnation at a known address, which proves
the old process is gone.

Covering the inbound direction is what keeps the watched side from leaking: a peer which only
watches actors here would otherwise never be heartbeated, and the watchers it registered in those
actors' registries would outlive it indefinitely. Peers no watch names in either direction and no
connection reads from are untracked again, so the tracking does not grow with every peer ever
connected.

A silent peer's two directions are not treated alike, because the tombstone is permanent and the
two owe different things:

- If an actor here watches actors on it, it dies: its incarnation is tombstoned and the
  synthesized signals go out. Severing the incarnation for good is the price of making those
  signals true, and it is why a partitioned node coming back cannot break the contract.
- If nothing here watches it, there is no signal to make true, so nothing is owed and nothing is
  severed. The peer is merely untracked: the wire watches it held on local actors are dropped and
  it stops being tracked, but its lane stays open and it is free to reconnect, re-sending its
  watches as it does. Tombstoning it instead would buy nothing and would make a peer permanently
  unreachable for having been briefly quiet.

The watch protocol is self-healing without any redelivery machinery: `Watch` frames are re-sent
after every reconnect (registration is idempotent on the watched node), so a `Watch` or
`Terminated` frame lost with a broken connection is compensated on the next connection, where a
re-sent `Watch` for a meanwhile terminated actor is answered with `Terminated` right away. The
failure detector covers the remaining case of a node that never comes back.

## Limitations

- One remoting endpoint per process; node identity is address based.
- One lane per peer node, so a large message delays unrelated frames behind it (per-target QUIC
  streams are a planned extension of the transport). A pair of nodes telling each other messages
  hence uses one connection per direction, each dialed on first use.
- No discovery: the first reference is exchanged out of band.
- Tombstoned incarnations are remembered for the life of the process. Refusing a handshake from
  an incarnation already declared dead is what keeps a synthesized signal true, and nothing can
  prove that such a handshake will never arrive, so the set only grows: one entry per peer
  incarnation this node outlives.
- An address which answered without speaking this protocol is not dialed again, so a
  misconfiguration costs one round of attempts rather than one per message. A waltz node coming
  up there recovers by dialing this node, whose successful handshake lifts the refusal; a waltz
  node which never dials back stays unreachable from here.
