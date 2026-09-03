/** Split Telegram messages without exceeding the platform limit. */
export function splitMessage(text: string, max = 4096): string[] {
  if (max < 1) throw new RangeError("max must be greater than zero");
  if (text.length <= max) return [text];

  const chunks: string[] = [];
  let offset = 0;
  while (text.length - offset > max) {
    let end = offset + max;
    if (isHighSurrogate(text.charCodeAt(end - 1))) {
      end = end === offset + 1 ? end + 1 : end - 1;
    }

    let relSplit = text.slice(offset, end + 2).lastIndexOf("\n\n");
    if (relSplit <= 0) {
      relSplit = text.slice(offset, end + 1).lastIndexOf("\n");
    }
    const splitAt = relSplit > 0 ? offset + relSplit : end;
    chunks.push(text.slice(offset, splitAt));
    offset = splitAt;
  }
  if (offset < text.length) chunks.push(text.slice(offset));
  return chunks;
}

function isHighSurrogate(code: number): boolean {
  return code >= 0xd800 && code <= 0xdbff;
}
