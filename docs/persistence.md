# Persistence

This document defines the guarantees of waltz persistence: event sourcing with optional
snapshots, behind the `persistence` feature. The implementation lives in
[`waltz/src/persistence`](../waltz/src/persistence) and the integration tests in
[`persistence.rs`](../waltz/tests/persistence.rs) encode these guarantees; everything builds on
the mechanics and guarantees of [actors.md](actors.md).

## Overview

An event-sourced actor persists what happened, not what the state is. Commands arrive as ordinary
messages; [`EventSourced::handle`](../waltz/src/persistence/event_sourced.rs) validates a command
against the current state and decides which events it causes; the events are appended to an event
store and only then applied via `apply`, the one and only state transition. The state is
therefore a fold of the persisted events over a seed and is itself never stored: after a crash, a
restart or a redeployment it is recovered by replaying the events. Snapshots are an optional
shortcut for that replay, a discardable derivative of the events, never a source of truth.

The stores are pluggable behind two traits, [`EventStore` and
`SnapshotStore`](../waltz/src/persistence/store.rs), which operate on encoded bytes;
implementations live in separate backend crates. Everything below holds for any conforming store.

## Defining an event-sourced actor

The [`EventSourced`](../waltz/src/persistence/event_sourced.rs) trait mirrors the shape of
[`Actor`](../waltz/src/actor.rs), split along the event-sourcing seams:

- `Command` is the type of the received messages; `Event` and `Snapshot` are the persisted types,
  both [`Versioned`](../waltz/src/persistence/versioned.rs) for schema evolution; actors without
  snapshots use the uninhabited `Nothing`.
- `persistence_id` names the actor's event stream.
- `init` is the pure seed of the fold and `init_from_snapshot` its snapshot-based alternative;
  `recovered` runs once after recovery and is the place to touch the world.
- `handle` takes the state by reference and returns an
  [`Effect`](../waltz/src/persistence/effect.rs): `persist`/`persist_all` name the caused events,
  `stop`/`and_stop` stop the actor, and `then` attaches a continuation running once the events
  are durable and applied, the only safe place for outward-facing actions.
- `apply` folds one event into the state and `snapshot` optionally offers a snapshot after an
  effect's events have been applied.

Spawning uses dedicated entry points carrying the
[`Persistence`](../waltz/src/persistence.rs) wiring, an event store plus optional snapshot store
and codec: `ActorSystem::event_sourced` for the root and `ActorContext::spawn_event_sourced` for
children, each with a `_with_config` variant taking the ordinary `ActorConfig`. Unlike a plain
actor value, which only needs `Send`, an event-sourced one must also be `Sync`: it is borrowed
across the store awaits during recovery and settlement. Everything else, mailboxes, supervision,
death watch and termination, works exactly as for plain actors.

## Identity

An event stream is identified by a `PersistenceId`, an application-chosen pair of `entity_type`
and `entity_id` which is stable across incarnations: every spawn of "order 42" names the same
stream, yesterday, today and on another node. It is unrelated to `ActorId`, which is fresh per
spawn and distinguishes incarnations; over time many `ActorId`s serve one `PersistenceId`.

## Recovery

On every spawn and every restart, before the first command:

1. The latest snapshot, if any, is loaded; `init_from_snapshot` turns it into the state covering
   every event before the snapshot's sequence number. Without a snapshot, `init` produces the
   seed state.
2. The events from that sequence number on are read in order and folded via `apply`.
3. `recovered` runs exactly once on the resulting state. This is the place to touch the world:
   spawn children, re-arm timers, register with other actors.

`init` is skipped entirely when a snapshot exists, so it must be nothing but the pure seed of the
fold; world-touching work in `init` would silently disappear the moment snapshots are enabled.
`recovered` always runs, and never during replay.

## Replay equals live execution

`apply` is pure and total: no I/O, no failure, no dependence on the clock, on randomness or on
per-incarnation values such as `ActorRef`s. Replay runs exactly the `apply` calls the live actor
ran, on the same events in the same order, and therefore reconstructs the same state. Effects
never run during replay; recovery is observable only through `recovered`.

## Effects and durability

`handle` returns an `Effect`: which events to persist, whether to stop, and what to do afterwards.
Events are durable, as defined by the store, before they are applied. A continuation attached via
`then` runs after its events are durable and applied, and never on replay: it is the only safe
place for outward-facing actions such as replying to an ask or telling another actor. On an
effect without events, `then` runs right after `handle`.

