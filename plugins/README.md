# plugins/

Drop a plugin folder here and it hot-loads — no restart, no reinstall.

Each plugin is a folder containing a `plugin.toml` manifest:

```toml
[plugin]
name = "My Search Provider"
author = "you"
version = "0.1.0"
description = "Adds an extra search provider."
capabilities = ["search.provider"]   # what it plugs into
permissions = []                      # sandbox requests; locked down by default
entry = "main.wasm"                   # executable entry point
```

Capability names (spec §§17–18): `search.provider`, `preview.provider`,
`action.context`, `organize.rule`, `cloud.provider`, `theme.pack`.

Sandbox policy: plugins start **locked down** — read-only, no network, no
subprocesses, 256 MB / 30 CPU-seconds. Extra permissions require explicit
user grants. A plugin that violates its policy is unloaded, never warned twice.

Full contract: `docs/PLUGIN_API.md`. Host implementation: `crates/ufm-plugins`.
