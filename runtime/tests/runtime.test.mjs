import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { createServer } from "node:http";
import { once } from "node:events";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const host = execFileSync("rustc", ["-vV"], { encoding: "utf8" })
  .split("\n")
  .find((line) => line.startsWith("host: "))
  .slice(6);
const binary = resolve(
  root,
  `src-tauri/binaries/mira-runtime-${host}${process.platform === "win32" ? ".exe" : ""}`,
);

function request(baseUrl, changes = {}) {
  return {
    version: 1,
    id: "test-request",
    type: "complete",
    config: {
      provider: "openai",
      name: "Fixture",
      base_url: baseUrl,
      model: "fixture-model",
      api_key: "fixture-key",
    },
    messages: [
      { role: "system", content: "Private memory and project fixture" },
      { role: "user", content: "Previous question" },
      { role: "assistant", content: "Previous reply" },
      { role: "user", content: "你好" },
    ],
    ...changes,
  };
}

function run(input, onEvent, env = {}) {
  const child = spawn(binary, [], {
    stdio: ["pipe", "pipe", "pipe"],
    env: { ...process.env, ...env },
  });
  let stdout = "";
  let stderr = "";
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (data) => {
    stdout += data;
    onEvent?.(data, child);
  });
  child.stderr.on("data", (data) => {
    stderr += data;
  });
  child.stdin.end(
    typeof input === "string" ? input : JSON.stringify(input) + "\n",
  );
  const result = once(child, "close").then(([code]) => {
    assert.equal(code, 0, stderr);
    assert.equal(stderr, "");
    const events = stdout
      .trim()
      .split("\n")
      .map((line) => JSON.parse(line));
    assert.equal(
      events.filter((event) => ["done", "error"].includes(event.type)).length,
      1,
    );
    return events;
  });
  return { child, result };
}

async function fixture(t, handler) {
  const server = createServer(handler);
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  t.after(() => {
    server.closeAllConnections();
    server.close();
  });
  return `http://127.0.0.1:${server.address().port}/v1`;
}

async function body(req) {
  let text = "";
  for await (const chunk of req) text += chunk;
  return JSON.parse(text);
}

function reply(res, deltas = [{ content: "你好 🌍" }]) {
  res.writeHead(200, { "Content-Type": "text/event-stream" });
  for (const delta of deltas) {
    res.write(
      `data: ${JSON.stringify({ id: "fixture", object: "chat.completion.chunk", choices: [{ index: 0, delta, finish_reason: null }] })}\n\n`,
    );
  }
  res.end(
    `data: ${JSON.stringify({ choices: [{ index: 0, delta: {}, finish_reason: "stop" }] })}\n\ndata: [DONE]\n\n`,
  );
}

test("compiled sidecar streams Unicode, preserves context and excludes tools", async (t) => {
  let sent;
  const url = await fixture(t, async (req, res) => {
    assert.equal(req.url, "/v1/chat/completions");
    assert.equal(req.headers.authorization, "Bearer fixture-key");
    sent = await body(req);
    reply(res, [
      { reasoning_content: "思考" },
      { content: "你好 " },
      { content: "🌍" },
    ]);
  });
  const events = await run(request(url)).result;
  assert.equal(sent.model, "fixture-model");
  assert.deepEqual(
    sent.messages.map((message) => message.content),
    request(url).messages.map((message) => message.content),
  );
  assert.equal(sent.tools, undefined);
  assert.equal(sent.max_completion_tokens, undefined);
  assert.equal(sent.reasoning_effort, undefined);
  assert.equal(
    events
      .filter((event) => event.type === "thinking_delta")
      .map((event) => event.delta)
      .join(""),
    "思考",
  );
  assert.equal(
    events
      .filter((event) => event.type === "text_delta")
      .map((event) => event.delta)
      .join(""),
    "你好 🌍",
  );
  assert.deepEqual(events.at(-1), {
    version: 1,
    id: "test-request",
    type: "done",
    content: "你好 🌍",
  });
});

