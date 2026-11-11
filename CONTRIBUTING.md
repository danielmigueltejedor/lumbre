# Contributing

Thank you for looking. Lumbre is small and young; that is the best time to get the habits right.

## Ground rules

- **Nothing is described as working unless it has been run.** If a change cannot be verified on
  your machine (a Linux-only path on macOS, a model you do not have), say so in the pull request
  and let CI or a reviewer verify it. Documentation states what is verified and what is not.
- **No simulated behaviour in shipped code.** Stand-ins live in tests. The interface shows what
  the backend reports, including "I cannot do that, because…".
- **No empty scaffolding.** A crate, module or directory appears together with the code that
  needs it.
- **Changes come with tests**, sized to the change.
- **The network is opt-in.** Code that makes an outbound request needs an explicit user action
  and says where it connects.
- **Weights never go in Git.** Model manifests do, with licence and provenance.

## Working on it

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test
benchmarks/idle.sh        # if you touched the daemon
```

Changes go through pull requests against `main`; keep them small enough to review in one
sitting. Commit messages are in English, imperative, and say why when it is not obvious.

Code is commented where a reader would otherwise have to guess: why a threshold has its value,
why an `unsafe` block is sound, what a heuristic gets wrong.

## Adding a model manifest

Read [docs/models.md](docs/models.md). A built-in manifest needs: a licence that permits
redistribution and commercial use, no access gate, a pinned revision, and size and SHA-256 taken
from the publisher. Say in the pull request where each value came from.

## Licence

By contributing you agree that your contribution is licensed under the Apache License 2.0, as
the rest of the project.
