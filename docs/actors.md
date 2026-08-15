# Actors

This document explains how the waltz core works, from the public API down to the run loop: defining
actors, the actor tree, messaging, supervision and death watch. The implementation lives in
[`waltz/src`](../waltz/src). For a user-level summary see the [README](../waltz/README.md).

## Overview

An actor is an independent unit of state processing one incoming message or signal at a time.
waltz provides:

- typed, synchronous message handling via the [`Actor`](../waltz/src/actor.rs) trait: `receive`
  is a plain function from the current state and the incoming message to the next state, with no
  future allocated or polled per message,
- an actor tree: an [`ActorSystem`](../waltz/src/actor_system.rs) spawns the root actor and any
  actor spawns children via [`ActorContext::spawn`](../waltz/src/actor_context.rs); stopping an
  actor stops its whole subtree, children first,
- fire-and-forget, at-most-once messaging via [`ActorRef::tell`](../waltz/src/actor_ref.rs),
  with undeliverable messages dropped and logged as dead letters,
- request-response on top of that delivery: [`ActorRef::ask`](../waltz/src/actor_ref.rs) awaits a
  reply from outside the actor tree and
  [`ActorContext::reply_to`](../waltz/src/actor_context.rs) lets actors reply to each other
  through their ordinary mailboxes,
- supervision: an actor failing with an error or a panic is stopped or restarted according to
  its [`SupervisionStrategy`](../waltz/src/actor_config.rs),
- death watch with an ordering guarantee: a terminated signal arrives behind all messages the
  terminated actor has delivered to the watcher, so receiving it proves the watcher has seen
  every message from that actor it will ever see.

Each actor runs as one Tokio task and owns a mailbox, unbounded by default.

## A first example

Condensed from [`examples/hello.rs`](../waltz/examples/hello.rs), a root actor which greets for
the one message it receives and then stops:

```rust
let system = ActorSystem::new(Greeter);
system.root().tell(Greet("Waltz".to_string()));
system.terminated().await?;
```

```rust
impl Actor for Greeter {
    type Message = Greet;
    type State = ();
    type Error = Infallible;

    fn init(&self, _: &ActorContext<Self::Message>) -> Result<Self::State, Self::Error> {
        Ok(())
    }

    fn receive(
        &self,
        _: &ActorContext<Self::Message>,
        incoming: Incoming<Self::Message>,
        _: Self::State,
    ) -> Result<Control<Self::State>, Self::Error> {
        if let Incoming::Message(Greet(name)) = incoming {
            println!("Hello, {name}!");
        }
        Ok(Control::Stop)
    }
}
```

For a larger example see [`examples/scatter_gather.rs`](../waltz/examples/scatter_gather.rs),
where a root actor scatters a workload across watched workers and uses the ordering guarantee to
know when all partial results are in.

## Defining an actor

The [`Actor`](../waltz/src/actor.rs) trait separates the actor value from its state:

- `Message` is the type of the received messages; actors which react to nothing use the
  uninhabited `Nothing`.
- `State` is rebuilt on every restart, while the actor value itself survives; stateless actors
  use `()`.
- `Error` is the failure type of `init` and `receive`; infallible actors use `Infallible`.

`init` creates the initial state and may already spawn children or send messages. `receive` takes
the state by value and returns [`Control`](../waltz/src/actor.rs): `Continue(next_state)`
designates the state handling the next message, `Stop` stops the actor. This makes an actor a
state machine over owned values, with no interior mutability required.

[`Incoming`](../waltz/src/actor.rs) is either `Message(M)` or the `Terminated(ActorId)` signal
for a watched actor. Handling both through one `receive` keeps signals ordered relative to
messages (see death watch below).

`receive` is synchronous and an actor cannot be stopped while it is running, which also blocks a
Tokio worker: a `receive` which never completes keeps all ancestors from terminating. For long
running or blocking work, spawn a task and send its result back via `tell`.

## The actor tree

[`ActorSystem::new`](../waltz/src/actor_system.rs) spawns the root actor and
[`ActorContext::spawn`](../waltz/src/actor_context.rs) (available in `init` and `receive`) spawns
children, so every actor has a parent and the whole application forms a tree. Spawning returns
the child's `ActorRef` immediately; `init` runs inside the child's own task, and messages told
before it completes simply queue in the mailbox.