test("DeepSeek reasoning and background planner sampling go through Pi", async (t) => {
  let sent;
  const url = await fixture(t, async (req, res) => {
    sent = await body(req);
    reply(res);
  });
  const input = request(url, { temperature: 0.1 });
  input.config.provider = "deepseek";
  input.config.model = "deepseek-reasoner";
  await run(input).result;
  assert.equal(sent.temperature, 0.1);
  assert.deepEqual(sent.thinking, { type: "enabled" });
  assert.equal(sent.reasoning_effort, "high");
});

test("SDK retries transient errors, never echoes credential-bearing error bodies", async (t) => {
  let calls = 0;
  const url = await fixture(t, async (_req, res) => {
    calls++;
    if (calls < 3) {
      res.writeHead(503, { "retry-after-ms": "1" });
      res.end("fixture-key private prompt");
    } else reply(res);
  });
  assert.equal((await run(request(url)).result).at(-1).type, "done");
  assert.equal(calls, 3);
});

test("authentication failure is terminal and redacted", async (t) => {
  let calls = 0;
  const url = await fixture(t, (_req, res) => {
    calls++;
    res.writeHead(401);
    res.end("fixture-key private prompt");
  });
  const events = await run(request(url)).result;
  assert.equal(calls, 1);
  assert.equal(events.at(-1).code, "provider");
  assert.equal(JSON.stringify(events).includes("fixture-key"), false);
  assert.equal(JSON.stringify(events).includes("private prompt"), false);
});

test("inherited SDK logging cannot pollute JSONL or expose error bodies", async (t) => {
  const url = await fixture(t, (_req, res) => {
    res.writeHead(401);
    res.end("fixture-key private prompt");
  });
  for (const OPENAI_LOG of ["info", "debug"]) {
    const events = await run(request(url), undefined, { OPENAI_LOG }).result;
    assert.equal(events.length, 1);
    assert.equal(events[0].code, "provider");
    assert.equal(JSON.stringify(events).includes("fixture-key"), false);
    assert.equal(JSON.stringify(events).includes("private prompt"), false);
  }
});

test("long server retry waits fail within the host's inactivity budget", async (t) => {
  let calls = 0;
  const url = await fixture(t, (_req, res) => {
    calls++;
    res.writeHead(503, { "retry-after": "60" });
    res.end("fixture-key private prompt");
  });
  const events = await run(request(url)).result;
  assert.equal(calls, 1);
  assert.equal(events.at(-1).code, "provider");
});

test("empty completion is not persisted as success", async (t) => {
  const url = await fixture(t, (_req, res) => reply(res, []));
  assert.equal((await run(request(url)).result).at(-1).type, "error");
});

test("isolated concurrent requests do not mix ids or credentials", async (t) => {
  const url = await fixture(t, async (req, res) => {
    const input = await body(req);
    reply(res, [{ content: input.model }]);
  });
  const first = request(url, { id: "first" });
  const second = request(url, { id: "second" });
  second.config.model = "second-model";
  const results = await Promise.all([run(first).result, run(second).result]);
  assert.ok(results[0].every((event) => event.id === "first"));
  assert.ok(results[1].every((event) => event.id === "second"));
  assert.equal(results[1].at(-1).content, "second-model");
});

test("invalid protocol, missing keys and oversized input fail without inference", async () => {
  for (const input of [
    "not-json\n",
    JSON.stringify({ version: 2 }) + "\n",
    request("file:///tmp/private"),
    request("http://localhost", { messages: [] }),
    "x".repeat(8 * 1024 * 1024 + 1) + "\n",
  ]) {
    assert.equal((await run(input).result).at(-1).code, "protocol");
  }
});

test(
  "abort interrupts a stalled response before any tokens",
  { skip: process.platform === "win32", timeout: 5000 },
  async (t) => {
    let child;
    const url = await fixture(t, (_req, _res) => child.kill("SIGTERM"));
    const running = run(request(url));
    child = running.child;
    const events = await running.result;
    assert.equal(events.at(-1).code, "cancelled");
  },
);
