import {
  type ChangeEvent,
  type DragEvent,
  type RefObject,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import sampleImageUrl from "../../examples/synthetic-blurred.png?url";
import {
  downloadCanvas,
  downloadReport,
  renderMatrix,
  renderPreview,
} from "./artifacts";
import type {
  BrowserRecoveryOptions,
  BrowserRecoveryResult,
  Effort,
  ParallelBatchSetting,
  ParallelBatches,
  RecoveryCandidate,
  WorkerResponse,
} from "./types";

interface ProgressState {
  stage: string;
  detail: string;
  completed: number;
  total: number;
}

interface HintState {
  prefix: string;
  expected: string;
  regex: string;
  maxSeconds: string;
  version: string;
  ecLevel: "" | "L" | "M" | "Q" | "H";
}

const initialHints: HintState = {
  prefix: "",
  expected: "",
  regex: "",
  maxSeconds: "",
  version: "",
  ecLevel: "",
};

const initialProgress: ProgressState = {
  stage: "STARTING WORKER",
  detail: "Loading QR engines",
  completed: 0,
  total: 0,
};

function formatBytes(size: number): string {
  if (size < 1_024) return `${size} B`;
  if (size < 1_048_576) return `${(size / 1_024).toFixed(1)} KB`;
  return `${(size / 1_048_576).toFixed(1)} MB`;
}

function buildOptions(
  effort: Effort,
  hints: HintState,
  parallelBatches: ParallelBatchSetting,
): BrowserRecoveryOptions {
  const regex = hints.regex.trim();
  if (regex) new RegExp(regex);
  const maxSeconds = Number(hints.maxSeconds);
  const version = Number(hints.version);
  return {
    effort,
    parallelBatches: parallelBatches === "auto" ? undefined : parallelBatches,
    maxSeconds: maxSeconds > 0 ? maxSeconds : undefined,
    version: version >= 1 && version <= 40 ? version : undefined,
    ecLevel: hints.ecLevel || undefined,
    payloadPrefix: hints.prefix.trim() || undefined,
    payloadRegex: regex || undefined,
    expectedText: hints.expected.trim() || undefined,
  };
}

function BrandHeader(): React.JSX.Element {
  return (
    <>
      <nav className="topbar" aria-label="Product">
        <a className="brand" href="#">
          <span className="brand-mark" aria-hidden="true">
            <i />
            <i />
            <i />
            <i />
            <i />
          </span>
          <span>
            QR RESTORE <b>LAB</b>
          </span>
        </a>
        <span className="local-badge">
          <i /> ON-DEVICE · NO UPLOAD
        </span>
      </nav>

      <section className="hero">
        <div className="eyebrow">
          <span>01</span> RECOVERY CONSOLE
        </div>
        <h1>
          Bring a blurred
          <br />
          <em>QR code</em> back.
        </h1>
        <p className="intro">
          Image ensembles, QR constraints, and Reed–Solomon correction—running privately in your
          browser.
        </p>
      </section>
    </>
  );
}

interface SourceImagesProps {
  files: File[];
  running: boolean;
  onFiles: (files: File[]) => void;
  onLoadSample: () => void;
}

function SourceImages({
  files,
  running,
  onFiles,
  onLoadSample,
}: SourceImagesProps): React.JSX.Element {
  const [dragging, setDragging] = useState(false);

  const handleInput = (event: ChangeEvent<HTMLInputElement>): void => {
    onFiles(Array.from(event.currentTarget.files ?? []));
    event.currentTarget.value = "";
  };

  const handleDrag = (event: DragEvent<HTMLLabelElement>, active: boolean): void => {
    event.preventDefault();
    if (!running) setDragging(active);
  };

  const handleDrop = (event: DragEvent<HTMLLabelElement>): void => {
    event.preventDefault();
    setDragging(false);
    if (!running) onFiles(Array.from(event.dataTransfer.files));
  };

  return (
    <>
      <label
        id="dropzone"
        className={`dropzone${files.length > 0 ? " has-files" : ""}${dragging ? " dragging" : ""}`}
        onDragEnter={(event) => handleDrag(event, true)}
        onDragOver={(event) => handleDrag(event, true)}
        onDragLeave={(event) => handleDrag(event, false)}
        onDrop={handleDrop}
      >
        <input
          id="file-input"
          type="file"
          accept="image/*"
          multiple
          disabled={running}
          onChange={handleInput}
        />
        <span className="scan-corners" aria-hidden="true" />
        <span className="drop-icon" aria-hidden="true">
          ＋
        </span>
        <strong>Drop QR images here</strong>
        <span>or choose files from your device</span>
      </label>

      <div className="file-list" id="file-list" aria-live="polite">
        {files.map((file, index) => (
          <div className="file-row" key={`${file.name}-${file.size}-${file.lastModified}-${index}`}>
            <span>
              {String(index + 1).padStart(2, "0")} {file.name}
            </span>
            <small>{formatBytes(file.size)}</small>
            <button
              type="button"
              disabled={running}
              onClick={() => onFiles(files.filter((_, item) => item !== index))}
            >
              REMOVE
            </button>
          </div>
        ))}
      </div>
      <button
        className="sample-button"
        id="load-sample"
        type="button"
        disabled={running}
        onClick={onLoadSample}
      >
        LOAD SYNTHETIC DEMO SAMPLE →
      </button>
    </>
  );
}

interface ControlsProps {
  effort: Effort;
  parallelBatches: ParallelBatchSetting;
  hints: HintState;
  onEffort: (effort: Effort) => void;
  onParallelBatches: (batches: ParallelBatchSetting) => void;
  onHints: (hints: HintState) => void;
}

function RecoveryControls({
  effort,
  parallelBatches,
  hints,
  onEffort,
  onParallelBatches,
  onHints,
}: ControlsProps): React.JSX.Element {
  const updateHint = <Key extends keyof HintState>(key: Key, value: HintState[Key]): void => {
    onHints({ ...hints, [key]: value });
  };

  return (
    <div className="control-grid">
      <fieldset>
        <legend>
          <span className="section-number">B</span> Search effort
        </legend>
        <div className="effort-switch">
          {(
            [
              ["fast", "Fast", "≤ 20 · grayscale ≈ 17"],
              ["balanced", "Balanced", "≤ 102 · grayscale ≈ 45"],
              ["thorough", "Thorough", "≤ 262 · grayscale ≈ 99"],
            ] as const
          ).map(([value, label, detail]) => (
            <label key={value}>
              <input
                type="radio"
                name="effort"
                value={value}
                checked={effort === value}
                onChange={() => onEffort(value)}
              />
              <span>
                {label}
                <small>{detail}</small>
              </span>
            </label>
          ))}
        </div>
        <label className="batch-control" htmlFor="parallel-batches">
          <span>
            Parallel batches
            <small>Single image · explicit values override Auto</small>
          </span>
          <select
            id="parallel-batches"
            value={parallelBatches}
            onChange={(event) => {
              const value = event.currentTarget.value;
              onParallelBatches(
                value === "auto" ? "auto" : (Number(value) as ParallelBatches),
              );
            }}
          >
            <option value="auto">Auto</option>
            <option value="1">1</option>
            <option value="2">2</option>
            <option value="3">3</option>
            <option value="4">4</option>
          </select>
        </label>
      </fieldset>

      <details>
        <summary>
          <span>
            <span className="section-number">C</span> Optional ranking hints
          </span>
          <i>＋</i>
        </summary>
        <div className="hint-grid">
          <label>
            Payload prefix
            <input
              id="prefix"
              value={hints.prefix}
              placeholder="https://"
              onChange={(event) => updateHint("prefix", event.currentTarget.value)}
            />
          </label>
          <label>
            Expected text
            <input
              id="expected"
              value={hints.expected}
              placeholder="Exact payload, if known"
              onChange={(event) => updateHint("expected", event.currentTarget.value)}
            />
          </label>
          <label>
            Payload regex
            <input
              id="regex"
              value={hints.regex}
              placeholder="^https://"
              onChange={(event) => updateHint("regex", event.currentTarget.value)}
            />
          </label>
          <label>
            Maximum seconds
            <input
              id="max-seconds"
              value={hints.maxSeconds}
              type="number"
              min="1"
              step="1"
              placeholder="Mode default"
              onChange={(event) => updateHint("maxSeconds", event.currentTarget.value)}
            />
          </label>
          <label>
            Version
            <input
              id="version"
              value={hints.version}
              type="number"
              min="1"
              max="40"
              placeholder="1–40"
              onChange={(event) => updateHint("version", event.currentTarget.value)}
            />
          </label>
          <label>
            Error correction
            <select
              id="ec-level"
              value={hints.ecLevel}
              onChange={(event) =>
                updateHint("ecLevel", event.currentTarget.value as HintState["ecLevel"])
              }
            >
              <option value="">Automatic</option>
              <option>L</option>
              <option>M</option>
              <option>Q</option>
              <option>H</option>
            </select>
          </label>
        </div>
        <p className="hint-note">
          Hints only rank verified results. They cannot make an invalid QR candidate valid.
        </p>
      </details>
    </div>
  );
}

function RecoveryProgress({
  progress,
  onCancel,
}: {
  progress: ProgressState;
  onCancel: () => void;
}): React.JSX.Element {
  const percent =
    progress.total > 0 ? Math.min(98, (progress.completed / progress.total) * 100) : 2;
  return (
    <div className="progress-wrap" id="progress-wrap" aria-live="polite">
      <div className="progress-meta">
        <span id="progress-stage">{progress.stage.toUpperCase()}</span>
        <span id="progress-detail">{progress.detail}</span>
      </div>
      <div className="progress-track">
        <i id="progress-bar" style={{ width: `${Math.max(2, percent)}%` }} />
      </div>
      <button className="cancel-button" id="cancel" type="button" onClick={onCancel}>
        Cancel
      </button>
    </div>
  );
}

function ResultCanvas({
  candidate,
  canvasRef,
}: {
  candidate: RecoveryCandidate | undefined;
  canvasRef: RefObject<HTMLCanvasElement | null>;
}): React.JSX.Element {
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    if (candidate) renderMatrix(canvas, candidate);
    else {
      canvas.width = 1;
      canvas.height = 1;
    }
  }, [candidate, canvasRef]);
  return <canvas id="restored-canvas" ref={canvasRef} />;
}

