#!/usr/bin/env bash
# Scaffold a new domain crate in the workspace with the standard wiring.
# Usage: scripts/new-crate.sh ufm-sync
set -euo pipefail
NAME="${1:?usage: new-crate.sh <crate-name>}"
DIR="crates/$NAME"
mkdir -p "$DIR/src"
cat > "$DIR/Cargo.toml" <<EOF
[package]
name = "$NAME"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
ufm-core.workspace = true
serde.workspace = true
EOF
cat > "$DIR/src/lib.rs" <<EOF
//! $NAME: one-line purpose (spec section reference).
EOF
echo "created $DIR — now add \"$DIR\" to workspace members in Cargo.toml"
