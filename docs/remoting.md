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
- Bootstrap goes through discovery: `remote::register` names a local actor and `remote::lookup`
  resolves that name at a known address. `serialize_ref` and `deserialize_ref` remain for
  exchanging a reference out of band, e.g. through configuration; the bytes name the message
  type, so a wrong-typed resolution is refused like a mistyped lookup. Every further reference
  travels inside messages.
- Death watch works across nodes through the ordinary `ActorContext::watch`, with the weakened
  contract below.
- Request-response works across nodes too: `ReplyTo` is serializable like a reference, so
  `ActorRef::ask` and `ActorContext::reply_to` work unchanged against remote actors, with the
  `NoReply` detection weakened as described below.

A node's identity is its advertised address plus an *incarnation*, a UUID minted per process
start, so a restarted node is distinguishable from its predecessor.

## The wire model

All frames from one node towards another ride one lane: a set of outbound queues drained by one
connection. Sending enqueues synchronously at `tell` time; the receiving endpoint injects into
local mailboxes in arrival order. The queues are unbounded underneath with a reservation counter
in front, exactly like a local mailbox, and one counter serves the whole lane, so
`outbound_capacity` means the same thing however many queues share it: ordinary messages and
replies are subject to the capacity, system frames (watch registration, terminated signals,
reply-dropped notifications, heartbeats) bypass it but ride the same queues, since a terminated
signal must never be dropped and must never overtake.

A lane is not one queue but a *control* queue plus a bounded pool of *data* queues, one per
stream of the connection, `max_streams_per_peer` of them at most. A frame delivered to an actor
picks its stream by hashing that actor's ID; every other frame rides the control stream. So FIFO
holds per recipient rather than per node, and a large message only delays frames towards
recipients hashing onto the same stream. The mapping never travels the wire: the receiving side
dispatches by the target named in the frame, so only the sender has to agree with itself.

That the terminated signal names its *watcher* is what carries the ordering guarantee across this
split. A `Terminated` frame hashes onto the watcher's stream, the same one the messages the dying
actor sent that watcher ride, so the shared queue orders them exactly as one lane used to. It
costs one frame per watcher rather than one per node. A `Reply` frame names the *asker* for the
same reason: it rides the asker's stream, behind whatever the responder told that actor before
replying.

How many data streams a lane gets is a property of the transport, not an assumption: `Transport`
reports it, QUIC offers as many as configured, and a transport without streams reports zero. At
zero every frame rides the control stream, which is one ordered lane per peer carrying everything.
The guarantees hold there by the same argument rather than as a special case, which is what keeps
the abstraction implementable over a stream-less transport such as TCP.

All data streams are opened when the connection is established, not on first use. A peer
admitting fewer concurrent streams than this node opens then fails the connection at setup, into
the ordinary reconnect path; opened on demand it would instead stall whichever queue happened to
hash onto a stream that was never granted, silently and forever, since these streams live as long
as the connection.

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
unknown target on the far side. Frames already sitting in the lane's queues when a reconnect's
handshake reveals the successor are the exception: they drain onto the new connection and die on
the far side as unknown targets, at-most-once either way.

## Guarantees

- **The tell contract extends verbatim.** Fire-and-forget, at-most-once; an unreachable node, a
  full outbound queue, a message encoding beyond `max_frame_size` and an undecodable payload all
  become logged dead letters. Undecodable includes a payload whose embedded reference names an
  actor of the receiving node which has already terminated: the whole message is the dead letter
  then, unlike a local tell to a terminated actor, which costs only itself.
- **Per-sender FIFO holds** for messages from one sender to one target, "with gaps" across
  reconnects as above.
- **Remote death watch has two tiers**, indistinguishable in the API:
  - A *real termination*, delivered over the wire, keeps the full local contract: the
    terminated signal arrives behind all messages the terminated actor delivered to the
    watcher, and it proves the actor's destructors have run. This is because the wire watcher
    fires inside the target's local termination sequence and its `Terminated` frame rides the
    same queue as the messages the actor sent that watcher before, the one their shared
    recipient hashes onto.
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
- **Request-response crosses nodes through the same API.** Serializing a `ReplyTo` moves the
  reply destination into a nonce-keyed pending table on its origin node; the receiving node gets
  a proxy whose reply rides a `Reply` frame back to the origin, and dropping the proxy without a
  reply sends a `ReplyDropped` frame instead. An ask still resolves exactly once, at latest at
  its timeout, and a `reply_to` reply stays FIFO with the responder's other messages to the
  asker, since the `Reply` frame rides the asker's stream. The `NoReply` detection weakens to
  best-effort:
  - `ReplyDropped` is fire-and-forget: one lost with its connection resolves the ask by its
    timeout, since a reply is not idempotent and nothing reply-related is replayed on a
    reconnect.
  - Node death fails every pending ask towards the dead node as `NoReply`, after the tombstone
    and quiesce, so such a `NoReply` is never followed by its reply. Node death is only declared
    for a peer involved in a watch or replaced by a new incarnation at its address; an ask
    towards a silently vanished node nothing watches is failed by the lane instead, below.
  - Giving up a lane fails the asks stamped with its peers as `NoReply` the same way, once the
    loss is noticed and the connect attempts are exhausted. Only the outbound direction is
    judged, so the eviction is what makes the `NoReply` true: a reply a live but unreachable
    peer sends over a later connection dies against the evicted entry, never behind the
    `NoReply`. The timeout remains the backstop, e.g. for a loss the transport is slow to
    report.
  - A message frame names its payload's reply destinations next to the payload, so a request
    the receiving node dead-letters undecoded, e.g. towards a meanwhile terminated actor, is
    answered with `ReplyDropped` for each of them: such an ask resolves as `NoReply` rather
    than by its timeout.
  - A `ReplyTo` may be forwarded to a third node and each hop chains its reply through the
    previous one; the node-death eviction covers each hop's next node only, the timeout covers
    the rest.