Termination is bottom-up: a stopping actor first stops its children, each of which recursively
does the same, and only terminates itself once all descendants have. `ActorSystem::terminated`
resolves once the root, and therefore the entire tree, has terminated.

## Configuration

[`ActorConfig`](../waltz/src/actor_config.rs), passed via `spawn_with_config` or
`ActorSystem::with_config`:

| Field                  | Default     | Meaning                                                                       |
| ---------------------- | ----------- | ----------------------------------------------------------------------------- |
| `mailbox_capacity`     | `Unbounded` | Mailbox capacity; excess messages become dead letters                          |
| `supervision_strategy` | `Stop`      | What happens when the actor fails: `Stop` or `Restart` with a `RestartPolicy`  |

An unbounded mailbox never drops messages, but an actor which cannot keep up grows it without
limit. A bounded mailbox applies no backpressure: sends to a full mailbox fail as dead letters,
while terminated signals are still delivered.

The `serde` feature makes the whole configuration deserializable, so it can come from a config file.
The one invariant it carries, `min <= max` on the backoff bounds, lives in
[`Backoff`](../waltz/src/backoff.rs): its fields are private and its constructor is fallible, and
deserialization is routed through that constructor via `#[serde(try_from = "...")]`. Every container
can therefore be plain public data, since no reachable `Backoff` value is invalid.

## References and identity

An [`ActorRef`](../waltz/src/actor_ref.rs) pairs an [`ActorId`](../waltz/src/actor_id.rs) (a
UUID v7, so IDs are unique and time-ordered) with a sender, and is cheap to clone and share.
`tell` never blocks: if the actor has terminated or a bounded mailbox is full, the message is
dropped and logged as a dead letter with the actor ID and message type. Even a delivered message
may go unprocessed if the actor stops first: delivery is at-most-once, end to end.

Internally the sender is the mailbox's sending half. The reference an actor gets for itself via
`ActorContext::self_ref` pairs it with that same half (`SelfRef` in
[`actor_ref.rs`](../waltz/src/actor_ref.rs)), which the watch mechanics below rely on.

## Request-response

Request-response is built on top of `tell`-style delivery, not beside it. A request message
carries a [`ReplyTo`](../waltz/src/ask.rs), a single-shot destination for the reply: the
responder calls `reply` exactly once, enforced by consumption, and cannot tell how the `ReplyTo`
was created. Both creators erase their delivery mechanism behind it, which also leaves room for
further backings.

[`ActorRef::ask`](../waltz/src/actor_ref.rs) is the boundary API, for code outside of any actor:
`main`, tests, HTTP handlers, spawned tasks. The given function builds the request around a
oneshot-backed `ReplyTo`, the request is sent like a tell, and the returned future resolves with
the reply. Since the caller is awaiting, failures are returned instead of only logged:
`AskError::MailboxFull` and `AskError::ActorTerminated` if the request cannot be sent,
`AskError::NoReply` once it is detected that no reply can arrive anymore, i.e. when the `ReplyTo`
was dropped without a reply or the actor stopped with the request still queued. That detection is
best-effort, which is why every ask carries a timeout: `AskError::Timeout` resolves the future
once the given duration has elapsed without a reply, e.g. against a responder which keeps the
`ReplyTo` alive without replying; a late reply is dropped as a dead letter.

[`ActorContext::reply_to`](../waltz/src/actor_context.rs) is the actor side: it creates a
`ReplyTo` which delivers the reply into this actor's own mailbox, converted into its message
type by the given function, typically an enum variant constructor. No future is created or
awaited; the reply arrives through `receive` like any other message, in the normal mailbox FIFO.
It takes the same path as a tell to this actor: it counts against a bounded capacity and becomes
a dead letter if the asker has terminated or its mailbox is full.

Supervision composes with the retained mailbox: a request queued behind a failing message
survives a restart and is answered by the restarted state, while a request consumed by the
failing `receive` itself is not redelivered and hence resolves as `NoReply`.

## Mailboxes

