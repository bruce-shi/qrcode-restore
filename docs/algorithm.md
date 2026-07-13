# Recovery algorithm

## 1. Evidence, not sharpening alone

A blurred pixel is a mixture of nearby QR modules. Sharpening can make that
mixture look cleaner without restoring the missing bits, so QR Restore treats
every preprocessing output as another observation rather than ground truth.

The prioritized Rust ensemble covers inversion, grayscale/color channels,
nearest-neighbor and Lanczos resampling at several scales, contrast,
gamma/unsharp enhancement, and Otsu/local/Gaussian thresholds. RXing and `rqrr`
provide independent localization and standards-decoding paths.

## 2. Geometry and multiple frames

Decoder quadrilaterals or nested `1:1:3:1:1` finder contours supply a homography.
Known finder, timing, alignment, format, version, and dark modules anchor the
module grid. Several images are homography-registered; failed registrations are
reported, while the remaining observations are combined with robust mean and
median evidence. This can recover subpixel information and complementary noise
that is absent from any single frame.

Every rectified module is represented by a hard black/white decision plus a
confidence value. Confidence is the usable form of the module's binary entropy:
values near the photometric threshold carry high entropy and are searched or
marked as erasures before high-confidence modules.

The Rust/WASM engine also has a bounded deformed-symbol path. When the finder
and alignment patterns are detectable but one projective grid is insufficient,
it samples a neighborhood of anisotropic grid scales and submodule offsets.
Candidates must repeat across neighboring geometries and retain at least 82%
confidence-weighted agreement with the image.

## 3. QR constraints and soft decoding

Format data has only 32 valid BCH-protected combinations: four error-correction
levels by eight data masks. Once version, EC level, and mask are known, QR
Restore removes function modules, unmasks the data traversal, and deinterleaves
the shortened Reed-Solomon blocks.

Each block is tried in increasing information cost:

1. hard Reed-Solomon decoding;
2. generalized minimum-distance decoding that progressively erases the least
   reliable codewords;
3. bounded Chase decoding that flips combinations of the least reliable bits.

For `nsym` parity symbols, errors and known erasures must satisfy
`2 * errors + erasures <= nsym`. Every repaired block must have zero syndromes.
The exact interleaved codewords are then placed back into a fresh deterministic
function matrix and independently decoded by ZXing.

For a deformed symbol whose complete data segment survives but whose trailing
padding and parity modules do not, the Rust engine may rebuild only the
deterministic terminator, pad sequence, and Reed-Solomon parity. It never fills
missing payload bits. The rebuilt matrix must independently decode with the
same version, EC level, and mask before it can become a candidate.

## 4. Candidate ranking and confidence

Candidates are ranked by decoder validity, agreement between their modules and
the rectified image, correction/search cost, agreement across independent
frames or transformations, and optional payload hints. Hints are priors only;
they cannot make an invalid QR valid.

`high` means a unique standard-decoder result or agreement across independent
observations. Soft-only recoveries start at `medium`. Multiple structurally
valid candidates without a clear score gap are returned as `ambiguous` and
labelled `low` rather than silently selecting one.

## 5. Effort budgets and limits

| Mode | Image variants | Geometry hypotheses | Chase bits | Candidate attempts | Default time |
| --- | ---: | ---: | ---: | ---: | ---: |
| fast | 20 | 1 | 0 | 2,000 | 10 s |
| balanced | 102 | 4 | 12 | 50,000 | 60 s |
| thorough | 262 | 12 | 18 | 500,000 | 600 s |

The browser plan is ordered by information gain rather than as a full Cartesian
product. Contrast, Otsu, and one representative adaptive threshold cover every
enabled channel and scale first. Unsharp, additional threshold windows, gamma,
and nearest-neighbor recipes then run on focused luma/color subsets. Balanced
mode stops after two independent verified reads plus a six-variant ambiguity
window; Thorough requires three reads and a twelve-variant window.

Before rendering variants, Rust samples at most 65,536 source pixels. Images
with very low RGB chroma are classified conservatively as achromatic, so their
red, green, blue, and minimum-channel recipes are identical evidence and are
removed. This reduces the usual grayscale plans to 17 Fast, 45 Balanced, or 99
Thorough variants. Colored or uncertain images retain the complete plan, with
channels ranked by Otsu separation. A finder-pattern estimate of pixels per
module selects the scale nearest six output pixels per module; when no finder
geometry is available, image dimensions provide a conservative scale prior.
This changes search order and removes only demonstrably redundant grayscale
channels; it does not accept or validate a candidate.

Thorough is monotonic over Balanced: it begins with the complete, identically
ordered Balanced phase using Balanced soft-decoding limits, then adds its
larger-scale, stronger-sharpening, and expanded Chase searches. Therefore a
Balanced-valid transform is not replaced by a differently tuned Thorough
transform. Thorough may still return `ambiguous` if its extra verified evidence
reveals a genuine second candidate.

For one browser observation, the prioritized plan is divided round-robin into
disjoint Web Worker batches. Fast uses at most two workers; Balanced and
Thorough use at most four, bounded further by the device's reported hardware
concurrency. Each worker runs Rust/WASM independently and stops at its first
verified candidate. The browser accepts the first batch whose local result is
`decoded`, terminates the remaining workers, and ignores their late messages.
Ambiguous and unrecoverable partials do not trigger early success; if no worker
decodes, Rust merges every partial, recomputes ranking and ambiguity, sums work
diagnostics, and selects the best preview. Multi-image requests stay sequential
because conflict detection and evidence fusion require shared per-frame state.
This is browser-side parallelism; the Cloudflare Worker deployment is static
hosting and never receives the source image.

The browser exposes Auto or an explicit 1–4 batch setting. Auto respects
hardware concurrency and effort defaults; an explicit value overrides those
defaults but remains capped at four isolated WASM workers.

The tool supports QR Code Model 2 versions 1–40 and bounded smooth deformation
when finder/alignment geometry and the complete payload segment remain
observable. Micro QR, rMQR, arbitrary folds, severe cylindrical wraps, and
video ingestion remain outside the current scope.

No algorithm can recover information that was destroyed beyond the symbol's
error-correction capacity and any supplied priors. Reed-Solomon validity is not
a cryptographic signature. Learned/GAN super-resolution is therefore excluded
from the trusted path: a network can hallucinate visually plausible modules and
produce an unrelated but valid-looking payload. A learned preprocessor may be
added later only as another low-trust observation subjected to all structural
and forward-image checks.
