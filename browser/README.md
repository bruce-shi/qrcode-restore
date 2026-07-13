# QR Restore Browser

This package runs QR Restore locally in a modern browser. React and TypeScript
own the UI state and component lifecycle, while browser-specific TypeScript is
limited to file decoding, the Web Worker bridge, and artifact rendering.
Rust compiled to WebAssembly owns the recovery session: effort budgets, variant
scheduling, image processing, localization, geometry hypotheses, BCH metadata,
module traversal, Reed-Solomon correction, reconstruction, verification,
ranking, fusion, ambiguity, and timeout decisions.

Generic Lanczos-3/nearest resize, fixed threshold, and inversion operations use
[`photon-rs`](https://github.com/silvia-odwyer/photon) inside the existing QR
Restore WASM module. Calibrated Lanczos-4, contrast, gamma, unsharp, Otsu,
local/Gaussian thresholds, and frame fusion are Rust kernels in the same module.
RXing's Rust port of the ZXing-C++ QR detector and `rqrr` provide independent
geometry and verification paths. Intermediate variants stay in WASM memory;
only progress, results, and selected diagnostics cross into JavaScript.

The React entrypoint is `src/main.tsx`, with the recovery workflow implemented
as typed components and hooks in `src/App.tsx`. React never performs QR image
processing; it sends each recovery request to the Rust/WASM worker boundary.

## Run

From this directory:

```bash
npm install
npm run dev
```

Or from the repository root:

```bash
npm --prefix browser install
npm --prefix browser run dev
```

The `predev` script builds the Rust crate with `wasm-pack`. For a deployable
static bundle:

```bash
npm --prefix browser run build
```

Serve `browser/dist/` with any static server. Both the QR engine and its WASM
files are self-hosted in the bundle; recovery does not upload input images.

## Deploy to Cloudflare Workers

The browser is configured as a Cloudflare Workers Static Assets application.
Wrangler uploads the Vite `dist/` directory directly; no server-side recovery
code or external asset CDN is involved.

Preview it through the local Workers runtime:

```bash
npm --prefix browser run cf:dev
```

Validate the Worker bundle without publishing:

```bash
npm --prefix browser run deploy:dry-run
```

For a real deployment, authenticate once and deploy:

```bash
cd browser
npx wrangler whoami
npm run deploy
```

By default Wrangler publishes to the `qr-restore-browser` Workers project and
enables its `workers.dev` address. Change `name` or add custom-domain routes in
[`wrangler.jsonc`](wrangler.jsonc) when needed.

The UI includes a **Load synthetic demo sample** action. Its blurred and
deformed fixtures are generated from project-owned demo URLs by the checked-in
Rust generator:

```bash
cargo run -p qr-restore-wasm --example generate_samples
```

No personal or third-party QR image is bundled with the application.

Recovery caches shared grayscale/contrast/resize bases inside WASM instead of
rebuilding them for every threshold recipe. The faster RXing localizer runs
first, while `rqrr` remains the localization fallback and independently verifies
every reconstructed matrix. In the single-worker path, search stops only after
one verified payload is confirmed by multiple distinct variants and an
additional effort-dependent ambiguity window has found no competitor; reports
identify this as `termination_reason: "confidence_limit"`. Multi-image searches
do not use this shortcut because every frame must be checked for conflicting
symbols.

Single-image recovery uses a bounded pool of browser Web Workers: up to two
batches for Fast and up to four for Balanced or Thorough, further capped by
`navigator.hardwareConcurrency`. Rust distributes the prioritized variants
round-robin so batches are disjoint and complete. Each worker owns an isolated
WASM instance. A batch returns as soon as one variant produces a structurally
and Reed-Solomon-verified decoded result; the coordinator immediately terminates
the remaining workers and displays that winner. If no batch returns `decoded`,
the coordinator waits for all partials and calls the Rust merger to preserve
ambiguity and negative-result handling. Multi-image recovery remains in one
worker so frame-conflict checks and image fusion share one state. Cloudflare
Workers only serves the static assets—the parallel computation still runs
privately in the visitor's browser.

The **Parallel batches** UI control can override Auto with 1, 2, 3, or 4
batches. Explicit values take precedence over the reported core count, with
four remaining the hard browser limit. Choosing 1 is useful on memory-limited
devices or when comparing deterministic timings. Multi-image requests always
use one batch regardless of this setting.

The prioritized scheduler uses 20 Fast, 102 Balanced, or 262 Thorough variants.
It covers every enabled channel/scale with contrast, Otsu, and a representative
adaptive threshold before spending time on focused sharpening, extra threshold,
gamma, or nearest-neighbor recipes.

A Rust preflight samples at most 65,536 pixels before the search. Conservative
achromatic detection removes duplicate RGB/minimum-channel work, reducing the
usual black-and-white plans to 17 Fast, 45 Balanced, or 99 Thorough variants.
For colored images the full plan remains available, but Otsu separation ranks
the most informative channel first. Detected finder spacing also selects the
upscale nearest six output pixels per QR module, with image dimensions used as
a fallback when geometry is not yet detectable.

Thorough is a strict extension of Balanced: each worker runs its exact Balanced
slice first with Balanced soft-decoding limits, followed by Thorough-only
variants. Stronger unsharp settings use distinct variant identities instead of
silently replacing their Balanced counterparts.

Release builds use Rust `opt-level = 3`, LTO, and `wasm-opt -O3`.

Photon, RXing, `rqrr`, and the QR-specific kernels are linked into one
self-hosted WASM module. This favors one Rust-owned execution boundary over the
smallest possible bundle.

## Validation

```bash
npm --prefix browser test
npm --prefix browser run typecheck
cargo test -p qr-restore-wasm
cargo clippy -p qr-restore-wasm --all-targets -- -D warnings
```

Rust golden-matrix hashes cover all 160 version/error-correction combinations.
The Reed-Solomon suite covers hard errors, erasures, and bounded Chase repair.

## Recovery scope

The browser implements the high-value recovery path used by the generated
`examples/synthetic-blurred.png` fixture in Rust/WASM: Lanczos-4 upscaling,
color-channel observations, Otsu and
Gaussian/local thresholds, unsharp and gamma variants, timing-pattern and
ZXing-C++-port geometry, BCH format ranking, exact codeword reconstruction,
multi-frame mean evidence, independent clean-matrix verification, and explicit
ambiguity handling. Smoothly deformed symbols add anisotropic/submodule grid
hypotheses, module-likelihood agreement, and deterministic data-tail repair;
payload bits are never synthesized.

The Rust core contains hard, erasure, and bounded Chase correction. It never
presents a candidate unless every reconstructed block is valid and the clean
matrix decodes independently. A calibrated score gap decides between a unique
result and `ambiguous`.

Registered subpixel multi-frame super-resolution and Wiener/Richardson-Lucy
deconvolution are not currently implemented.