[`mailbox.rs`](../waltz/src/mailbox.rs). A mailbox is a FIFO channel of `Incoming<M>` split into
a `MailboxHandle` (the sending half, cloned into every `ActorRef`) and a `Mailbox` (the receiving
half, owned by the actor's run loop). Messages from one sender are received in the order they
were sent.

The underlying channel is always unbounded; a bounded mailbox is enforced by a lock-free
reservation counter ([`quota.rs`](../waltz/src/quota.rs)) in front of it: a message reserves
capacity before it is enqueued and releases it when it is received. This split is deliberate:
terminated signals and capacity answer different needs, so `send_terminated` enqueues into the
same FIFO channel (preserving order behind queued messages) but bypasses the capacity check,
since a terminated signal must never be dropped.

The mailbox also owns watcher registration: watchers are collected in a map keyed by watcher ID
which termination takes and closes atomically, so a watcher racing with termination either
registers in time and is signaled, or fails registration and learns immediately that the actor
has terminated. Either way no watcher is lost. Registration is O(1) and two-sided: watching twice
registers once as a property of the map, and each watching actor records what it watches, so
`unwatch` and the watcher's own termination deregister it. Dead watchers hence never accumulate.

## The run loop

`spawn` in [`actor_context.rs`](../waltz/src/actor_context.rs), reached via
`ActorContext::spawn_with_config` and by `spawn_root`, spawns one Tokio task per actor:

1. Run `init`; a failure is fed to supervision just like a failure of `receive`, so under
   `Restart` even the first initialization is retried.
2. Loop: wait (biased) for either the parent's stop signal or the next incoming from the
   mailbox, and pass the incoming to `receive`.
3. Map the result to an outcome: `Continue` carries the next state, an error or panic consults
   the `SupervisionStrategy` (`Restart` or `Stop`), `Control::Stop` stops.
4. On stop, run the termination sequence below.

Both `init` and `receive` run under `catch_unwind`, so a panic is handled exactly like a returned
error: logged with the actor ID and fed to supervision. The biased select ensures a stop signal
is honored before further queued messages once the parent is stopping.

## Stopping and termination

Every actor owns a Tokio watch channel; its children hold receiver clones and every child task
selects on it (the "parent stopping" branch of the run loop). The termination sequence, in
`terminate` and `stop_children` in [`actor_context.rs`](../waltz/src/actor_context.rs):

```mermaid
sequenceDiagram
    participant A as stopping actor
    participant C as children
    participant W as watchers
    A->>A: drop state (destructors run)
    A->>C: stop signal (watch channel)
    C->>C: same sequence, recursively
    C-->>A: channel closed (all receivers dropped)
    A->>A: drop actor value (destructors run)
    A->>W: terminated signals (into their mailboxes)
```

The state goes first: it is consumed by the `receive` which decided to stop, or dropped right
where the parent's stop signal is received, in both cases before the children are stopped. A
resource an actor keeps in its state is hence gone while its children may still be running; what
outlives the whole subtree belongs in the actor value.

The closed channel is the completion barrier: a child drops its receiver clone only when its task
ends, so the channel closing proves every child has terminated. Only then does the actor drop its
own value and finally signal its watchers: a terminated signal must prove that the actor's
destructors have run.

The mailbox is drained and disconnected right at the start of the sequence: senders observe the
termination while the children still stop, and the destructors of the drained messages run as
part of it, so e.g. a queued request's reply channel resolves its ask as `NoReply` instead of
pending forever. A send racing with the drain can still slip past it; such a message is retained
until its last sender is dropped, which is why the `NoReply` detection is best-effort.

## Supervision and restarts

[`SupervisionStrategy`](../waltz/src/actor_config.rs) decides what happens when `init` or
`receive` fails: `Stop` (the default) runs the termination sequence, `Restart` rebuilds the
actor's state, limited and paced by its `RestartPolicy`.

A restart stops the children (they belong to the failed state), waits for the backoff delay, then
re-runs `init` on the same actor value: anything the actor value itself carries survives, only the
state is rebuilt. The mailbox is retained, so messages queued behind the failing one are processed
by the restarted state and keep queuing during the backoff; the failing message itself is consumed
and not redelivered. Before restarting, the loop probes whether the parent has meanwhile started
stopping and stops instead; the parent stopping also interrupts a backoff delay in progress.

Failures form a streak: the n-th restart within a streak is delayed by `backoff.min() * 2^(n-1)`
([`backoff.rs`](../waltz/src/backoff.rs)), capped at `backoff.max()`; once a streak exceeds
`max_restarts`, the actor stops, which is how a persistent failure escalates to the watchers. A
streak ends, resetting count and backoff, once the actor has run for at least `reset_after`
without failing. Failures of `init` count into the same streak, including the first
initialization at spawn, so a failing initializer, e.g. one connecting to a struggling
dependency, is retried with backoff instead of looping hot.

Watches survive a restart: the restarted state can receive terminated signals for actors the
previous incarnation watched, including its own stopped children if it watched them.

## Death watch

`ActorContext::watch(other)` registers this actor as a watcher of `other`; once `other` has
terminated, this actor receives `Incoming::Terminated(other.actor_id())`. If `other` has already
terminated, registration fails and the signal is delivered right away, so watching is race-free.

`ActorContext::unwatch(other)` reverts a watch with a strong contract: after it returns, no
terminated signal for `other` is received, even if `other` has already terminated and the signal
is already enqueued. This is enforced on the watcher's side: the run loop delivers a terminated
signal only if its sender is still watched, consuming the watch, so a stale signal is dropped
before `receive` ever sees it. The same watcher-side bookkeeping deregisters a terminating actor
from everything it still watches.

A watcher (in [`mailbox.rs`](../waltz/src/mailbox.rs)) is essentially a sending handle into the
watching actor's own mailbox, handed to the watched actor. At termination, after its destructors
have run, the watched actor sends the terminated signal through it: into the same FIFO channel as
all the messages it delivered to the watcher before, which is the entire mechanism behind the
ordering guarantee. No coordination is needed beyond the shared queue.

At-most-once delivery bounds what the signal can prove: a message dropped as a dead letter, e.g.
told to a full bounded mailbox, never entered the queue. Receiving the signal hence proves that
the watcher has seen every message from the terminated actor it will ever see: each arrived
before the signal or was dropped as a dead letter.

## Resolving `ActorSystem::terminated`

`spawn_root` in [`actor_system.rs`](../waltz/src/actor_system.rs) spawns the root actor through
the same `spawn` path as any child and registers a watcher directly in the root's watcher
registry. The watcher's sink resolves the oneshot behind `terminated()` and also owns the sender
of the root's stop channel, so the root keeps running exactly until its own termination has
signaled the watchers and drops them. If the root has already terminated when the watcher is
registered, registration fails and the oneshot is resolved right away, mirroring the race-free
watch registration. Dropping the `ActorSystem` without awaiting it does not stop the tree; the
root stops on its own terms.

## Panicking destructors

Every drop path the framework controls contains destructor panics: the state dropped when the
parent stops the actor as well as the actor value and the mailbox dropped at termination are all
dropped under `catch_unwind` (`drop_containing_panic` in
[`actor_context.rs`](../waltz/src/actor_context.rs)), so the panic is logged and termination
completes, including the signals to the watchers. A destructor panic on a normal return of
`receive` is equally contained: when `receive` returns an error and the state's destructor panics
while the frame returns, that panic is caught like any other and fed to supervision. The locks the
framework holds itself recover from poisoning ([`sync.rs`](../waltz/src/sync.rs)), so a panic
contained by supervision cannot disable the watcher registry.

The one case out of reach is a panic during a panic: `init` or `receive` panics, and while that
panic unwinds, the destructor of a value still alive in the frame (the state, the message, a
local) panics as well. Rust aborts the process on a panic during unwinding, below `catch_unwind`
and hence below supervision. This is inherent to Rust, not specific to waltz: keep destructors
panic-free.

## Guarantees and limitations

- `tell` is fire-and-forget and at-most-once: no acknowledgements, no redelivery, dead letters
  are logged, not returned.
- Messages from one sender arrive in send order; terminated signals are ordered behind the
  watched actor's delivered messages and prove its destructors have run.
- `receive` is synchronous and cannot await; long running or blocking work belongs in a
  spawned task reporting back via `tell`.
- Request-response is at-most-once too: `ask` resolves exactly once, with the reply or an error,
  at the latest when its timeout elapses, and never redelivers; a `reply_to` reply is an ordinary
  send, dropped as a dead letter if the asker is gone or its bounded mailbox is full.
