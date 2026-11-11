# Changelog

## 0.1.0 — unreleased

The first foundation.

- Rust workspace: `lumbre-resources`, `lumbre-router`, `lumbre-tools`, `lumbre-models`,
  `lumbre-runtime`, `lumbre-api`, `lumbre-core`, `lumbre-code`; the `lumbred` daemon, the
  `lumbre` desktop app and the `lumbre-code` command line.
- Adaptive routing: deterministic rules (time, date, arithmetic, file search, in Spanish and
  English), a keyword classifier, and an interface for a micro-LLM resolver.
- Resource governor: live probes on Linux and macOS, memory budget with three modes, user and
  cgroup limits, defer and degrade verdicts with reasons, idle and pressure unloading with
  hysteresis and a reload cooldown.
- Model manifests for six Apache-2.0 Qwen models, a store with resumable SHA-256-verified
  downloads, and budget-aware quantisation selection.
- llama.cpp backend as a worker process. Not yet verified against real llama.cpp.
- Local API v1 on loopback with token authentication and streaming; conversation history with
  search.
- Desktop app base with the Nodalia Design Language: chat, history, models, privacy.
- `lumbre-code`: overview, file, text and symbol search, diff check and apply, permissioned
  command execution, read-only git.
- Application icon (Leonardo Torres Quevedo on charcoal) with a Lumbre Código variant, as PNG,
  ICO and ICNS.
- systemd socket activation units; CI with formatting, Clippy, tests on Linux and macOS, an
  idle-footprint gate, Linux probe checks and dependency licence and advisory checks.
