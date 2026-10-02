import { once } from "node:events";
import { infer } from "./inference";
import { MAX_FRAME_BYTES, MAX_REQUEST_BYTES, parseRequest } from "./protocol";
import type { RuntimeEvent } from "./protocol";

async function readRequest(): Promise<unknown> {
  const chunks: Buffer[] = [];
  let length = 0;
  for await (const chunk of process.stdin) {
    const bytes = Buffer.from(chunk);
    length += bytes.length;
    if (length > MAX_REQUEST_BYTES) throw new Error("Request too large");
    chunks.push(bytes);
    if (bytes.includes(10)) {
      const buffer = Buffer.concat(chunks);
      const newline = buffer.indexOf(10);
      if (
        buffer
          .subarray(newline + 1)
          .some((byte) => ![10, 13, 32, 9].includes(byte))
      ) {
        throw new Error("Only one request per process is supported");
      }
      return JSON.parse(buffer.subarray(0, newline).toString("utf8"));
    }
  }
  throw new Error("Incomplete request");
}

async function emit(event: RuntimeEvent): Promise<void> {
  const frame = JSON.stringify(event) + "\n";
  if (Buffer.byteLength(frame) > MAX_FRAME_BYTES)
    throw new Error("Response too large");
  if (!process.stdout.write(frame)) await once(process.stdout, "drain");
}

async function main(): Promise<void> {
  let id = "invalid";
  // The OpenAI SDK otherwise inherits OPENAI_LOG and writes request/error
  // diagnostics to console/stdout, violating our private JSONL protocol.
  process.env.OPENAI_LOG = "off";
  const controller = new AbortController();
  process.once("SIGTERM", () => controller.abort());
  process.once("SIGINT", () => controller.abort());
  let valid = false;
  try {
    const request = parseRequest(await readRequest());
    id = request.id;
    valid = true;
    await infer(request, controller.signal, emit);
  } catch {
    controller.abort();
    await emit({
      version: 1,
      id,
      type: "error",
      code: valid ? "provider" : "protocol",
      message: valid ? "Runtime inference failed" : "Invalid runtime request",
    });
  }
}

// stdout is exclusively JSONL; never print prompts, keys or SDK error bodies.
main().then(
  () => process.stdout.end(() => process.exit(0)),
  () => process.stdout.end(() => process.exit(1)),
);
