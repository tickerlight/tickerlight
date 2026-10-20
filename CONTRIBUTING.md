# Contributing

Bug reports and ideas are welcome as [issues](https://github.com/tickerlight/tickerlight/issues); questions go to
[discussions](https://github.com/tickerlight/tickerlight/discussions). For a change larger than a fix, open an
issue first, so the work is not done twice or in a direction the program won't take.

## Building and checking

[Rust](https://rustup.rs) installs the version in `rust-toolchain.toml` by itself. Then:

```sh
cargo run --release -p tickerlight-app
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The pixel tests draw offscreen and compare with the pictures in the repository. On a machine without a GPU they
need a software Vulkan driver (Mesa's lavapipe: `mesa-vulkan-drivers` on Debian and Ubuntu).

To see a change without a window, the tour writes every step of using the program as PNGs. Give it a scratch
data folder, not the one you use:

```sh
TICKERLIGHT_DATA_DIR=/tmp/tl-data cargo run -p tickerlight-app --example tour -- /tmp/tour/
```

## How the code is built

- `tickerlight-core` holds everything about a chart (data, scales, drawings, indicators, replay, trading,
  strategies) with no GPU and no I/O; the program, the data sources and the drawing backend are around it.
- Nothing may see past the replay cursor: a feature reads the bars and trades the chart gives it, never the
  data behind them.
- A change that shows on screen comes with a test that looks at what is drawn.
- Comments say why, not what.

## Pull requests

One change per pull request, with tests; CI must be green (format, clippy, tests on Linux, Windows and macOS).
Contributions are dual-licensed under MIT and Apache-2.0, as the README says.
