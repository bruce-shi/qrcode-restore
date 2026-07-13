import type { Effort, ParallelBatches } from "./types";

export function resolveBatchCount(
  inputCount: number,
  effort: Effort,
  requested: ParallelBatches | undefined,
  hardwareConcurrency: number,
): number {
  if (inputCount !== 1) return 1;
  if (requested !== undefined) return Math.min(4, Math.max(1, requested));
  const hardware = Math.max(1, Math.floor(hardwareConcurrency || 2));
  const effortLimit = effort === "fast" ? 2 : 4;
  return Math.min(hardware, effortLimit);
}