function PreviewCanvas({ result }: { result: BrowserRecoveryResult }): React.JSX.Element {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    if (canvasRef.current && result.bestVariant) renderPreview(canvasRef.current, result);
  }, [result]);
  return <canvas id="preview-canvas" ref={canvasRef} />;
}

function RecoveryResultPanel({ result }: { result: BrowserRecoveryResult }): React.JSX.Element {
  const restoredCanvas = useRef<HTMLCanvasElement>(null);
  const [copied, setCopied] = useState(false);
  const candidate = result.candidates[0];
  const summary = candidate
    ? `${candidate.confidence} confidence · ${result.diagnostics.examinedVariants} variants · ${result.diagnostics.elapsedSeconds.toFixed(2)} s`
    : `No verified candidate after ${result.diagnostics.examinedVariants} variants in ${result.diagnostics.elapsedSeconds.toFixed(2)} s.`;
  const metadata = candidate
    ? [
        ["VERSION", candidate.version],
        ["EC LEVEL", candidate.ecLevel],
        ["MASK", candidate.mask],
        ["MATRIX", candidate.matrixKind.toUpperCase()],
        ["RS CORRECTIONS", candidate.correctedSymbols],
        ["EVIDENCE", candidate.evidenceCount],
      ]
    : [];

  const copyPayload = async (): Promise<void> => {
    if (!candidate?.text) return;
    await navigator.clipboard.writeText(candidate.text);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1_500);
  };

  return (
    <section className="result-section" id="result-section" aria-live="polite">
      <div className="result-heading">
        <div>
          <span className="section-number">D</span>
          <div>
            <h2>Recovery result</h2>
            <p id="result-summary">{summary}</p>
          </div>
        </div>
        <span className="status-pill" id="status-pill" data-status={result.status}>
          {result.status.toUpperCase()}
        </span>
      </div>
      <div className="result-grid">
        <article className="qr-card">
          <div className="canvas-frame">
            <ResultCanvas candidate={candidate} canvasRef={restoredCanvas} />
          </div>
          <div className="artifact-actions">
            <button
              id="download-png"
              type="button"
              disabled={!candidate}
              onClick={() => {
                if (restoredCanvas.current) downloadCanvas(restoredCanvas.current, "restored.png");
              }}
            >
              RESTORED.PNG
            </button>
            <button id="download-report" type="button" onClick={() => downloadReport(result)}>
              REPORT.JSON
            </button>
          </div>
        </article>
        <article className="data-card">
          <div className="data-label">
            VERIFIED PAYLOAD
            <button
              id="copy-payload"
              type="button"
              disabled={!candidate?.text}
              onClick={copyPayload}
            >
              {copied ? "COPIED" : "COPY"}
            </button>
          </div>
          <pre id="payload">
            {candidate
              ? candidate.text || `[binary payload · ${candidate.payload.length} bytes]`
              : "The available image evidence is below the recovery limit."}
          </pre>
          <dl id="metadata">
            {metadata.map(([label, value]) => (
              <div key={label}>
                <dt>{label}</dt>
                <dd>{value}</dd>
              </div>
            ))}
          </dl>
          {candidate && result.bestVariant && (
            <div className="processed-preview">
              <div className="processed-preview-heading">
                <span>WORKING VARIANT</span>
                <b id="variant-name">{result.bestVariantName ?? candidate.source}</b>
              </div>
              <div className="processed-preview-frame">
                <PreviewCanvas result={result} />
              </div>
              <small>
                {result.bestVariant.width} × {result.bestVariant.height} px · exact processed image
                used by the winning payload
              </small>
            </div>
          )}
        </article>
      </div>
    </section>
  );
}

