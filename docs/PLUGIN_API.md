# Plugin API (v0.1 contract)

Plugins extend the app without forking it. Host: `crates/ufm-plugins`.

## Lifecycle

1. Drop a folder with `plugin.toml` into `plugins/` → host validates the
   manifest → sandbox policy assigned (locked down by default) → capabilities
   registered with the broker.
2. Edit `plugin.toml` → hot reload: re-validate, swap, no restart.
3. Delete the folder (or unload in settings) → capabilities unregistered.

One bad manifest never takes down the host; violations of the sandbox policy
unload the plugin.

## Capabilities

| Name | What it plugs into | Example |
|---|---|---|
| `search.provider` | the "find by thought" box | "super mario" also downloads via a ROM plugin |
| `preview.provider` | spacebar Quick Look | DWG thumbnails |
| `action.context` | right-click / quick-action bar | "send to transcoder" |
| `organize.rule` | Hazel-style rule folders | auto-file camera imports by date |
| `cloud.provider` | locations | a new storage backend |
| `theme.pack` | theme gallery | community themes |

A plugin may only invoke capabilities its manifest declares.

## Sandbox

Default policy: read-only, no network, no subprocesses, 256 MB RAM,
30 CPU-seconds per call. More requires explicit user grants in settings.
Filesystem access is limited to declared path prefixes.

## Writing a plugin

See `plugins/README.md` for the manifest shape. Plugin entry points are
WASM modules (host ABI to be published with the first plugin milestone).
Until then, first-party capabilities ship as native crates behind the same
broker traits so the contract is proven before third parties depend on it.
