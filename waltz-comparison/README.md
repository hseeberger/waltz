# waltz-comparison

Messaging benchmarks comparing [waltz](../waltz) against two actively maintained Rust actor
frameworks, [kameo](https://crates.io/crates/kameo) and
[ractor](https://crates.io/crates/ractor).

This package is never published and is not part of `just all`, so its dependencies stay out of
waltz's own build and out of the per-pull-request CI.

## Running

```shell
just comparison           # run the benchmarks
just comparison-report    # render the HTML report from the results
just comparison-check     # cargo check
just comparison-lint      # clippy
```

Results are written to `target/criterion-comparison`, deliberately separate from the
`target/criterion` tree used by waltz's own regression benchmarks, so the two never mix.

Runs published by CI are collected on the
[comparison dashboard](https://hseeberger.github.io/waltz/comparison/), one directory per version
tag and per manual run.

## Benchmarks

Three shapes, modeled on [`waltz/benches/messaging.rs`](../waltz/benches/messaging.rs):

- `flood`: the bench thread floods a single counting actor with 100,000 messages.
- `ping_pong`: pairs of actors play ping-pong for 1,000 rounds, with one pair and eight pairs.
- `fan_out`: the bench thread sends 100,000 messages round-robin to eight and to 32 workers.

Actor counts are fixed rather than derived from `available_parallelism`, so benchmark ids stay
stable across machines and published results remain comparable. Also unlike the waltz bench,
`fan_out` has the bench thread rather than a root actor distribute the messages.

## Why these two competitors

Both are actively released and, decisively, both run on a plain Tokio runtime like waltz, which
makes the comparison structurally meaningful. `actix` was considered and rejected despite being far
more popular: it uses its own `System`/`Arbiter` and places actors on a single-threaded arbiter by
default, so a fair comparison would require spreading actors across arbiters and would still be
architecturally apples-to-oranges. `xtra` and `coerce` are dormant.

## Fairness rules

The point of these benchmarks is that all three frameworks perform the *same work*:

1. **One run, one machine, back-to-back.** Numbers are only ever compared within a single run.
   Never compare figures across runs or machines.
2. **Unbounded mailboxes everywhere.** waltz defaults to unbounded, ractor is unbounded, and kameo
   is explicitly spawned via `spawn_with_mailbox(.., mailbox::unbounded())` because its default is a
   *bounded* mailbox of capacity 64, which would otherwise apply backpressure the others do not.
3. **Non-blocking fire-and-forget sends only.** waltz `ActorRef::tell`, kameo
   `tell(..).try_send()`, ractor `ActorRef::send_message`. No awaited sends (that is backpressure, a
   different guarantee) and no request-response calls.
4. **Identical timing boundaries.** Every framework goes through the same `measure` helper: spawning
   happens outside the measured region, and the timer covers sending plus awaiting termination. In
   `ping_pong` that includes both actors of a pair: waltz tears the ponger down as a child of the
   pinger, kameo and ractor stop and await it in the pinger's stop hook.
5. **Identical runtime**: one multi-threaded Tokio runtime, same configuration for all.
6. **Competitors get their fastest configuration** (see below).

Termination is also the correctness check: each actor only stops once it has processed exactly its
expected number of messages, so a dropped or lost message makes a benchmark hang rather than finish
early.

## Competitors are configured for speed, not defaults

Both competitors ship per-message instrumentation enabled by default, which waltz has no equivalent
of (waltz depends on `tracing` too, but emits nothing per message). Benchmarking them as-shipped
would charge them for an observability feature while measuring waltz without one, so both are built
with those features off:

- `kameo`: `default-features = false`, dropping `tracing` (and `macros`, which has no runtime cost).
  Measured **12.5% faster** than with defaults on `flood`.
- `ractor`: `default-features = false, features = ["tokio_runtime"]`, dropping
  `message_span_propogation`. Measured **21% faster** than with defaults on `flood`.

This deliberately biases the setup *in the competitors' favour*, which is the appropriate direction
for a comparison published by waltz's own maintainer. Anyone reproducing the out-of-the-box
experience should expect both to be correspondingly slower.

## Caveats

Read these before drawing conclusions from any number. The published report repeats this list,
folding the speed configuration above into it; keep both in sync with
[`src/bin/report.rs`](src/bin/report.rs).

1. **waltz's `receive` is synchronous; kameo's and ractor's handlers are `async fn`.** waltz
   therefore avoids allocating and polling a future per message, but in exchange it cannot await
   inside `receive`. This is a capability difference, not only a speed difference, and it favours
   waltz on exactly these microbenchmarks.
2. **waltz's mailbox is statically typed; the others erase message types.** kameo and ractor box
   messages to support their richer messaging APIs, which costs an allocation and a dynamic dispatch
   per message that waltz does not pay.
3. **These are messaging microbenchmarks only.** They say nothing about supervision, distribution,
   ergonomics, memory use or production readiness. kameo and ractor are mature,
   feature-rich frameworks; waltz is under active development and does far less.
4. **CI numbers come from a shared 2-core GitHub hosted runner**, so absolute figures there are not
   representative of real deployments; only the relative comparison within a run is meaningful.
5. **Written and run by waltz's maintainer.** The methodology and every line of the benchmark are in
   this package; corrections and pull requests are welcome.