export function App(): React.JSX.Element {
  const [files, setFiles] = useState<File[]>([]);
  const [effort, setEffort] = useState<Effort>("balanced");
  const [parallelBatches, setParallelBatches] =
    useState<ParallelBatchSetting>("auto");
  const [hints, setHints] = useState<HintState>(initialHints);
  const [running, setRunning] = useState(false);
  const [progress, setProgress] = useState<ProgressState>(initialProgress);
  const [result, setResult] = useState<BrowserRecoveryResult>();
  const [error, setError] = useState<string>();
  const workerRef = useRef<Worker | undefined>(undefined);

  const stopWorker = useCallback((): void => {
    workerRef.current?.terminate();
    workerRef.current = undefined;
  }, []);

  useEffect(() => stopWorker, [stopWorker]);

  const replaceFiles = (nextFiles: File[]): void => {
    setFiles(nextFiles.filter((file) => file.type.startsWith("image/")));
    setError(undefined);
  };

  const loadSample = async (): Promise<void> => {
    try {
      const response = await fetch(sampleImageUrl);
      if (!response.ok) throw new Error(`sample request returned ${response.status}`);
      const blob = await response.blob();
      replaceFiles([
        new File([blob], "synthetic-blurred.png", { type: blob.type || "image/png" }),
      ]);
    } catch (sampleError) {
      setError(`Could not load the bundled sample: ${String(sampleError)}`);
    }
  };

  const startRecovery = (): void => {
    let recoveryOptions: BrowserRecoveryOptions;
    try {
      recoveryOptions = buildOptions(effort, hints, parallelBatches);
    } catch (optionError) {
      setError(`Invalid regular expression: ${String(optionError)}`);
      return;
    }

    stopWorker();
    const nextWorker = new Worker(new URL("./recovery.worker.ts", import.meta.url), {
      type: "module",
    });
    workerRef.current = nextWorker;
    setError(undefined);
    setResult(undefined);
    setRunning(true);
    setProgress(initialProgress);

    const finish = (): void => {
      if (workerRef.current !== nextWorker) return;
      nextWorker.terminate();
      workerRef.current = undefined;
      setRunning(false);
    };

    nextWorker.addEventListener("message", (event: MessageEvent<WorkerResponse>) => {
      if (workerRef.current !== nextWorker) return;
      const message = event.data;
      if (message.kind === "progress") {
        setProgress(message);
      } else if (message.kind === "result") {
        setResult(message.result);
        finish();
      } else {
        setError(`Recovery failed: ${message.message}`);
        finish();
      }
    });
    nextWorker.addEventListener("error", (event) => {
      if (workerRef.current !== nextWorker) return;
      setError(`Recovery worker failed: ${event.message}`);
      finish();
    });
    nextWorker.postMessage({ kind: "recover", files, options: recoveryOptions });
  };

  const cancelRecovery = (): void => {
    stopWorker();
    setRunning(false);
    setProgress(initialProgress);
  };

  return (
    <main>
      <BrandHeader />

      <section className="workspace" aria-labelledby="input-title">
        <div className="panel-heading">
          <div>
            <span className="section-number">A</span>
            <div>
              <h2 id="input-title">Source images</h2>
              <p>One image, or several frames of the same symbol.</p>
            </div>
          </div>
          <span className="format-note">PNG · JPEG · WEBP</span>
        </div>

        <SourceImages
          files={files}
          running={running}
          onFiles={replaceFiles}
          onLoadSample={loadSample}
        />
        <RecoveryControls
          effort={effort}
          parallelBatches={parallelBatches}
          hints={hints}
          onEffort={setEffort}
          onParallelBatches={setParallelBatches}
          onHints={setHints}
        />

        <button
          className="recover-button"
          id="recover"
          type="button"
          disabled={running || files.length === 0}
          onClick={startRecovery}
        >
          <span>RUN RECOVERY</span>
          <b aria-hidden="true">↗</b>
        </button>

        {error && (
          <p className="error-message" role="alert">
            {error}
          </p>
        )}
        {running && <RecoveryProgress progress={progress} onCancel={cancelRecovery} />}
      </section>

      {!running && result && <RecoveryResultPanel result={result} />}

      <footer>
        <span>QR RESTORE / BROWSER PROTOTYPE</span>
        <span>STANDARD-CONSTRAINED · NO GENERATIVE GUESSING</span>
      </footer>
    </main>
  );
}
