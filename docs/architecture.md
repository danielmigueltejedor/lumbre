# Architecture

Lumbre is a Rust workspace. Each crate has one job, depends only on what it needs, and can be
tested without the others. Nothing in the tree is a placeholder: a directory exists when there is
code in it.

```
crates/
  lumbre-resources   probes (Linux, macOS) and the governor: budget, residency, load gate
  lumbre-router      rules → classifier → resolver; says what a request deserves
  lumbre-tools       Tool trait, permission model; clock, calculator, file search
  lumbre-models      manifests, registry, store, verified resumable download
  lumbre-runtime     Backend trait; llama.cpp as a worker process
  lumbre-api         the /v1 contract: wire types, paths, token, blocking client
  lumbre-core        the coordinator (Engine), configuration, conversation history
  lumbre-code        Lumbre Código library: workspace, symbols, diffs, permissions, run
apps/
  lumbred            the daemon: HTTP on loopback over the Engine
  lumbre-desktop     the desktop app (binary `lumbre`): an API client, egui + NDL
cli/
  lumbre-code        the `lumbre-code` binary
models/              model manifests (TOML). No weights.
integrations/linux/  systemd units
benchmarks/          repeatable measurements
design/              icon proposals
docs/
```

Dependencies point one way:

```
lumbre-desktop ─┐
lumbre-code CLI ─┼─► lumbre-api ◄─ lumbred ─► lumbre-core ─► router, resources, tools, models, runtime
                └─► lumbre-code (library; no daemon needed)
```

The clients depend on `lumbre-api` only. They cannot reach the engine except through the
contract, which is what "strict separation between logic, interface and operating system" means
in practice here:

- **Logic** — `lumbre-core`, `lumbre-router`, the governor in `lumbre-resources`. Pure where
  possible: the router and the governor take data and return decisions, with no I/O or clock.
- **Interface** — `lumbre-desktop` and the CLI. They render what the API returns.
- **Operating system** — `lumbre-resources::{linux, macos}`, the worker spawn in
  `lumbre-runtime`, socket activation in `lumbred`. Each sits behind a trait or a `cfg`.

## Decisions and their reasons

### llama.cpp, as a worker process, behind a trait

llama.cpp is the mature open engine for GGUF models on CPUs and consumer GPUs; rewriting it
would be exactly the kind of reimplementation without technical reason the project avoids.
`lumbre-runtime::Backend` is the seam: load, generate (streaming, stoppable), unload, report
memory. Another backend (an in-process binding, MLX, ONNX Runtime, a remote machine on the
user's own network) implements the same trait.

0.1 runs llama.cpp's own `llama-server` as a child process and talks to it over loopback, rather
than linking the library, because for a resource manager a process boundary is the strongest
tool available:

- **Unloading is real.** Ending the process returns all of its memory to the system. An
  in-process unload leaves that to the allocator, and a fragmented heap keeps a "freed" model
  resident — the opposite of what the governor promises.
- **The idle daemon stays small.** It links no inference code. Measured: under 7 MiB resident.
- **Isolation.** A crash in native code cannot take the daemon or the conversation with it.
- **Limits.** The worker can be put in its own cgroup with a hard memory ceiling.

The cost is honest too: one HTTP hop on loopback per request (negligible next to inference), a
dependency on an external binary that 0.1 does not ship, and less control than the C API over
context reuse and caches. The planned direction (see the roadmap) is a `lumbre-worker` binary of
Lumbre's own, built from pinned llama.cpp sources, speaking a small private protocol — keeping
the process boundary while removing the dependency on upstream's server and its HTTP layer.

This is also where Lumbre is, today, closest to the managers it wants to differ from: Ollama
runs llama.cpp as a subprocess as well. The difference is not the engine; it is everything that
decides whether and how the engine runs.

### One daemon, clients over a versioned API

The assistant, the coding agent and any future client share one resident process and one model.
The API is HTTP/1.1 with JSON and server-sent events because every language and tool already
speaks it; it lives under `/v1` so it can evolve without breaking clients.

It listens on loopback TCP rather than a Unix socket for portability (Windows) and because
systemd can socket-activate either. Loopback is reachable by every local account, so requests
carry a bearer token from a file with owner-only permissions; the daemon refuses to bind any
non-loopback address. A Unix socket with peer-credential checks is a possible hardening on Linux
later.

The daemon runs a single-threaded Tokio runtime. Blocking work (generation, downloads) runs on
threads that are created on demand and released after ten idle seconds; housekeeping sleeps
without a timer while no model is loaded. An idle daemon therefore has one thread and no wakeups.
Tokio is used only here, where there is real concurrent I/O. The libraries are synchronous:
they are easier to test and a routing decision or a budget calculation has nothing to await.

### egui for the desktop app, not Tauri

Tauri 2 was the obvious candidate and was rejected for this application:

| | Tauri 2 | egui (chosen) |
| --- | --- | --- |
| Memory | A WebView process tree; tens to hundreds of MiB before any content | One process; the interface is a few MiB of the app |
| Consistency with Nodalia | A second stack next to the Rust apps | The same toolkit as Forja, Pluma and the other Rust apps |
| Design system | NDL's web CSS | NDL's `nodalia-theme` crate, already written for egui |
| Toolchain | Rust plus Node and a bundler | Rust only |
| Text and accessibility | Excellent (the browser's) | Good enough for a chat; weaker for rich text and screen readers |

An assistant whose reason to exist is a small footprint should not open a browser engine to show
a text box. The weaknesses of egui that matter here — Markdown rendering and accessibility — are
known work, listed in the roadmap.

### The router does not know the machine, and the governor does not know the request

`lumbre-router` answers "what does this deserve?" from the text alone. `lumbre-resources`
answers "what may run now?" from the machine alone. `lumbre-core` is the only place where the two
meet. Each can be tested exhaustively on its own, and each can be replaced (a better classifier,
another platform's probe) without touching the other.

### Honest non-answers are part of the type system

`Answer` has six variants; four of them are ways of *not* answering — `Deferred`, `NoModel`,
`BackendMissing`, `Error` — each carrying its reason. An interface cannot show a made-up reply
because there is no code path that produces one: with no model there is no text.

## What a request does

```
client ──POST /v1/chat──► lumbred ──► Engine::chat
  1. Router::route(text)                         → Tool{name,args} | Model{tier,intent}
     event: routed
  2a. Tool: Registry::invoke (permission check)  → Answer::Tool            ── done
  2b. Model:
      Backend::available?                         no → Answer::BackendMissing
      Probe::snapshot + budget(policy, snapshot)  Defer → Answer::Deferred
      Registry::pick(tier, budget, installed)     none → Answer::NoModel (+ suggestion)
      resident model differs?                     load gate closed → Answer::Deferred
        unload old, event: loading, Backend::load
      Backend::generate, event: token …           → Answer::Model (+ measured stats)
  3. housekeeping (every 5 s while loaded): Residency::observe → unload when idle or under pressure
```

## Not built yet, and where it will go

These appear in the brief's suggested layout but have no crate, because there is no code for
them yet:

- **Agents** (multi-step tool use driven by a model) — a `lumbre-agents` crate once inference is
  verified; the tool registry and permission model it needs already exist.
- **Memory** (long-term, document recall) and **search** (local documents, web) — separate
  crates behind `Tool` implementations, each with its own permission.
- **macOS, Windows and Nodalix integrations** — packaging and service management per platform,
  next to `integrations/linux`.
- **The IDE** — a separate repository; see [code-ide.md](code-ide.md).
