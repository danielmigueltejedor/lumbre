# Roadmap

Lumbre 0.1 is the foundation. This is what comes next, as concrete tasks. Order within a version
is priority order.

## 0.2 — make the model path real

1. **Verify inference end to end** with real llama.cpp and real weights: load, stream, stop,
   unload, worker crash. Add a CI job that does it with the Nano model (0.6 GiB) so it stays
   verified. Fix whatever the stand-in server did not reveal.
2. **Decide Qwen3's thinking mode per tier** and handle the reasoning block explicitly.
3. **Measure, then replace the memory estimate**: record load time, peak resident memory, time
   to first token and tokens per second per model and machine; use them in `Registry::pick`.
4. **Evaluate the default models** on a small Spanish/English set (chat, reasoning, tool
   calling, code) and revise the tier assignments. Evaluate Phi-4-mini, Granite and SmolLM3 as
   alternatives.
5. **Own the worker**: a `lumbre-worker` binary built from pinned llama.cpp sources with a
   private protocol, so installing Lumbre is enough to run a model. Keep the process boundary.
6. **Micro-LLM resolver**: use the Nano tier to classify the requests the keyword classifier
   abstains on, loaded only for that and only if it fits.
7. **Icon and accent colour**, once chosen: assets, README, application icon.
8. **Packages**: Arch (PKGBUILD), Fedora (RPM), Debian/Ubuntu (deb), with the systemd units.
9. **Desktop**: Markdown rendering, a first-run flow that proposes the model that fits the
   machine, settings for mode and memory limit, Spanish interface.
10. **Coding agent loop** in `lumbre-code`: the model proposes a diff, the user reviews it, the
    tool applies it and runs the allowed tests.

## 0.3 — adapt better

- Context size and thread count adjusted to the budget at load time; reload smaller under
  sustained pressure instead of only unloading.
- PSI triggers (Linux) and memory-pressure events (macOS) instead of polling while loaded.
- A cgroup of its own for the worker, with limits set by the governor.
- NVIDIA VRAM through an optional NVML adapter; GPU layer count chosen from free VRAM.
- macOS power state and busy processes; first Windows probe.
- Tools: web search (opt-in), weather, calendar; local document search with an index.
- A syntax-tree symbol index for `lumbre-code` (tree-sitter).
- Pin GitHub Actions by commit; coverage with `cargo llvm-cov`.

## Later

- Lumbre Código as an editor extension on Open VSX, then the branded editor
  ([plan](docs/code-ide.md)).
- Long-term memory with explicit user control.
- macOS and Windows packages.
- Integration with Nodalix OS (optional; Lumbre stays independent of it).
- Accessibility pass on the desktop app with a screen reader.
