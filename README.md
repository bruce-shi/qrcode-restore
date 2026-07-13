# QR Restore

[**Open QR Restore → qrcode.toolbox.icu**](https://qrcode.toolbox.icu/)

QR Restore is a local, CPU-based browser tool for recovering blurred,
low-resolution, and moderately deformed QR Code Model 2 symbols. The recovery
engine is written in Rust and compiled to WebAssembly; React and TypeScript own
only the browser UI, file decoding, worker coordination, and artifact download.

Images never leave the browser. Recovery combines a deterministic image
variant ensemble with QR geometry, confidence-aware module sampling, BCH
metadata, Reed-Solomon correction, matrix reconstruction, and independent
verification. Results are explicit: `decoded`, `ambiguous`, or `unrecoverable`.

## Repository layout

- `crates/qr-restore-wasm/` — Rust image processing and QR recovery engine
- `browser/` — React UI, Web Worker pool, WASM bridge, and Cloudflare config
- `examples/` — generated synthetic blurred and deformed fixtures
- `crates/qr-restore-wasm/examples/generate_samples.rs` — fixture generator
- `docs/algorithm.md` — recovery stages, effort budgets, and limits

## Run locally

Install Rust with the `wasm32-unknown-unknown` target, `wasm-pack`, and a current
Node.js/npm runtime. Then run:

```bash
npm --prefix browser install
npm --prefix browser run dev
```

The browser can load local QR images or the bundled synthetic demo. A
successful result displays the restored matrix and working image variant and
can download `restored.png` and `report.json`.

Regenerate the public demo fixtures at any time without using personal QR data:

```bash
cargo run -p qr-restore-wasm --example generate_samples
```

## Build and verify

```bash
npm --prefix browser test
npm --prefix browser run typecheck
npm --prefix browser run build
cargo test -p qr-restore-wasm --lib
cargo clippy -p qr-restore-wasm --all-targets -- -D warnings
```

The production command compiles the Rust crate to an optimized self-hosted WASM
module and writes the static application to `browser/dist/`.

## Cloudflare Workers

Cloudflare Workers serves only the static application. All image processing
continues to run locally in the visitor's browser.

```bash
npm --prefix browser run deploy:dry-run
npm --prefix browser run deploy
```

See [browser/README.md](browser/README.md) for browser architecture and
deployment details, and [docs/algorithm.md](docs/algorithm.md) for the recovery
model and theoretical limits.
