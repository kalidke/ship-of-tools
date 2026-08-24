import assert from "node:assert/strict";
import { PassThrough } from "node:stream";
import { test } from "node:test";
import type { SdkLike, SdkQuery, SdkQueryOpts } from "../src/main.js";

/**
 * HELPER_TEST_TURN_CAP must be set BEFORE src/main.js is first evaluated,
 * since TURN_SPOOL_CAP_BYTES is computed once at module load (that's the
 * point — "a module const the test can't change" after the fact). A static
 * `import` is hoisted ahead of ordinary statements in an ES module, so a
 * plain top-of-file assignment followed by a static import would run too
 * late; a dynamic import() is an ordinary expression, evaluated exactly
 * where it's written, so it sees the env var. node:test runs each test FILE
 * as its own process, so this override is scoped to this file only — every
 * other test file (and the real capsule, which never sets this var) sees
 * the real 64 MiB default.
 */
process.env.HELPER_TEST_TURN_CAP = "500";
const { run, TURN_SPOOL_CAP_BYTES } = await import("../src/main.js");

type Msg = Record<string, unknown>;

function scriptedQuery(messages: Msg[]): SdkQuery {
  async function* gen(): AsyncGenerator<Msg> {
    for (const m of messages) yield m;
  }
  const iterator = gen();
  return { [Symbol.asyncIterator]: () => iterator, interrupt: () => Promise.resolve(undefined) };
}

class FakeSdk implements SdkLike {
  #queue: SdkQuery[] = [];
  enqueue(q: SdkQuery): this {
    this.#queue.push(q);
    return this;
  }
  query(opts: SdkQueryOpts): SdkQuery {
    void (async () => {
      for await (const _m of opts.prompt) {
        /* drain, unused */
      }
    })();
    const q = this.#queue.shift();
    if (!q) throw new Error("FakeSdk: no queued query");
    return q;
  }
}

function harness(sdk: SdkLike) {
  const stdin = new PassThrough();
  const stdout = new PassThrough();
  const lines: unknown[] = [];
  let buf = "";
  stdout.on("data", (chunk: Buffer) => {
    buf += chunk.toString("utf8");
    let idx: number;
    while ((idx = buf.indexOf("\n")) >= 0) {
      lines.push(JSON.parse(buf.slice(0, idx)));
      buf = buf.slice(idx + 1);
    }
  });
  const exitCodes: number[] = [];
  void run(sdk, stdin, stdout, {}, (code) => exitCodes.push(code));
  return { stdin, lines, exitCodes };
}

function send(stdin: PassThrough, op: unknown): void {
  stdin.write(JSON.stringify(op) + "\n");
}

async function waitFor(check: () => boolean, ticks = 2000): Promise<void> {
  for (let i = 0; i < ticks && !check(); i++) {
    await new Promise((r) => setImmediate(r));
  }
  assert.ok(check(), "condition was not met in time");
}

test("HELPER_TEST_TURN_CAP overrides the module's spool cap for this test process only", () => {
  assert.equal(TURN_SPOOL_CAP_BYTES, 500);
});

test("cumulative msg output crossing the per-turn spool cap fatals as turn_too_large, never truncated", async () => {
  const sdk = new FakeSdk();
  // Ten small messages, each well under the 8 MiB per-line cap but comfortably
  // crossing the 500-byte per-turn cap together.
  const messages: Msg[] = Array.from({ length: 10 }, (_, i) => ({ type: "assistant", i, pad: "x".repeat(60) }));
  sdk.enqueue(scriptedQuery(messages));
  const { stdin, lines, exitCodes } = harness(sdk);
  send(stdin, { op: "user_turn", query_id: 1, text: "hi" });
  // writeLine resolves on the write's completion callback (M6), which can
  // settle after the stdout 'data' event that populates `lines` — wait for
  // exitCodes too, not just the fatal line.
  await waitFor(() => lines.some((l) => (l as { ev?: string }).ev === "fatal") && exitCodes.length > 0);
  const fatal = lines.find((l) => (l as { ev?: string }).ev === "fatal") as Record<string, unknown>;
  assert.equal(fatal.reason, "turn_too_large");
  assert.deepEqual(exitCodes, [1]);
  // Never a truncation: the line that would cross the bound is never
  // written at all, so strictly fewer "msg" lines arrive than were scripted.
  const msgCount = lines.filter((l) => (l as { ev?: string }).ev === "msg").length;
  assert.ok(msgCount < messages.length, `expected the cap to stop emission before all ${messages.length} messages, got ${msgCount}`);
});

test("a turn that stays under the spool cap ends normally, with no fatal", async () => {
  const sdk = new FakeSdk();
  sdk.enqueue(
    scriptedQuery([
      { type: "system", subtype: "init", session_id: "s" },
      { type: "result", session_id: "s" },
    ]),
  );
  const { stdin, lines, exitCodes } = harness(sdk);
  send(stdin, { op: "user_turn", query_id: 1, text: "hi" });
  await waitFor(() => lines.some((l) => (l as { ev?: string }).ev === "turn_end"));
  assert.equal(lines.some((l) => (l as { ev?: string }).ev === "fatal"), false);
  assert.deepEqual(exitCodes, []);
});
