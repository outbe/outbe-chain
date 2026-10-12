/** A `data:application/json` URI decoded to its document, or the input when it is not one. */
export type DecodedDataUri = Record<string, unknown> | string;

export function parseDataUri(s: string): DecodedDataUri {
  const marker = "data:application/json";
  if (!s.startsWith(marker)) return s;
  const comma = s.indexOf(",");
  if (comma < 0) return s;
  const meta = s.slice(0, comma);
  let payload = s.slice(comma + 1);
  try {
    if (meta.includes(";base64")) {
      payload = Buffer.from(payload, "base64").toString("utf8");
    }
    const document: unknown = JSON.parse(payload);
    if (document === null || typeof document !== "object" || Array.isArray(document)) return s;
    return collapseInlineImage(document as Record<string, unknown>);
  } catch {
    return s;
  }
}

/** An on-chain image is kilobytes of base64, so tool output keeps only its type and size. */
function collapseInlineImage(document: Record<string, unknown>): Record<string, unknown> {
  const image = document.image;
  if (typeof image !== "string" || !image.startsWith("data:image/")) return document;
  const comma = image.indexOf(",");
  if (comma < 0) return document;
  const mediaType = image.slice("data:".length, comma).split(";")[0];
  const bytes = image.slice(0, comma).includes(";base64")
    ? Buffer.byteLength(image.slice(comma + 1), "base64")
    : image.length - comma - 1;
  return { ...document, image: `<${mediaType}, ${bytes} bytes>` };
}
