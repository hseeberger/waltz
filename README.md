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
trees, death watch with an ordering guarantee and optional remoting over QUIC. Inspired by Carl
Hewitt's [Actor Model](https://en.wikipedia.org/wiki/Actor_model) and strongly influenced by
[Akka](https://akka.io).

waltz is under active development: the API is unstable and the crate is not yet published to
[crates.io](https://crates.io/).

## Packages

- [`waltz`](waltz): the actor framework itself. See [its README](waltz/README.md) for highlights,
  getting started and the core concepts.
- [`waltz-comparison`](waltz-comparison): messaging benchmarks comparing waltz against
  [kameo](https://crates.io/crates/kameo) and [ractor](https://crates.io/crates/ractor). See
  [its README](waltz-comparison/README.md) for the methodology, fairness rules and caveats and the
  [comparison dashboard][comparison-url] for published results.

## Documentation

How waltz works under the hood, top-down with links into the implementation:

- [docs/actors.md](docs/actors.md): the core, from the `Actor` trait down to the run loop.
- [docs/remoting.md](docs/remoting.md): the `remote` feature, in particular which of the core
  guarantees carry over the network and which weaken.

## Development

The [justfile](justfile) defines the usual tasks; `just all` runs check, fmt, lint, test and doc,
each across the `serde` and `remote` feature combinations.
Formatting uses nightly rustfmt options, which `just fmt` takes care of.

Messaging throughput benchmarks (criterion) run with `just bench`. On CI every pull request is
benchmarked against its merge base and the comparison posted as a comment, and every commit on
`main` is tracked over time on the [benchmark dashboard][benchmarks-url].

The benchmarks against kameo and ractor run with `just comparison`; they are excluded from
per-pull-request CI and published to the [comparison dashboard][comparison-url] on version tags
and manual runs.

The open items on the remoting side are listed under the limitations in
[docs/remoting.md](docs/remoting.md), the main one being cluster membership: discovery resolves a
name at a known address, but nothing gossips which nodes there are.

## License

This code is open source software licensed under the
[Apache 2.0 License](http://www.apache.org/licenses/LICENSE-2.0.html).
