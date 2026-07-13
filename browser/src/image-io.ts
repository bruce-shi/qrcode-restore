export interface PixelImage {
  width: number;
  height: number;
  data: Uint8ClampedArray;
}

const MAX_INPUT_PIXELS = 24_000_000;
const MAX_SIDE = 4096;

/** Decode a local browser File into the one RGBA upload sent to Rust/WASM. */
export async function fileToPixels(file: File): Promise<PixelImage> {
  const bitmap = await createImageBitmap(file, { imageOrientation: "from-image" });
  const scale = Math.min(
    1,
    MAX_SIDE / Math.max(bitmap.width, bitmap.height),
    Math.sqrt(MAX_INPUT_PIXELS / (bitmap.width * bitmap.height)),
  );
  const width = Math.max(21, Math.round(bitmap.width * scale));
  const height = Math.max(21, Math.round(bitmap.height * scale));
  const canvas = new OffscreenCanvas(width, height);
  const context = canvas.getContext("2d", { willReadFrequently: true });
  if (!context) throw new Error("This browser does not provide a 2D canvas context.");
  context.fillStyle = "white";
  context.fillRect(0, 0, width, height);
  context.drawImage(bitmap, 0, 0, width, height);
  bitmap.close();
  const image = context.getImageData(0, 0, width, height);
  return { width, height, data: image.data };
}
