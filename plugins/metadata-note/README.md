# Metadata Note — example Fable Files plugin

A dependency-free WASM plugin for the Phase 6 host ABI
(`crates/ufm-plugins/src/runtime.rs`, contract in `docs/PLUGIN_API.md`).

Invoke it from any file's context menu (Plugins ▸ Metadata Note): it
re-reads the item's metadata through the host, logs what it found, and
**proposes** a `<item name>.fable-note.txt` summarising the item.
Nothing is written unless you confirm the proposal; the write itself
runs as a normal journaled Fable job.

## Build

```sh
rustup target add wasm32-unknown-unknown
cargo build --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/fable_metadata_note.wasm main.wasm
```

`main.wasm` in this folder is the built artifact of `src/lib.rs`
(rustc 1.92.0). The app also embeds this plugin and seeds it into
`~/.local/share/fable-files/plugins/` on first run.
