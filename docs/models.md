# Models

Lumbre trains no models. It points at open-weight models published by others, says whose they
are, and checks what it downloads.

"Lumbre Nano", "Mini", "Lumbre" (standard), "Large" and "Lumbre Code" are **tiers** — size
classes the router asks for. A manifest assigns an upstream model to a tier and always carries
its real name, publisher, licence and pinned revision. If Lumbre ever ships fine-tuned or
re-quantised files, they will get their own manifests with their own provenance.

## Manifest format

`models/<id>.toml`, schema 1:

```toml
schema = 1
id = "qwen3-4b"
name = "Qwen3 4B"
tier = "standard"                 # nano | mini | standard | large | code
publisher = "Alibaba Cloud (Qwen team)"
upstream = "https://huggingface.co/Qwen/Qwen3-4B"        # the model card
source = "https://huggingface.co/Qwen/Qwen3-4B-GGUF"     # where files come from
revision = "bc640142c66e1fdd12af0bd68f40445458f3869b"    # commit the digests belong to
license = "Apache-2.0"            # SPDX identifier of the weights' licence
license_url = "…/LICENSE"
format = "gguf"
parameters = "4B"

[[variant]]
quant = "Q4_K_M"
file = "Qwen3-4B-Q4_K_M.gguf"
size_bytes = 2497280256
sha256 = "7485fe6f…"
```

Unknown fields and unknown schema versions are rejected. Downloads use
`<source>/resolve/<revision>/<file>` — a pinned commit, never a branch — go to `<file>.part`,
resume with a `Range` request, and are only moved into place when size and SHA-256 match. A
mismatch deletes the partial file. Your own manifests go in `models/` inside Lumbre's
configuration directory and override built-in ones with the same id.

## Built-in manifests

Sizes, digests, revisions and licences below were read from the Hugging Face API on 2026-11-11.
"Estimated memory" is Lumbre's heuristic at a 4096-token context (see below), not a measurement.

| Tier | Model | Quantisations | File | Estimated memory | Licence |
| --- | --- | --- | --- | --- | --- |
| Nano | Qwen3 0.6B | Q8_0 | 0.6 GiB | 0.9 GiB | Apache-2.0 |
| Mini | Qwen3 1.7B | Q8_0 | 1.7 GiB | 2.3 GiB | Apache-2.0 |
| Standard | Qwen3 4B | Q4_K_M · Q5_K_M · Q6_K · Q8_0 | 2.3–4.0 GiB | 3.1–5.2 GiB | Apache-2.0 |
| Large | Qwen3 8B | Q4_K_M · Q5_K_M | 4.7–5.4 GiB | 6.1–7.0 GiB | Apache-2.0 |
| Code | Qwen2.5 Coder 1.5B Instruct | Q4_K_M · Q8_0 | 1.0–1.8 GiB | 1.5–2.4 GiB | Apache-2.0 |
| Code | Qwen2.5 Coder 7B Instruct | Q4_K_M · Q5_K_M | 4.4–5.1 GiB | 5.7–6.6 GiB | Apache-2.0 |

The official `Qwen/…-GGUF` repositories were chosen as sources because the publisher of the
weights is also the publisher of the quantised files: one party to trust and attribute. They do
not offer every quantisation (the 0.6B and 1.7B come only as Q8_0).

## Why these

The criteria, in order: **a licence that allows redistribution and commercial use without
conditions on the user**, availability without an access gate, a range of sizes from one family
(so behaviour is consistent across tiers), usable on a CPU within 16 GB.

| Candidate | Licence (HF metadata, 2026-11-11) | Gated | Decision |
| --- | --- | --- | --- |
| Qwen3 0.6B / 1.7B / 4B / 8B | Apache-2.0 | no | **Default** for the general tiers: one family from 0.6B to 8B under one permissive licence |
| Qwen2.5-Coder 1.5B / 7B | Apache-2.0 | no | **Default** for the code tier |
| Qwen2.5-Coder 3B | `qwen-research` | no | **Excluded.** Research licence; not redistributable for general use. The 3B size would fit 16 GB well, which is exactly why this is called out |
| Phi-4-mini | MIT | no | Good alternative for Mini/Standard; candidate for a manifest once evaluated |
| IBM Granite 4.0 micro | Apache-2.0 | no | Candidate; tool use is a stated focus of the family |
| SmolLM3 3B | Apache-2.0 | no | Candidate for Mini |
| Mistral 7B Instruct v0.3 | Apache-2.0 | no | Older; kept as a known-good fallback candidate |
| Gemma 3 | Gemma terms | manual gate | **Excluded as a default.** Custom terms with a use policy, and an access gate: incompatible with "install and it works, no account" |
| Llama 3.2 | Llama community licence | manual gate | **Excluded as a default**, same reasons |

