export type Effort = "fast" | "balanced" | "thorough";
export type ParallelBatches = 1 | 2 | 3 | 4;
export type ParallelBatchSetting = "auto" | ParallelBatches;
export type RecoveryStatus = "decoded" | "ambiguous" | "unrecoverable";
export type Confidence = "high" | "medium" | "low";

export interface BrowserRecoveryOptions {
  effort: Effort;
  parallelBatches?: ParallelBatches;
  maxSeconds?: number;
  version?: number;
  ecLevel?: "L" | "M" | "Q" | "H";
  payloadPrefix?: string;
  payloadRegex?: string;
  expectedText?: string;
  fallbackEncoding?: string;
}

export interface RecoveryCandidate {
  payload: Uint8Array;
  text: string;
  version: number;
  ecLevel: string;
  mask: number;
  matrix: Uint8Array;
  matrixKind: "exact" | "equivalent";
  correctedSymbols: number;
  score: number;
  scoreComponents: Record<string, number>;
  confidence: Confidence;
  source: string;
  evidenceCount: number;
}

export interface RecoveryDiagnostics {
  examinedVariants: number;
  validReads: number;
  invalidReads: number;
  softDecodeAttempts: number;
  elapsedSeconds: number;
  inputCount: number;
  runtime: "rust-wasm+photon+rxing+rqrr";
}

export interface PreviewImage {
  width: number;
  height: number;
  data: Uint8ClampedArray;
}

export interface BrowserRecoveryResult {
  status: RecoveryStatus;
  candidates: RecoveryCandidate[];
  diagnostics: RecoveryDiagnostics;
  discardedFrames: string[];
  terminationReason: "time_limit" | "variant_limit" | "confidence_limit" | null;
  bestVariant?: PreviewImage;
  bestVariantName?: string;
}

export interface RecoverRequest {
  kind: "recover";
  files: File[];
  options: BrowserRecoveryOptions;
}

export type WorkerRequest = RecoverRequest;

export type WorkerResponse =
  | {
      kind: "progress";
      stage: string;
      completed: number;
      total: number;
      detail: string;
    }
  | { kind: "result"; result: BrowserRecoveryResult }
  | { kind: "error"; message: string };
