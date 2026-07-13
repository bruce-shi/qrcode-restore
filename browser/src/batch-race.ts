export interface BatchRaceResult<T> {
  winner?: T;
  results: T[];
}

/**
 * Resolve as soon as one batch returns a successful result. If no batch
 * succeeds, retain the original input order so the caller can merge all
 * partial evidence deterministically.
 */
export function raceForSuccess<T>(
  jobs: Promise<T>[],
  isSuccess: (result: T) => boolean,
): Promise<BatchRaceResult<T>> {
  if (jobs.length === 0) return Promise.resolve({ results: [] });

  return new Promise((resolve, reject) => {
    const results: Array<T | undefined> = Array.from({ length: jobs.length });
    let remaining = jobs.length;
    let settled = false;

    jobs.forEach((job, index) => {
      job.then(
        (result) => {
          if (settled) return;
          results[index] = result;
          if (isSuccess(result)) {
            settled = true;
            resolve({ winner: result, results: [result] });
            return;
          }

          remaining -= 1;
          if (remaining === 0) {
            settled = true;
            resolve({ results: results.filter((value): value is T => value !== undefined) });
          }
        },
        (error: unknown) => {
          if (settled) return;
          settled = true;
          reject(error);
        },
      );
    });
  });
}
