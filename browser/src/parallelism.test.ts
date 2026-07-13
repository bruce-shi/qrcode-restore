import { describe, expect, it } from "vitest";
import { resolveBatchCount } from "./parallelism";

describe("parallel batch selection", () => {
  it("uses hardware-aware effort defaults in auto mode", () => {
    expect(resolveBatchCount(1, "fast", undefined, 8)).toBe(2);
    expect(resolveBatchCount(1, "balanced", undefined, 8)).toBe(4);
    expect(resolveBatchCount(1, "thorough", undefined, 2)).toBe(2);
  });

  it("allows an explicit bounded batch count", () => {
    expect(resolveBatchCount(1, "balanced", 3, 1)).toBe(3);
    expect(resolveBatchCount(1, "fast", 4, 8)).toBe(4);
  });

  it("keeps multi-image fusion in one shared worker", () => {
    expect(resolveBatchCount(2, "thorough", 4, 8)).toBe(1);
  });
});
