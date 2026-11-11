# Configuration

Lumbre works without a configuration file. To change something, create `config.toml` in the
configuration directory:

| Platform | Configuration | Data (models, conversations, token) |
| --- | --- | --- |
| Linux | `$XDG_CONFIG_HOME/lumbre` (default `~/.config/lumbre`) | `$XDG_DATA_HOME/lumbre` (default `~/.local/share/lumbre`) |
| macOS | `~/Library/Application Support/Lumbre` | same |
| Windows | `%APPDATA%\Lumbre` | `%LOCALAPPDATA%\Lumbre` |

Setting `LUMBRE_HOME` puts everything in one directory instead.

```toml
# Every key is optional; these are the defaults.
mode = "auto"            # "auto" | "performance" | "saver"
# max_ram_mb = 6000      # hard ceiling for model memory
# reserve_mb = 2048      # memory always left to other applications (default: 15 % of RAM, min 2048)
idle_unload_secs = 300   # unload a model nobody used for this long
context_tokens = 4096    # context window models are loaded with
max_tokens = 1024        # upper bound on the length of an answer
gpu_layers = 0           # model layers to offload to a GPU; 0 is CPU only
# llama_server = "/usr/bin/llama-server"   # when it is not on PATH
search_roots = []        # folders file search may look into; empty turns it off
port = 47110             # loopback port of the local API
```

An unknown key is an error, so a typo does not silently do nothing.

Environment variables:

| Variable | Effect |
| --- | --- |
| `LUMBRE_HOME` | One directory for configuration and data |
| `LUMBRE_ADDR` | Address clients connect to (default: the one the daemon recorded, else `127.0.0.1:47110`) |
| `LUMBRE_LLAMA_SERVER` | Path to `llama-server` |
| `LUMBRE_LOG` | Log filter for the daemon, e.g. `debug` or `lumbre_core=trace` |

Other files:

- `models/*.toml` in the configuration directory — your own model manifests
  ([format](models.md#manifest-format)).
- `code-permissions.toml` in the configuration directory — what `lumbre-code` may do in each
  repository. Edit it with `lumbre-code allow` and `lumbre-code revoke`.

The daemon logs to stderr, as text or — with `--log-json` — as one JSON object per line:
routing decisions, loads and unloads with their reasons, generation timings, downloads.
