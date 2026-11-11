# Quality gates

What runs, where, and what it proves.

| Gate | Command | In CI |
| --- | --- | --- |
| Formatting | `cargo fmt --all --check` | yes |
| Lints, warnings as errors | `cargo clippy --workspace --all-targets` with `RUSTFLAGS=-D warnings` | yes |
| Unit and integration tests | `cargo test` | Linux and macOS |
| Desktop app builds | `cargo build -p lumbre-desktop` | Linux |
| Idle footprint under 20 MiB | `benchmarks/idle.sh` | Linux, fails the build |
| Linux probes read real values (memory, PSI, cgroup) | assertion on `/v1/status` | Linux |
| Socket activation and exit-on-idle | `systemd-socket-activate` scenario | Linux |
| Dependency licences and advisories | `cargo deny check` (`deny.toml`) | yes |

Workspace lints: `unsafe_code` is denied everywhere except the two places that need it and say
why (the macOS probe's system calls, and adopting the systemd socket). `unwrap` is a warning —
and therefore an error in CI — outside tests.

Tests are of three kinds:

- **Pure rules** (router, governor, calculator, manifest validation, diff planning): exhaustive
  and fast.
- **Real I/O against local stand-ins**: downloads against a loopback HTTP server that honours
  ranges and can cut the connection; the llama.cpp client against a server that speaks the same
  streaming protocol; file tools and `lumbre-code` against temporary directories.
- **End to end**: the daemon's real router and engine on a loopback port, driven by the real
  client.

What the tests do **not** prove: anything about llama.cpp itself or a real model. The stand-in
server checks Lumbre's half of the protocol against what Lumbre believes the protocol is.

Benchmarks are scripts and examples that print what they measured on the machine they ran on:
`benchmarks/idle.sh` and `cargo run --release -p lumbre-router --example route_bench`. Numbers
quoted in the documentation say where and when they were taken.

## SonarCloud and CodeRabbit

Other Nodalia repositories use both. For Lumbre:

- **CodeRabbit** — worth enabling: it reviews pull requests with no CI cost and no extra
  configuration burden. A minimal `.coderabbit.yaml` is in the repository; it does nothing until
  the app is installed on the repository, which is the owner's decision.
- **SonarCloud** — deferred. Its Rust analysis is mostly a channel for Clippy results and
  coverage, both of which CI already enforces directly; it adds a project to create, a token to
  keep, and a second place where the gate can fail. It becomes worthwhile when coverage tracking
  over time is wanted (with `cargo llvm-cov`), or for the TypeScript of the future editor
  extension. No configuration is committed for it.

GitHub Actions are referenced by major version tag, not pinned by commit; pinning is a
hardening task on the roadmap.
