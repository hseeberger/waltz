# waltz

[![license][license-badge]][license-url]
[![build][build-badge]][build-url]
[![benchmarks][benchmarks-badge]][benchmarks-url]
[![comparison][comparison-badge]][comparison-url]

[license-badge]: https://img.shields.io/github/license/hseeberger/waltz
[license-url]: https://github.com/hseeberger/waltz/blob/main/LICENSE
[build-badge]: https://img.shields.io/github/actions/workflow/status/hseeberger/waltz/ci.yml
[build-url]: https://github.com/hseeberger/waltz/actions/workflows/ci.yml
[benchmarks-badge]: https://img.shields.io/badge/benchmarks-dashboard-informational
[benchmarks-url]: https://hseeberger.github.io/waltz/dev/bench/
[comparison-badge]: https://img.shields.io/badge/comparison-dashboard-informational
[comparison-url]: https://hseeberger.github.io/waltz/comparison/

An actor framework for Rust, built on [Tokio](https://tokio.rs): typed messages, supervision
trees and death watch with an ordering guarantee. Inspired by Carl Hewitt's
[Actor Model](https://en.wikipedia.org/wiki/Actor_model) and strongly influenced by
[Akka](https://akka.io).

waltz is under active development: the API is unstable and the crate is not yet published to
[crates.io](https://crates.io/).

## Highlights

- **Typed actors as state machines.** An actor implements the `Actor` trait with associated
  `Message`, `State` and `Error` types: `init` creates the initial state, `receive` consumes the
  current state and a message and returns the state for the next one via `Control::Continue`, or
  stops via `Control::Stop`. No `&mut self`, no async in actor code.
- **Supervision tree.** Actors form a tree below the root actor of an `ActorSystem`. Stopping an
  actor stops its children first; `ActorSystem::terminated` resolves once the whole tree has
  terminated.
- **Fire-and-forget messaging.** `ActorRef::tell` never blocks and delivers at most once;
  undeliverable messages are dropped and logged as dead letters (structured logging via `tracing`
  with fields).
- **Request-response.** `ActorRef::ask` awaits a reply from outside the actor tree;
  `ActorContext::reply_to` lets actors reply to each other through their ordinary mailboxes,
  keeping actor code free of futures.
- **Death watch with an ordering guarantee.** `ActorContext::watch` delivers a terminated signal
  which is ordered behind all messages the terminated actor has delivered to the watcher, hence
  receiving it proves the watcher has seen every message from that actor it will ever see.
  `ActorContext::unwatch` reverts a watch, guaranteed even against an already enqueued signal.
- **Supervision strategies.** On an error or panic (panics are caught), the configured strategy
  decides: `Restart` re-initializes the actor with a restart limit and exponential backoff, `Stop`
  terminates it.
- **Bounded or unbounded mailboxes.** Bounded mailboxes drop messages beyond capacity as dead
  letters, but terminated signals are never dropped.

## Getting started

waltz is not yet on crates.io; use a git dependency:

```toml
[dependencies]
anyhow = { version = "1.0" }
tokio  = { version = "1", features = [ "macros", "rt-multi-thread" ] }
waltz  = { git = "https://github.com/hseeberger/waltz" }
```

A minimal actor system with a single actor which handles one message and stops:

```rust
use anyhow::Context;
use std::convert::Infallible;
use waltz::{Actor, ActorContext, ActorSystem, Control, Incoming};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let system = ActorSystem::new(Greeter);
    system.root().tell(Greet("Waltz".to_string()));
    system
        .terminated()
        .await
        .context("awaiting actor system termination")
}

struct Greeter;

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

struct Greet(String);
```

`ActorSystem::new` and `ActorContext::spawn` use the default `ActorConfig`; use
`ActorSystem::with_config` and `ActorContext::spawn_with_config` to choose a mailbox capacity or
supervision strategy.

## Core concepts

A short tour; for the full picture, top-down with links into the implementation, see
[docs/actors.md](../docs/actors.md).

### Actors and state

An actor is defined by implementing the `Actor` trait. `init` creates the initial state, possibly
spawning child actors or sending messages. `receive` gets the current state by value along with an
incoming message or signal and designates the state for the next one: a state machine rather than
mutation. For stateless actors the state is `()`; actors which never receive messages (pure
supervisors, for example) use the uninhabited `Nothing` as message type.

Failures are values: `Error` is the actor's failure type (`Infallible` for actors that cannot
fail). Inside `receive`, use `?` to escalate a failure to supervision and an explicit `match` to
handle it as part of the domain.

`receive` is synchronous and runs on a Tokio worker: an actor cannot be stopped while `receive` is
running, so a `receive` which never completes keeps all its ancestors from terminating. For long
running or blocking work, spawn a task and send the result back via `ActorRef::tell` or a
`ReplyTo`.

### The actor tree and termination

Creating an `ActorSystem` spawns the root actor; every actor can spawn children via
`ActorContext::spawn`. When an actor stops (by returning `Control::Stop`, by failing under the
`Stop` strategy, or because its parent stopped), its children are stopped first; only once all
descendants have terminated does it terminate itself. Consequently `ActorSystem::terminated`
resolves exactly when the entire tree has terminated.

### Messaging

`ActorRef::tell` is non-blocking, fire-and-forget and at-most-once. If the actor has terminated, or
its bounded mailbox is full, the message is dropped and logged as a dead letter. Delivery does not
imply processing: even a delivered message may go unprocessed if the actor stops before getting to
it.

Request-response builds on the same delivery: a request message carries a `ReplyTo`, a single-shot
reply destination consumed by `reply`. From outside the actor tree, `ActorRef::ask` sends the
request and awaits the reply, returning an `AskError` instead of only logging when the mailbox is
full, the actor has terminated or it is detected that no reply can arrive anymore; that detection
is best-effort, so every ask carries a timeout which resolves the future at the latest when it
elapses. Between actors, `ActorContext::reply_to` creates a `ReplyTo` which delivers the
reply into the asking actor's own mailbox, converted into its message type, so the reply arrives
through `receive` like any other message.

### Watch

`ActorContext::watch` registers interest in another actor's termination: the watcher receives an
`Incoming::Terminated` signal carrying the terminated actor's `ActorId`. The signal is ordered
behind all messages the terminated actor has delivered to the watcher, hence receiving it proves
the watcher has seen every message from that actor it will ever see: each arrived before the
signal or was dropped as a dead letter; see `examples/scatter_gather.rs` for putting this to work.
Watching an actor that has already terminated delivers the signal right away, and terminated
signals are delivered even when a bounded mailbox is full. `ActorContext::unwatch` stops
watching: after it returns, no terminated signal for that actor is received, even if the signal
was already enqueued.

### Supervision

Each actor is configured with a `SupervisionStrategy` deciding what happens when `init` or
`receive` returns an error or panics: `Stop` terminates the actor, `Restart` stops its children and
re-runs `init` for a fresh state, limited and paced by a `RestartPolicy`: restarts back off
exponentially between the backoff's `min` and `max`, more than `max_restarts` failures in a
streak stop the actor, and running for `reset_after` without failure ends the streak. Failures are
logged at error level either way.

### Configuration

`ActorConfig` currently holds the mailbox capacity and the supervision strategy:

```rust
let config = ActorConfig {
    mailbox_capacity: MailboxCapacity::Bounded(NonZeroUsize::MIN),
    ..Default::default()
};
let child = context.spawn_with_config(actor, config);
```

With the `serde` feature the whole configuration is deserializable, so it can be read from a config
file, with human readable durations:

```toml
mailbox_capacity = { bounded = 100 }

[supervision_strategy.restart]
max_restarts = 3
reset_after  = "30s"
backoff      = { min = "250ms", max = "4s" }
```

```yaml
mailbox_capacity:
  bounded: 100

supervision_strategy:
  restart:
    max_restarts: 3
    reset_after: 30s
    backoff:
      min: 250ms
      max: 4s
```

waltz stays format agnostic and pulls in no parser of its own, so picking the loader is up to the
application. [`config`](https://crates.io/crates/config) is the recommended one: it normalizes every
format into one value tree before deserializing, which is what makes the YAML above plain maps, and
it reports the key path along with any error. Note that `serde_yaml` deserializes the same types
differently, expecting a YAML tag (`supervision_strategy: !restart`) instead.

Anything omitted falls back to its default. Backoff bounds which contradict each other are rejected
rather than silently repaired: `Backoff::new` is fallible and deserialization goes through it, so an
invalid pair is unrepresentable whether it comes from code or from a file:

```text
max backoff 1s below min backoff 10s for key `supervision_strategy.backoff`
```

## Examples

- [`hello`](examples/hello.rs): the getting started snippet above:

  ```shell
  cargo run --quiet -p waltz --example hello
  ```

- [`scatter_gather`](examples/scatter_gather.rs): a root actor scatters a workload across worker
  actors and gathers their partial results, using the watch ordering guarantee to know when all
  results are in. It prints the total to stdout and logs to stderr; the log level is configured
  via `RUST_LOG`:

  ```shell
  RUST_LOG=waltz=debug cargo run --quiet -p waltz --example scatter_gather
  ```

## License

This code is open source software licensed under the
[Apache 2.0 License](http://www.apache.org/licenses/LICENSE-2.0.html).