All events of one `Effect` are appended atomically: after a crash the stream contains all of them
or none, never a prefix.

Durability of events is guaranteed; delivery of effects is not. A crash between the append and
`then` loses the continuation, and it is not re-run on recovery, so outward-facing actions are
at-most-once, exactly like `tell`.

## Ordering

Writes are strict: a command is fully settled, meaning its events are durable and applied and its
`then` has run, before the next incoming is taken from the mailbox. The mailbox guarantees of
[actors.md](actors.md) carry over unchanged: messages from one sender arrive in send order, and a
terminated signal arrives behind all messages the terminated actor delivered. A parent's stop is
honored between commands, never in the middle of one settling, and the termination sequence is
the same as for any actor.

## Failure and supervision

A failure of `handle`, a panic, or a failed append is an actor failure and consults the
`SupervisionStrategy`, exactly like a failure of `receive` in a plain actor. `Restart` recovers
by replaying from the store, which is precisely the reconciliation an ambiguous append needs: if
the append reached the store before the failure, replay picks its events up; if not, they were
never applied either, since `apply` runs only after the append. There is no retry logic besides
supervision.

Failures during recovery divide in two: a failure of `init`, `init_from_snapshot`, event
decoding or `apply` is deterministic, a restart fails identically, and so it escalates through
the restart streak to a stop; an undecodable snapshot is the one exception, discarded in favor of
full replay (see schema evolution below). A failure of `recovered`, or of a store itself while
loading the snapshot or reading events, is an ordinary startup failure, e.g. a struggling
dependency, which a restart with backoff may well fix. A failed snapshot load is deliberately not
traded for full replay: only an undecodable snapshot is discarded; a snapshot store failure
surfaces through supervision instead of silently turning every recovery into a full replay.

A failure to save a snapshot is logged and never fails the actor: losing a snapshot only makes
the next recovery replay more events.

## Fencing

At most one incarnation can extend a stream. Every append is conditional on the expected next
sequence number, and the store rejects an append whose expectation is stale. When two
incarnations of one `PersistenceId` are alive at once, whether through a network partition under
remoting or through plain misconfiguration, one of them loses every append race: its failure goes
through supervision, and a restarted loser replays the winner's events instead of overwriting
them. The stream never loses events and never interleaves two writers; the store is the arbiter,
not the nodes.

## Sequence numbers

Sequence numbers are per `PersistenceId`, gapless, and start at 0, carried by the `SeqNo` type.
A `SeqNo` names either a stored event's position or the position at which the next event is
appended, which is `SeqNo::ZERO` for an empty stream. One type covers both because every use is a
position replay resumes at, never one it resumes after: a snapshot records the sequence number at
which replay resumes, the fencing expectation is the position the next append claims, and a read
starts at the given position inclusively. Nothing in the persistence path adds or subtracts one.

## Schema evolution

Events outlive the code that wrote them, so evolution is designed into the stored shape. Every
stored event and snapshot carries a manifest, a stable name independent of the Rust type path,
and a schema version, both outside the payload
([`Versioned`](../waltz/src/persistence/versioned.rs)); the payload itself is encoded
self-describingly by a [`Codec`](../waltz/src/persistence/codec.rs), CBOR by default. Old
versions can be upcast on read by overriding `Versioned::decode`: the current code declares which
versions it still decodes and converts them while replaying; a version which is neither current
nor upcast is rejected on read, and the store is never rewritten. A rejected event fails recovery;
a snapshot that can no longer be decoded is simply discarded and recovery falls back to full
replay, which is why snapshots need no migration story at all.

## Guarantees and limitations

- An appended event is never modified or deleted: the stream is append-only and is the single
  source of truth.
- The recovered state is exactly the live state: the same fold over the same events, with
  effects suppressed and `recovered` as the only world-touching step.
- Events of one effect are atomic and durable before they are applied; sequence numbers are
  gapless per `PersistenceId`.
- Conditional appends fence concurrent incarnations: one writer wins, the loser fails into
  supervision and replays.
- Outward-facing actions are at-most-once: a crash can lose a `then` continuation, never an
  event.
- Strict writes serialize commands: nothing overlaps a command's settlement, which bounds
  throughput per actor by store latency. Saving a due snapshot is part of settlement, so a
  command which triggers one also waits for the save; snapshots shorten recovery, they do not
  speed up writes.