## Discovery

The first reference cannot travel inside a message, so it is resolved by name instead:
`remote::register(&Key::new("worker-pool"), actor_ref)` names an actor of this node, and
`remote::lookup(&Key::new("worker-pool"), addr).await` resolves that name at the node advertising
`addr`. A lookup dials through the ordinary lane machinery, so it inherits the reconnect backoff
and needs no separate connection; it is answered over the answering node's own lane back.

A `Key<M>` carries the message type next to the name, and the type travels as the name its
compiler spells it, so a key naming the wrong type is refused as `TypeMismatch` rather than
resolved into a reference which drops every message told to it. That comparison assumes both nodes
are built from the same source, which is the assumption the wire format already makes.

The properties worth knowing:

- **It is a point query, not a directory.** A lookup names one address; nothing is gossiped, and a
  node knows only what it registered itself. Resolution composes with whatever names addresses
  already, DNS or an orchestrator, rather than duplicating it.
- **`NotFound` is an ordinary bootstrap answer**, since a node answers lookups from the moment its
  endpoint starts, which is before it has registered anything. There is no internal timeout:
  callers wrap a lookup in `tokio::time::timeout` and retry `NotFound`. A lookup issued before the
  node is up is not an error either; it rides the lane which keeps dialing, though only for the
  connect attempts an unwatched address is granted (eight by default; how long each takes is the
  transport's business, e.g. QUIC abandons a silent address only after its 30 s handshake
  timeout): a lookup outliving them fails as unreachable and is retried by the caller exactly
  like `NotFound`.
- **More than one actor may hold one key**, and a point lookup answers with one of them. The
  registry is hence already shaped like the receptionist a membership layer would grow from.
- **A registration lives as long as the actor.** Naming an actor binds it exactly as serializing a
  reference to it does, and the same eviction drops both once it terminates.
- **The answer names an incarnation**, since the reply carries the responder's node identity, so a
  resolved reference is the same kind of reference as one which arrived inside a message and stops
  working when that node is replaced.

Nothing here assumes a name lives on exactly one node, and node identity stays out of `Key`, so a
cluster-wide lookup or a listing subscription is an addition rather than a change.

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
after every reconnect and re-asserted every `watch_refresh_interval` (registration is idempotent
on the watched node), so a `Watch` or `Terminated` frame lost with a broken connection is
compensated on the next connection or the next refresh, where a re-sent `Watch` for a meanwhile
terminated actor is answered with `Terminated` right away. The refresh covers the loss a
reconnect cannot see: a `Terminated` frame lost with the *watched* side's connection while the
watcher's own lane never broke. The healed answer still reflects a real termination, delayed by
at most the refresh interval, which stays below the failure detection deadline, so a quiet pair
heals by answer rather than by a false node death. The failure detector covers the remaining
case of a node that never comes back.

## Trust

TLS is mandatory for QUIC, but server-only TLS authenticates just the dialed side: any client
able to reach the port can complete a handshake, name any advertised address as its own and
thereby have the healthy node there tombstoned as dead. The production posture is hence mutual
TLS via `QuicTransport::mutual_tls`: every node presents one certificate as both its server and
its client identity, verified against the cluster's certificate authority, so a stranger cannot
complete a connection and reaches neither the protocol nor the failure detection. Issuing and
renewing the certificates is established automation, e.g. cert-manager or SPIFFE/SPIRE on
Kubernetes.

Two boundaries are documented rather than closed: a certificate does not bind its holder to an
advertised address, so an authenticated node can still claim another member's address (binding
identity to address, e.g. via certificate names, would need the transport to expose peer
identity); and certificates are read once at startup, so a renewal takes a process restart until
hot rotation via quinn's server config swap is added.

## Limitations

- One remoting endpoint per process; node identity is address based.
- One connection per direction between a pair of nodes, each dialed on first use. Head-of-line
  blocking is bounded rather than gone: recipients hashing onto the same stream still delay each
  other, and a pool of `max_streams_per_peer` streams cannot separate an unbounded number of
  recipients.
- Discovery is a point query at a known address: there is no membership, no gossip and no
  cluster-wide lookup, so an address has to come from configuration or an orchestrator.
- Tombstoned incarnations are remembered for the life of the process. Refusing a handshake from
  an incarnation already declared dead is what keeps a synthesized signal true, and nothing can
  prove that such a handshake will never arrive, so the set only grows: one entry per peer
  incarnation this node outlives.
- An address which answered without speaking this protocol is not dialed again, so a
  misconfiguration costs one round of attempts rather than one per message. A waltz node coming
  up there recovers by dialing this node, whose successful handshake lifts the refusal; a waltz
  node which never dials back stays unreachable from here.
- A vanished node is noticed by a watch-involved peer within the failure detection deadline. A
  peer that merely messages it learns of the loss only once the transport kills the idle
  connection (30 s under QUIC's defaults): until then sends vanish into the dead connection as
  accepted but undeliverable frames, within the at-most-once contract.
