# Security

Lumbre is pre-release software maintained by one person. Only the latest commit on `main` is
supported.

## Reporting

Please report vulnerabilities privately through GitHub's
[security advisories](https://github.com/danielmigueltejedor/lumbre/security/advisories/new)
rather than a public issue. Expect an acknowledgement within a week.

## What Lumbre defends against

- **Other machines.** The API binds loopback only and the daemon refuses any other address.
- **Other local accounts.** Requests need a bearer token kept in a file with owner-only
  permissions.
- **Tampered downloads.** Model files are fetched from a pinned revision and verified by size
  and SHA-256 from the manifest before they are installed.
- **A hostile repository** opened with `lumbre-code`: no writes and no execution without a grant
  stored outside the repository; diffs cannot touch paths outside it (including through
  symlinks); commands run without a shell and are matched on their real argument vector.

## What it does not defend against

- A process running as your own user: it can read the token, the conversations and the models.
- A malicious model file whose digest matches a manifest you chose to trust, or a vulnerability
  in llama.cpp's model parser. The worker process contains the damage only as far as an
  unsandboxed process of your user can be contained.
- Prompt injection: text from a file or a web page can influence a model's output. This is why
  tools and `lumbre-code` are permissioned in code rather than by instruction, and why the agent
  loop planned for 0.2 keeps a human review step before anything is written or run.
