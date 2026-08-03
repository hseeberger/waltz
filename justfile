set shell := ["bash", "-uc"]

nightly := `rustc --version | grep -oE '[0-9]{4}-[0-9]{2}-[0-9]{2}' | sed 's/^/nightly-/'`

# Flag a benchmark as regressed once we are 95% confident it is at least this fraction slower.
bench_regression_threshold := "0.15"

check:
    cargo check -p waltz --all-targets
    cargo check -p waltz --all-targets --features serde
    cargo check -p waltz --all-targets --features remote
    cargo check -p waltz --all-targets --features remote-dev
    cargo check -p waltz --all-targets --all-features

fix:
    cargo fix -p waltz --all-targets --allow-dirty --allow-staged

fmt:
    cargo +{{ nightly }} fmt
    RUST_LOG=error taplo fmt

fmt-check:
    cargo +{{ nightly }} fmt --check

lint:
    cargo clippy -p waltz --all-targets --no-deps                       -- -D warnings
    cargo clippy -p waltz --all-targets --no-deps --features serde      -- -D warnings
    cargo clippy -p waltz --all-targets --no-deps --features remote     -- -D warnings
    cargo clippy -p waltz --all-targets --no-deps --features remote-dev -- -D warnings
    cargo clippy -p waltz --all-targets --no-deps --all-features        -- -D warnings

lint-fix:
    cargo clippy -p waltz --all-targets --no-deps --allow-dirty --allow-staged --fix

test:
    cargo test -p waltz
    cargo test -p waltz --all-features

doc:
    RUSTDOCFLAGS="-D warnings --cfg docsrs" cargo +{{ nightly }} doc -p waltz --no-deps --all-features

all: check fmt lint test doc

bench:
    cargo bench -p waltz

bench-save baseline:
    cargo bench -p waltz --bench messaging -- --save-baseline {{ baseline }}

bench-compare baseline:
    cargo bench -p waltz --bench messaging -- --baseline-lenient {{ baseline }}

bench-bencher:
    cargo bench -p waltz --bench messaging -- --output-format bencher

bench-report:
    #!/usr/bin/env bash
    set -euo pipefail
    threshold={{ bench_regression_threshold }}
    threshold_pct=$(awk -v t="$threshold" 'BEGIN { printf "%.0f", t * 100 }')
    printf '### Benchmark comparison\n\n'
    printf '| Benchmark | Time | Change | Verdict |\n'
    printf '| --- | --- | --- | --- |\n'
    regressed=0
    while IFS= read -r change; do
        dir=${change%/change/estimates.json}
        id=${dir#target/criterion/}
        time=$(jq -r '.mean.point_estimate' "$dir/new/estimates.json")
        read -r pct lower < <(jq -r '.mean.point_estimate, .mean.confidence_interval.lower_bound' "$change" | paste -sd' ')
        verdict=$(awk -v l="$lower" -v t="$threshold" 'BEGIN { print (l > t) ? "⚠️ regressed" : "ok" }')
        [[ $verdict == ok ]] || regressed=1
        awk -v id="$id" -v t="$time" -v p="$pct" -v v="$verdict" \
            'BEGIN { printf "| %s | %.3f ms | %+.1f%% | %s |\n", id, t / 1e6, p * 100, v }'
    done < <(find target/criterion -path '*/change/estimates.json' | sort)
    printf '\n'
    if [[ $regressed -ne 0 ]]; then
        printf '_Regression: 95%% confident a benchmark is at least %s%% slower._\n' "$threshold_pct"
    fi

comparison:
    cargo bench -p waltz-comparison --bench frameworks

comparison-report tag="local":
    cargo run -p waltz-comparison --bin report -- --tag {{ tag }}

comparison-check:
    cargo check -p waltz-comparison --all-targets

comparison-lint:
    cargo clippy -p waltz-comparison --all-targets --no-deps -- -D warnings

run-examples-hello:
    cargo run -p waltz --example hello

run-examples-scatter-gather:
    RUST_LOG=waltz=debug cargo run -p waltz --example scatter_gather

run-examples-remote-scatter-gather:
    RUST_LOG=waltz=debug cargo run -p waltz --features remote-dev --example remote_scatter_gather
