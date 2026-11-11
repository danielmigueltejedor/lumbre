# Lumbre Código IDE: plan

Status: **plan only.** Nothing here is built. The `lumbre-code` command line exists and works;
this document is about the graphical editor that should follow it.

This is an engineering reading of public licence texts and terms, not legal advice. Anything
marked *verify* must be re-checked against the current upstream text before work starts.

## The idea

An editor based on **Code - OSS** — the MIT-licensed source of Visual Studio Code — with Nodalia's
identity and Lumbre built in as its assistant, talking to the same local daemon as everything
else. No fork is checked into this repository, and none should be: the editor lives in its own
repository as a thin layer over upstream.

## Licences, trademarks, redistribution

| Thing | Terms | Consequence for Lumbre |
| --- | --- | --- |
| `microsoft/vscode` source ("Code - OSS") | MIT | Can be built, modified and redistributed, keeping the copyright and licence notices |
| The **Visual Studio Code** product (Microsoft's binaries) | Microsoft's own product licence, not MIT | Never redistributed. Lumbre builds from source |
| Names and logos: "Visual Studio Code", "VS Code", the icon | Microsoft trademarks | Not used for the product, its icon, its binary name or its marketing. Factual statements such as "based on Code - OSS" are how other projects describe this, *verify* wording against Microsoft's trademark guidance |
| **Visual Studio Marketplace** | Terms of use limit it to Microsoft's Visual Studio products | Not used. The gallery is configured to **Open VSX** (Eclipse Foundation), as VSCodium and other derivatives do |
| Microsoft's proprietary extensions (C/C++ debugger components, Pylance, Remote - SSH/Containers/WSL, Live Share, GitHub Copilot) | Licensed for use only with Microsoft's products | Not bundled, not offered, and documented as unavailable. Open alternatives exist for most (clangd, Pyright/basedpyright, Open Remote SSH) |
| Telemetry, crash reporting, update and settings-sync services | Microsoft endpoints configured in `product.json` | All endpoints removed or left empty; telemetry off at build time. Fits Lumbre's privacy stance anyway |
| Bundled third-party components (Electron, Chromium, Node.js, ripgrep, built-in extensions) | Their own licences, listed upstream in `ThirdPartyNotices.txt` | The notices file ships with the product; the build regenerates it |
| Built-in extensions in the upstream tree | MIT, in the same repository | Usable. Some depend on proprietary pieces at run time and are dropped, *verify* per release |

The practical precedent is **VSCodium** (MIT build scripts) and the many Code - OSS derivatives:
this path is well trodden, and the constraints above are the same ones they follow.

Open points to settle before distributing anything:

- the exact trademark-safe wording to describe the relationship to VS Code;
- Open VSX's terms of use for a derivative product (*verify*; it is designed for this use);
- whether any default built-in extension pulls a non-free binary at run time in the pinned
  release.

## Architecture

```
lumbre-code-ide (separate repository)
├── upstream.json        pinned Code - OSS tag and commit
├── product.json         name, identifiers, Open VSX gallery, no Microsoft endpoints
├── patches/             a small, numbered patch series (branding hooks, defaults)
├── branding/            icons and names (Nodalia)
├── extensions/lumbre/   the built-in extension: all Lumbre functionality lives here
└── build/               scripts and CI: fetch upstream at the pin, apply, build, package
```

Principles:

1. **No source fork.** The repository contains what differs, not a copy of upstream. A build
   fetches the pinned tag, applies `product.json` and the patch series, adds the extension.
2. **Everything Lumbre does is an extension.** Chat panel, inline suggestions, "apply this diff",
   "run the tests" — implemented against the public extension API, talking to the daemon's
   `/v1` API on loopback. Extension API is the most stable surface upstream has; patches to the
   core are the least stable. The patch series should stay in single digits and be about
   branding and defaults only.
3. **One brain.** The extension does not embed a model or start a server. It is a client of
   `lumbred`, like the desktop app. The permission model of `lumbre-code` (write and exec grants
   per repository, stored outside the repository) is reused, exposed through the daemon so the
   editor and the CLI agree.
4. **Works as a plain extension too.** Because of (2), the same extension can be published to
   Open VSX for people who already use VSCodium or another Code - OSS build. That is likely the
   first deliverable — much cheaper than a branded editor and it validates the integration.

What this needs from Lumbre first: verified inference, the agent loop, and API endpoints for
repository operations (today they are in the `lumbre-code` library, called in-process by the CLI).

## Staying current with upstream

VS Code releases monthly, plus recovery releases with security fixes. A derivative that falls
behind ships known vulnerabilities in Electron and Chromium, so the maintenance plan matters
more than the initial build.

- **Track stable tags only**, never `main`.
- **Automate the bump.** A scheduled CI job notices a new stable tag, updates `upstream.json` on
  a branch, runs the build and the smoke tests, and opens a pull request. A human merges.
- **Budget for patch breakage.** Each patch is small, single-purpose and documented with why it
  exists and what would let it be dropped. When one fails to apply, the job says which.
- **Security releases within days**, feature releases within about two weeks: a stated target,
  to be revised against what one maintainer can sustain.
- **Prefer configuration to patches, and extensions to both.** Before adding a patch, check
  whether `product.json`, a default setting or the extension can do it.
- **Decide what happens if this cannot be sustained:** the fallback is to ship only the
  extension (principle 4) and recommend an existing Code - OSS build. That fallback is a
  feature of the design, not a failure mode to hide.

## Size of the work

For an honest sense of scale: a Code - OSS build is a multi-gigabyte checkout and a long CI
build per platform; packaging for four Linux distributions, plus signing on macOS and Windows
later, is a project of its own. It should not start until the extension has proven the
integration.
