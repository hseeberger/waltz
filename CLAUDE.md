# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

Tasks are defined in the [justfile](justfile):

- `just all`: check, fmt, lint, test and doc; the full local gate for the `waltz` crate.
- `just check` / `just lint` / `just test` / `just doc`: the individual steps, all scoped to `-p waltz` and each run across the `serde` and `remote` feature combinations.
- `just fmt`: formats Rust (nightly rustfmt, the justfile derives the matching nightly from the installed stable) and TOML (taplo). Plain `cargo fmt` is not enough; the rustfmt config uses unstable options.
- Single test: `cargo test -p waltz --test watch <test_name>` (integration tests live in `waltz/tests/`: `supervision.rs`, `termination.rs`, `watch.rs`, plus `remoting.rs`, which needs `--features remote-dev`, has no test harness and runs its scenarios from `main`, spawning itself as further nodes).
- Examples: `just run-examples-hello`, `just run-examples-scatter-gather`, `just run-examples-remote-scatter-gather`.

Benchmarks:

- `just bench`: waltz's own criterion regression benchmarks (`waltz/benches/messaging.rs`); `just bench-save <baseline>` / `just bench-compare <baseline>` for local before/after comparisons. CI benchmarks every PR against its merge base and flags a regression at 95% confidence of at least 15% slowdown.
- `just comparison` plus `comparison-check` / `comparison-lint`: the `waltz-comparison` crate benchmarking waltz against kameo and ractor. It is deliberately excluded from `just all` and from per-PR CI so its dependencies stay out of waltz's build; touching it means running its own check and lint recipes.

CI enforces that a PR consists of exactly one commit; squash before pushing.

## Workspace layout

- `waltz/`: the actor framework, the only published-facing crate.
- `waltz-comparison/`: competitive benchmarks with strict fairness rules (unbounded mailboxes everywhere, fire-and-forget sends only, identical timing boundaries); read its README before changing any benchmark.
- `docs/actors.md`: the authoritative top-down explanation of the core, from the `Actor` trait to the run loop, with links into the implementation. Read it before changing `waltz/src`; keep it consistent with implementation changes.
- `docs/remoting.md`: the same for the feature-gated `remote` module, in particular which core guarantees carry over the network and which weaken; keep it consistent with `waltz/src/remote`.
- `mentor/`: generated code-review artifacts, not source code.

## Architecture

The core is small (~1200 lines in `waltz/src`) but dense with cross-file invariants:

- An actor is a state machine: `Actor::receive` (`actor.rs`) is a synchronous function from owned state and an `Incoming` (message or terminated signal) to `Control::Continue(next_state)` or `Control::Stop`. No async, no `&mut self`; each actor runs as one Tokio task.
- `actor_context.rs` holds the run loop, spawning and the termination sequence. Actors form a tree; termination is bottom-up, with each actor's Tokio watch channel closing (all child receivers dropped) as the barrier proving every descendant has terminated. The state is dropped before the children are stopped; only the actor value waits for the barrier, and the watchers are signaled last: a terminated signal must prove the actor's destructors have run.
- `mailbox.rs` is where messaging and death watch meet: one FIFO flume channel per actor, always unbounded underneath, with a bounded capacity enforced by a reservation counter (`quota.rs`) in front. Terminated signals bypass the capacity check but ride the same FIFO channel as ordinary messages; that shared queue is the entire mechanism behind the ordering guarantee (a terminated signal arrives behind all messages the terminated actor delivered to the watcher). Watcher registration is also owned by the mailbox and closes atomically with termination, so watch is race-free.
- `unwatch` is enforced on the watcher's side: the run loop drops a terminated signal whose sender is no longer watched before `receive` ever sees it.
- Supervision (`actor_config.rs`): errors and panics (both `init` and `receive` run under `catch_unwind`) are handled identically; `Restart` rebuilds only the state via `init` on the same actor value, retains the mailbox, and backs off exponentially in failure streaks.
- `actor_system.rs`: `spawn_root` spawns the root through the same `spawn` path as any child and registers a watcher directly in the root's watcher registry; the watcher's sink resolves `terminated()` and owns the sender keeping the root running.
- Remoting (`remote/`, `remote` feature) hangs off one seam: `ActorRef` holds a `Sink` enum whose second variant is a remote sink, so `tell`, `watch` and `unwatch` stay one API. `ActorRef` is serializable via a process-wide endpoint singleton (`endpoint.rs`) plus a lazy registry (`registry.rs`) evicting through the core's watcher mechanism. All frames towards a node ride one FIFO lane, with system frames (watch, terminated, heartbeats) bypassing the outbound capacity but not the order, mirroring `mailbox.rs`. Remote watch is two tier: a real termination keeps the local guarantees, node death (`failure.rs`, incarnation tombstones) synthesizes signals whose weaker contract is made true by tombstoning before flushing.

Changes to the run loop, mailbox or termination sequence almost always affect the ordering and watch guarantees spelled out in `docs/actors.md`; the integration tests in `waltz/tests/` encode those guarantees.
