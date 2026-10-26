// Bounded NDJSON framing for adapter stdout. Lines are delivered with an
// explicit error instead of being silently dropped: an overlong line aborts
// the stream, and a partial line at EOF is a truncation, not a drop.

import { StringDecoder } from "node:string_decoder";

/** Async generator over complete lines of a stream. Each yield is
 * `{ line, error }` where error is null, `"line_oversized"` or
 * `"truncated"`. The generator returns after a truncation or after the
 * stream ends. */
export async function* readNdjsonLines(stream, maxLineBytes) {
  // A StringDecoder keeps multibyte UTF-8 characters that span chunk
  // boundaries intact.
  const decoder = new StringDecoder("utf8");
  let buffer = "";
  for await (const chunk of stream) {
    buffer += decoder.write(chunk);
    let index;
    while ((index = buffer.indexOf("\n")) !== -1) {
      const line = buffer.slice(0, index);
      buffer = buffer.slice(index + 1);
      yield {
        line,
        error:
          Buffer.byteLength(line, "utf8") > maxLineBytes ? "line_oversized" : null,
      };
    }
    // The buffered tail is a single unterminated line that already exceeds
    // the bound; waiting for its newline would be unbounded memory growth.
    if (Buffer.byteLength(buffer, "utf8") > maxLineBytes) {
      yield { line: "", error: "line_oversized" };
      return;
    }
  }
  buffer += decoder.end();
  if (buffer.length > 0) {
    yield {
      line: buffer,
      error:
        Buffer.byteLength(buffer, "utf8") > maxLineBytes
          ? "line_oversized"
          : "truncated",
    };
  }
}
