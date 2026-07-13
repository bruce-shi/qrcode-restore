/// <reference lib="webworker" />

import initQrCore, { RecoveryEngine } from "./wasm/pkg/qr_restore_wasm.js";
import type { BrowserRecoveryOptions } from "./types";

interface BatchObservation {
  width: number;
  height: number;
  data: Uint8Array;
}

interface BatchRequest {
  kind: "recover-batch";
  observations: BatchObservation[];
  options: BrowserRecoveryOptions & {
    batchIndex: number;
    batchCount: number;
  };
}

type BatchResponse =
  | {
      kind: "progress";
      stage: string;
      completed: number;
      total: number;
      detail: string;
    }
  | { kind: "result"; result: unknown }
  | { kind: "error"; message: string };

const context = self as unknown as DedicatedWorkerGlobalScope;

function post(message: BatchResponse): void {
  context.postMessage(message);
}

context.addEventListener("message", (event: MessageEvent<BatchRequest>) => {
  if (event.data.kind !== "recover-batch") return;
  void initQrCore()
    .then(() => {
      const engine = new RecoveryEngine(event.data.options);
      try {
        for (const observation of event.data.observations) {
          engine.add_observation(observation.data, observation.width, observation.height);
        }
        return engine.recover(
          (stage: string, completed: number, total: number, detail: string) => {
            post({ kind: "progress", stage, completed, total, detail });
          },
        );
      } finally {
        engine.free();
      }
    })
    .then((result) => post({ kind: "result", result }))
    .catch((error: unknown) => {
      post({ kind: "error", message: error instanceof Error ? error.message : String(error) });
    });
});
