import * as fs from "fs";
import * as path from "path";

export const IMAGE_DIR = "/tmp/baoclaw-images";

/** Ensure the image output directory exists */
export function ensureImageDir(): void {
  if (!fs.existsSync(IMAGE_DIR)) {
    fs.mkdirSync(IMAGE_DIR, { recursive: true });
  }
}

/** Save a base64-encoded image to /tmp/baoclaw-images/ and return the file path */
export function saveBase64Image(base64Data: string, mediaType: string): string {
  ensureImageDir();
  const ext = mediaType.split("/")[1] || "png"; // e.g. "png", "jpeg", "gif", "webp"
  const normalizedExt = ext === "jpeg" ? "jpg" : ext;
  const timestamp = Math.floor(Date.now() / 1000);
  const fileName = `baoclaw-${timestamp}.${normalizedExt}`;
  const filePath = path.join(IMAGE_DIR, fileName);
  const buffer = Buffer.from(base64Data, "base64");
  fs.writeFileSync(filePath, buffer);
  return filePath;
}

/** Display an image inline using iTerm2 Inline Image Protocol (if supported) */
export function displayIterm2Image(filePath: string): void {
  if (process.env.TERM_PROGRAM !== "iTerm.app") return;
  try {
    const data = fs.readFileSync(filePath);
    const base64 = data.toString("base64");
    const name = path.basename(filePath);
    process.stdout.write(
      `\x1b]1337;File=inline=1;name=${name}:size=${data.length}:${base64}\x07\n`,
    );
  } catch {
    // Silently ignore if iTerm2 display fails
  }
}

/** Extract image content blocks from a tool_result output, save & display them.
 *  Returns the number of images found. */
export function extractAndSaveImages(output: unknown): number {
  if (typeof output !== "object" || output === null) return 0;
  const o = output as Record<string, unknown>;

  let count = 0;

  // Case 1: Top-level image (ImageGenTool format)
  if (o.type === "image" && typeof o.source === "object" && o.source !== null) {
    const src = o.source as Record<string, unknown>;
    if (
      src.type === "base64" &&
      typeof src.data === "string" &&
      typeof src.media_type === "string"
    ) {
      const savedPath = saveBase64Image(src.data, src.media_type);
      displayIterm2Image(savedPath);
      count++;
    }
  }

  // Case 2: Array of content blocks
  if (Array.isArray(o.content)) {
    for (const block of o.content) {
      if (typeof block === "object" && block !== null) {
        const b = block as Record<string, unknown>;
        if (
          b.type === "image" &&
          typeof b.source === "object" &&
          b.source !== null
        ) {
          const src = b.source as Record<string, unknown>;
          if (
            src.type === "base64" &&
            typeof src.data === "string" &&
            typeof src.media_type === "string"
          ) {
            const savedPath = saveBase64Image(src.data, src.media_type);
            displayIterm2Image(savedPath);
            count++;
          }
        }
      }
    }
  }

  return count;
}
