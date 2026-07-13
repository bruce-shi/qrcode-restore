/// <reference lib="webworker" />

import initQrCore, {
  RecoveryEngine,
  merge_recovery_results,
} from "./wasm/pkg/qr_restore_wasm.js";
import { raceForSuccess } from "./batch-race";
import { fileToPixels } from "./image-io";
import type { PixelImage } from "./image-io";
import { resolveBatchCount } from "./parallelism";
import type {
  BrowserRecoveryOptions,
  BrowserRecoveryResult,
  RecoveryCandidate,
  WorkerRequest,
  WorkerResponse,
} from "./types";

interface RustCandidate extends Omit<RecoveryCandidate, "payload" | "matrix"> {
  payload: Uint8Array | number[];
  matrix: Uint8Array | number[];
}

interface RustResult extends Omit<BrowserRecoveryResult, "candidates" | "bestVariant"> {
  candidates: RustCandidate[];
  bestVariant?: {
    width: number;
    height: number;
    data: Uint8Array | number[];
  } | null;
}

const context = self as unknown as DedicatedWorkerGlobalScope;
let initialized: Promise<void> | undefined;

interface BatchWorkerResponse {
  kind: "progress" | "result" | "error";
  stage?: string;
  completed?: number;
  total?: number;
  detail?: string;
  result?: unknown;
  message?: string;
}

function initialize(): Promise<void> {
  initialized ??= initQrCore().then(() => undefined);
  return initialized;
}

function post(message: WorkerResponse): void {
  context.postMessage(message);
}

function normalizeResult(result: RustResult): BrowserRecoveryResult {
  return {
    ...result,
    candidates: result.candidates.map((candidate) => ({
      ...candidate,
      payload: Uint8Array.from(candidate.payload),
      matrix: Uint8Array.from(candidate.matrix),
    })),
    bestVariant: result.bestVariant
      ? {
          width: result.bestVariant.width,
          height: result.bestVariant.height,
          data: Uint8ClampedArray.from(result.bestVariant.data),
        }
      : undefined,
  };
}

function batchCountFor(files: File[], options: BrowserRecoveryOptions): number {
  return resolveBatchCount(
    files.length,
    options.effort,
    options.parallelBatches,
    navigator.hardwareConcurrency || 2,
  );
}

function recoverSequential(
  observations: PixelImage[],
  options: BrowserRecoveryOptions,
): RustResult {
  const engine = new RecoveryEngine(options);
  try {
    for (const observation of observations) {
      engine.add_observation(
        Uint8Array.from(observation.data),
        observation.width,
        observation.height,
      );
    }
    return engine.recover(
      (stage: string, completed: number, total: number, detail: string) => {
        post({ kind: "progress", stage, completed, total, detail });
      },
    ) as RustResult;
  } finally {
    engine.free();
  }
}

async function recoverParallel(
  observations: PixelImage[],
  options: BrowserRecoveryOptions,
  batchCount: number,
): Promise<RustResult> {
  const workers: Worker[] = [];
  const progress = Array.from({ length: batchCount }, () => ({ completed: 0, total: 0 }));
  post({
    kind: "progress",
    stage: "parallel",
    completed: 0,
    total: batchCount,
    detail: `Starting ${batchCount} recovery batches`,
  });

  const runBatch = (batchIndex: number): Promise<RustResult> =>
    new Promise((resolve, reject) => {
      const worker = new Worker(new URL("./variant-batch.worker.ts", import.meta.url), {
        type: "module",
      });
      workers.push(worker);
      worker.addEventListener("message", (event: MessageEvent<BatchWorkerResponse>) => {
        const message = event.data;
        if (message.kind === "progress") {
          progress[batchIndex] = {
            completed: message.completed ?? 0,
            total: message.total ?? 0,
          };
          const completed = progress.reduce((sum, value) => sum + value.completed, 0);
          const total = progress.reduce((sum, value) => sum + value.total, 0);
          post({
            kind: "progress",
            stage: "parallel",
            completed,
            total: total || batchCount,
            detail: `Batch ${batchIndex + 1}/${batchCount} · ${message.detail ?? message.stage ?? "searching"}`,
          });
        } else if (message.kind === "result") {
          const batchProgress = progress[batchIndex];
          if (batchProgress) batchProgress.completed = batchProgress.total;
          resolve(message.result as RustResult);
        } else {
          reject(new Error(`Batch ${batchIndex + 1}: ${message.message ?? "recovery failed"}`));
        }
      });
      worker.addEventListener("error", (event) => {
        reject(new Error(`Batch ${batchIndex + 1} worker failed: ${event.message}`));
      });

      const batchObservations = observations.map((observation) => ({
        width: observation.width,
        height: observation.height,
        data: Uint8Array.from(observation.data),
      }));
      worker.postMessage(
        {
          kind: "recover-batch",
          observations: batchObservations,
          options: { ...options, batchIndex, batchCount },
        },
        batchObservations.map((observation) => observation.data.buffer as ArrayBuffer),
      );
    });

  try {
    const race = await raceForSuccess(
      Array.from({ length: batchCount }, (_, batchIndex) => runBatch(batchIndex)),
      (result) => result.status === "decoded",
    );
    if (race.winner) {
      post({
        kind: "progress",
        stage: "complete",
        completed: 1,
        total: 1,
        detail: `Verified ${race.winner.bestVariantName ?? "QR variant"}; stopped remaining batches`,
      });
      return race.winner;
    }
    post({
      kind: "progress",
      stage: "merging",
      completed: batchCount,
      total: batchCount,
      detail: "Merging verified batch evidence in Rust",
    });
    return merge_recovery_results(race.results) as RustResult;
  } finally {
    for (const worker of workers) worker.terminate();
  }
}

async function recover(
  files: File[],
  options: BrowserRecoveryOptions,
): Promise<BrowserRecoveryResult> {
  await initialize();
  post({
    kind: "progress",
    stage: "loading",
    completed: 0,
    total: files.length,
    detail: "Reading images",
  });
  const observations: PixelImage[] = [];
  for (let index = 0; index < files.length; index += 1) {
    const file = files[index];
    if (!file) continue;
    observations.push(await fileToPixels(file));
    post({
      kind: "progress",
      stage: "loading",
      completed: index + 1,
      total: files.length,
      detail: file.name,
    });
  }
  const batchCount = batchCountFor(files, options);
  const result =
    batchCount === 1
      ? recoverSequential(observations, options)
      : await recoverParallel(observations, options, batchCount);
  return normalizeResult(result);
}

context.addEventListener("message", (event: MessageEvent<WorkerRequest>) => {
  if (event.data.kind !== "recover") return;
  recover(event.data.files, event.data.options)
    .then((result) => post({ kind: "result", result }))
    .catch((error: unknown) => {
      post({ kind: "error", message: error instanceof Error ? error.message : String(error) });
    });
});
