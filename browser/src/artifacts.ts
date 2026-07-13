import type { BrowserRecoveryResult, RecoveryCandidate } from "./types";

function download(blob: Blob, filename: string): void {
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = filename;
  anchor.click();
  setTimeout(() => URL.revokeObjectURL(url), 0);
}

export function renderMatrix(
  canvas: HTMLCanvasElement,
  candidate: RecoveryCandidate,
  modulePixels = 10,
): void {
  const quiet = 4;
  const size = candidate.version * 4 + 17;
  const side = (size + quiet * 2) * modulePixels;
  canvas.width = side;
  canvas.height = side;
  const context = canvas.getContext("2d");
  if (!context) return;
  context.fillStyle = "#fff";
  context.fillRect(0, 0, side, side);
  context.fillStyle = "#090a08";
  for (let y = 0; y < size; y += 1) {
    for (let x = 0; x < size; x += 1) {
      if (candidate.matrix[y * size + x] !== 0) {
        context.fillRect(
          (x + quiet) * modulePixels,
          (y + quiet) * modulePixels,
          modulePixels,
          modulePixels,
        );
      }
    }
  }
}

export function renderPreview(canvas: HTMLCanvasElement, result: BrowserRecoveryResult): void {
  const preview = result.bestVariant;
  if (!preview) return;
  canvas.width = preview.width;
  canvas.height = preview.height;
  const context = canvas.getContext("2d");
  if (!context) return;
  context.putImageData(
    new ImageData(new Uint8ClampedArray(preview.data), preview.width, preview.height),
    0,
    0,
  );
}

function bytesToBase64(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

export function resultReport(result: BrowserRecoveryResult): Record<string, unknown> {
  return {
    status: result.status,
    candidates: result.candidates.map((candidate) => ({
      payload_text: candidate.text,
      payload_base64: bytesToBase64(candidate.payload),
      version: candidate.version,
      ec_level: candidate.ecLevel,
      mask: candidate.mask,
      matrix_kind: candidate.matrixKind,
      corrected_symbols: candidate.correctedSymbols,
      score: candidate.score,
      score_components: candidate.scoreComponents,
      confidence: candidate.confidence,
      source: candidate.source,
      evidence_count: candidate.evidenceCount,
    })),
    diagnostics: result.diagnostics,
    discarded_frames: result.discardedFrames,
    termination_reason: result.terminationReason,
  };
}

export function downloadReport(result: BrowserRecoveryResult): void {
  download(
    new Blob([JSON.stringify(resultReport(result), null, 2)], { type: "application/json" }),
    "report.json",
  );
}

export function downloadCanvas(canvas: HTMLCanvasElement, filename: string): void {
  canvas.toBlob((blob) => {
    if (blob) download(blob, filename);
  }, "image/png");
}
