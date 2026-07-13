import { describe, expect, it } from "vitest";
import { raceForSuccess } from "./batch-race";

interface Deferred<T> {
  promise: Promise<T>;
  resolve: (value: T) => void;
}

function deferred<T>(): Deferred<T> {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

describe("parallel batch race", () => {
  it("returns the first successful batch without waiting for the others", async () => {
    const slow = deferred<{ status: string; batch: number }>();
    const winner = Promise.resolve({ status: "decoded", batch: 1 });

    const raced = await raceForSuccess(
      [slow.promise, winner],
      (result) => result.status === "decoded",
    );

    expect(raced.winner).toEqual({ status: "decoded", batch: 1 });
    expect(raced.results).toEqual([{ status: "decoded", batch: 1 }]);
  });

  it("returns every partial in batch order when none succeeds", async () => {
    const first = deferred<{ status: string; batch: number }>();
    const second = deferred<{ status: string; batch: number }>();
    const raced = raceForSuccess(
      [first.promise, second.promise],
      (result) => result.status === "decoded",
    );

    second.resolve({ status: "ambiguous", batch: 1 });
    first.resolve({ status: "unrecoverable", batch: 0 });

    await expect(raced).resolves.toEqual({
      results: [
        { status: "unrecoverable", batch: 0 },
        { status: "ambiguous", batch: 1 },
      ],
    });
  });
});
