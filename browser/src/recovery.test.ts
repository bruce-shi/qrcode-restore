import { readFileSync } from "node:fs";
import { beforeAll, describe, expect, it } from "vitest";
import {
  RecoveryEngine,
  initSync,
  merge_recovery_results,
} from "./wasm/pkg/qr_restore_wasm.js";

beforeAll(() => {
  const bytes = readFileSync(
    new URL("./wasm/pkg/qr_restore_wasm_bg.wasm", import.meta.url),
  );
  initSync({ module: new WebAssembly.Module(bytes) });
});

describe("Rust recovery engine", () => {
  it("owns the effort search and returns an explicit negative result", () => {
    const engine = new RecoveryEngine({ effort: "fast", maxSeconds: 2 });
    const blank = new Uint8Array(21 * 21 * 4);
    for (let index = 0; index < blank.length; index += 4) {
      blank[index] = 255;
      blank[index + 1] = 255;
      blank[index + 2] = 255;
      blank[index + 3] = 255;
    }
    engine.add_observation(blank, 21, 21);
    const progress: string[] = [];
    const result = engine.recover((stage: string) => progress.push(stage)) as {
      status: string;
      candidates: unknown[];
      diagnostics: { examinedVariants: number; runtime: string };
    };
    engine.free();

    expect(result.status).toBe("unrecoverable");
    expect(result.candidates).toEqual([]);
    expect(result.diagnostics.examinedVariants).toBe(17);
    expect(result.diagnostics.runtime).toBe("rust-wasm+photon+rxing+rqrr");
    expect(progress).toContain("preflight");
    expect(progress).toContain("searching");
  });

  it("merges disjoint parallel batches inside Rust", () => {
    const blank = new Uint8Array(21 * 21 * 4);
    for (let index = 0; index < blank.length; index += 4) {
      blank.fill(255, index, index + 4);
    }
    const partials = [0, 1].map((batchIndex) => {
      const engine = new RecoveryEngine({
        effort: "fast",
        maxSeconds: 2,
        batchIndex,
        batchCount: 2,
      });
      try {
        engine.add_observation(blank, 21, 21);
        return engine.recover(() => undefined);
      } finally {
        engine.free();
      }
    });
    const merged = merge_recovery_results(partials) as {
      status: string;
      candidates: unknown[];
      diagnostics: { examinedVariants: number };
    };
    expect(merged.status).toBe("unrecoverable");
    expect(merged.candidates).toEqual([]);
    expect(merged.diagnostics.examinedVariants).toBe(17);
  });
});