Nothing stops a user from adding a manifest for a gated or differently licensed model they have
the right to use; Lumbre just does not point at them out of the box.

### What has not been evaluated

This selection rests on licence, size and availability, which were verified. It does **not**
rest on measurements of quality, because none have been made by this project yet:

- **Spanish and English quality**, reasoning, tool calling and code quality per tier: the model
  cards make claims (Qwen3 describes itself as multilingual with tool-calling support); Lumbre
  has not tested them. A small bilingual evaluation set is a 0.2 task.
- **Speed on CPU** (tokens per second, time to first token) per model and quantisation.
- **Real memory use** per model and context size.
- **Qwen3's thinking mode.** Qwen3 can emit a reasoning block before the answer. Lumbre ignores
  `reasoning_content` deltas in the stream, but whether llama.cpp separates the block for these
  files with default settings, and what thinking costs in latency on a CPU, is untested. The
  Nano and Mini tiers probably want it off.

## Choosing a quantisation

`Registry::pick(tier, budget, context, usable)` returns, among installed variants: the requested
tier's largest file whose estimate fits the budget; else the same for the next tier down
(Large → Standard → Mini → Nano; Code → Standard → …); else nothing, with a suggestion of what
would fit. Within a model, larger file means higher-quality quantisation, so "largest that fits"
is "best that fits".

The estimate:

```
memory ≈ file size × 1.05 + context tokens × (file size / 20 000) + 200 MiB
```

It is a heuristic chosen to err high. The context term in particular scales with file size,
which over-counts high-bit quantisations of the same model. It is replaced by measured values
once they exist.

## Risks for 16 GB

Stated plainly, because the 16 GB goal is the one most likely to be missed:

1. **The Large and Code tiers are marginal.** On a 16 GiB machine with 10 GiB available, auto
   mode's budget is 5.3 GiB. Qwen3 8B Q4_K_M is estimated at 6.1 GiB and Qwen2.5-Coder 7B
   Q4_K_M at 5.7 GiB: neither fits in auto mode; both fit in performance mode (budget 6.8 GiB)
   or once about 11–11.5 GiB are available. The estimate is conservative, so real behaviour may be
   better — but until it is measured, the honest position is that a 16 GB laptop with a
   browser open gets the Standard tier, and the coder falls back to 1.5B.
2. **"Available" on a real desktop is less than people expect.** The development machine for
   0.1 (16 GB) showed 3.4–3.9 GiB available with elevated pressure during ordinary use, which
   yields a budget of about 0.5 GiB: only tools answer, by design. Whether that is the right
   trade-off or too timid needs real-world tuning of the reserve and the shares.
3. **A 1.5B coder is weak for real programming work.** If 7B does not fit and 3B is not
   licensable, the code tier on 16 GB may be unsatisfying. Options: a permissively licensed
   3–4B code model if one appears, Q3 quantisations of 7B (lower quality), or using the general
   4B model for code.
4. **CPU speed.** A 4B model at Q4 on a laptop CPU without a GPU is usable but not fast; an 8B
   model is slow. No numbers yet.
5. **Context length costs memory.** 4096 tokens is small for coding; 16k on a 7B model adds
   gigabytes.
6. **Swapping the general and code models** means a reload each time (seconds, from page cache
   if the file is cached, much longer from a slow disk).

## Benchmarks per hardware

Not built. The plan: `lumbre-models` records, per machine and per variant, load time, peak
resident memory, time to first token and tokens per second from real requests (the runtime
already measures the last three), stores them next to the model, and uses them instead of the
estimate. Results only ever come from the user's own machine; none are shipped as claims.
