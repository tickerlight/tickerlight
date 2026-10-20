#!/usr/bin/env bash
# Builds the WebAssembly module into www/pkg. Needs: rustup target add wasm32-unknown-unknown,
# and wasm-bindgen-cli of the same version as the wasm-bindgen crate (cargo install wasm-bindgen-cli).
set -euo pipefail
cd "$(dirname "$0")/../.."
profile="${1:-web}"
case "$profile" in
  debug) flag="" ;;
  *) flag="--profile $profile"; [ "$profile" = release ] && flag="--release" ;;
esac
cargo build -p tickerlight-web --target wasm32-unknown-unknown $flag
wasm-bindgen --target web --out-dir crates/web/www/pkg "target/wasm32-unknown-unknown/$profile/tickerlight_web.wasm"
echo "built: serve crates/web/www (for example: python3 -m http.server -d crates/web/www 8000)"
