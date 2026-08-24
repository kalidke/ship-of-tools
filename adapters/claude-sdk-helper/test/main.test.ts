import assert from "node:assert/strict";
import { PassThrough } from "node:stream";
import { test } from "node:test";
import type { SDKUserMessage } from "@anthropic-ai/claude-agent-sdk";
import { MAX_LINE_BYTES } from "../src/protocol.js";
import { run, type SdkLike, type SdkQuery, type SdkQueryOpts } from "../src/main.js";

type Msg = Record<string, unknown>;

/** A query whose iterator yields a fixed script, then ends (or throws). */
function scriptedQuery(messages: Msg[], opts: { throwAfter?: Error; interrupt?: () => Promise<unknown> } = {}): SdkQuery {
  async function* gen(): AsyncGenerator<Msg> {
    for (const m of messages) yield m;
    if (opts.throwAfter) throw opts.throwAfter;
  }
  const iterator = gen();
  return {
    [Symbol.asyncIterator]: () => iterator,
    interrupt: opts.interrupt ?? (() => Promise.resolve(undefined)),
  };
}

/** A query whose iterator never produces a next value — models a turn genuinely in flight. */
function neverEndingQuery(interrupt: () => Promise<unknown>): SdkQuery {
  return {
    [Symbol.asyncIterator]: () => ({
      next: () => new Promise<IteratorResult<Msg>>(() => {}),
    }),
    interrupt,
  };
}

/** A query whose iterator only advances when told to — for tests that need to interleave a mid-turn op. */
function controllableQuery(interrupt: () => Promise<unknown>): { query: SdkQuery; push: (m: Msg) => void; end: () => void } {
  let resolveNext: ((r: IteratorResult<Msg>) => void) | null = null;
  const pending: Msg[] = [];
  let ended = false;
  const iterator: AsyncIterator<Msg> = {
    next: () => {
      if (pending.length > 0) return Promise.resolve({ value: pending.shift() as Msg, done: false });
      if (ended) return Promise.resolve({ value: undefined, done: true });
      return new Promise((resolve) => {
        resolveNext = resolve;
      });
    },
  };
  return {
    query: { [Symbol.asyncIterator]: () => iterator, interrupt },
    push: (m: Msg) => {
      if (resolveNext) {
        const r = resolveNext;
        resolveNext = null;
        r({ value: m, done: false });
      } else {
        pending.push(m);
      }
    },
    end: () => {
      ended = true;
      if (resolveNext) {
        const r = resolveNext;
        resolveNext = null;
        r({ value: undefined, done: true });
      }
    },
  };
}

class FakeSdk implements SdkLike {
  calls: SdkQueryOpts[] = [];
  /** Each call's prompt iterable, fully drained (fire-and-forget, since query() is sync). */
  receivedPrompts: SDKUserMessage[][] = [];
  #queue: SdkQuery[] = [];
  enqueue(q: SdkQuery): this {
    this.#queue.push(q);
    return this;
  }
  query(opts: SdkQueryOpts): SdkQuery {
    this.calls.push(opts);
    const received: SDKUserMessage[] = [];
    this.receivedPrompts.push(received);
    void (async () => {
      for await (const m of opts.prompt) received.push(m);
    })();
    const q = this.#queue.shift();
    if (!q) throw new Error(`FakeSdk: no queued query for call #${this.calls.length}`);
    return q;
  }
}

function harness(sdk: SdkLike, env: NodeJS.ProcessEnv = {}) {
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
  const donePromise = run(sdk, stdin, stdout, env, (code) => exitCodes.push(code));
  return { stdin, stdout, lines, exitCodes, donePromise };
}

function send(stdin: PassThrough, ...ops: unknown[]): void {
  stdin.write(ops.map((o) => JSON.stringify(o)).join("\n") + "\n");
}

async function waitFor(check: () => boolean, ticks = 2000): Promise<void> {
  for (let i = 0; i < ticks && !check(); i++) {
    await new Promise((r) => setImmediate(r));
  }
  assert.ok(check(), "condition was not met in time");
}

test("hello is the first line, carrying protocol 1 and the pinned sdk version", async () => {
  const { lines } = harness(new FakeSdk());
  await waitFor(() => lines.length >= 1);
  assert.deepEqual(lines[0], { ev: "hello", protocol: 1, sdk_version: "0.3.241" });
});

test("happy two-turn flow: fresh query then resume, identical authority-bearing options", async () => {
  const sdk = new FakeSdk();
  const init = { type: "system", subtype: "init", session_id: "sess-A" };
  sdk.enqueue(scriptedQuery([init, { type: "assistant", session_id: "sess-A" }, { type: "result", session_id: "sess-A" }]));
  sdk.enqueue(scriptedQuery([init, { type: "result", session_id: "sess-A" }]));
  const { stdin, lines } = harness(sdk, { HELPER_MODEL: "claude-sonnet-5" });

  send(stdin, { op: "user_turn", query_id: 1, text: "hello" });
  await waitFor(() => lines.some((l) => (l as { ev?: string }).ev === "turn_end"));

  send(stdin, { op: "user_turn", query_id: 2, text: "again" });
  await waitFor(() => lines.filter((l) => (l as { ev?: string }).ev === "turn_end").length === 2);
  await waitFor(() => sdk.receivedPrompts[1]?.length === 1);

  assert.equal(sdk.calls.length, 2);
  assert.equal(sdk.receivedPrompts[0][0].message.content, "hello");
  assert.equal(sdk.calls[0].options.resume, undefined);
  assert.equal(sdk.receivedPrompts[1][0].message.content, "again");
  assert.equal(sdk.calls[1].options.resume, "sess-A");
  for (const call of sdk.calls) {
    assert.equal(call.options.permissionMode, "bypassPermissions");
    assert.equal(call.options.allowDangerouslySkipPermissions, true);
    assert.equal(call.options.includePartialMessages, false);
    assert.equal(call.options.model, "claude-sonnet-5");
  }
  assert.deepEqual(lines[lines.length - 1], { ev: "turn_end", query_id: 2, results: 1 });
});

test("each turn delivers exactly one SDKUserMessage on the prompt iterable, then ends", async () => {
  const sdk = new FakeSdk();
  sdk.enqueue(
    scriptedQuery([
      { type: "system", subtype: "init", session_id: "s" },
      { type: "result", session_id: "s" },
    ]),
  );
  const { stdin, lines } = harness(sdk);
  send(stdin, { op: "user_turn", query_id: 1, text: "only one, please" });
  await waitFor(() => lines.some((l) => (l as { ev?: string }).ev === "turn_end"));
  await waitFor(() => sdk.receivedPrompts[0]?.length === 1);
  // No further messages ever arrive on the iterable after the first — give
  // the fake's drain loop a further tick to prove that, not just assert
  // count-at-this-instant.
  await new Promise((r) => setImmediate(r));
  assert.equal(sdk.receivedPrompts[0].length, 1);
  assert.deepEqual(sdk.receivedPrompts[0][0], {
    type: "user",
    message: { role: "user", content: "only one, please" },
    parent_tool_use_id: null,
  });
});

test("HELPER_MODEL is omitted from options when unset", async () => {
  const sdk = new FakeSdk();
  sdk.enqueue(
    scriptedQuery([
      { type: "system", subtype: "init", session_id: "s" },
      { type: "result", session_id: "s" },
    ]),
  );
  const { stdin, lines } = harness(sdk, {});
  send(stdin, { op: "user_turn", query_id: 1, text: "hi" });
  await waitFor(() => lines.some((l) => (l as { ev?: string }).ev === "turn_end"));
  assert.equal("model" in sdk.calls[0].options, false);
});

test("a message that cannot be projected fatals as serializer", async () => {
  const sdk = new FakeSdk();
  sdk.enqueue(scriptedQuery([{ type: "system", subtype: "init", session_id: "s", cursed: 10n }]));
  const { stdin, lines, exitCodes } = harness(sdk);
  send(stdin, { op: "user_turn", query_id: 1, text: "hi" });
  // writeLine now resolves on the write's completion callback (M6), which
  // can settle after the stdout 'data' event that populates `lines` — so
  // wait for exitCodes too, not just the fatal line, before asserting on it.
  await waitFor(() => lines.some((l) => (l as { ev?: string }).ev === "fatal") && exitCodes.length > 0);
  assert.deepEqual(lines[lines.length - 1], { ev: "fatal", reason: "serializer", detail: "an SDK message could not be projected to JSON" });
  assert.deepEqual(exitCodes, [1]);
});

test("zero result messages fatals as no_result (no interrupt was ever sent for this turn)", async () => {
  const sdk = new FakeSdk();
  sdk.enqueue(scriptedQuery([{ type: "system", subtype: "init", session_id: "s" }, { type: "assistant", session_id: "s" }]));
  const { stdin, lines, exitCodes } = harness(sdk);
  send(stdin, { op: "user_turn", query_id: 1, text: "hi" });
  // writeLine now resolves on the write's completion callback (M6), which
  // can settle after the stdout 'data' event that populates `lines` — so
  // wait for exitCodes too, not just the fatal line, before asserting on it.
  await waitFor(() => lines.some((l) => (l as { ev?: string }).ev === "fatal") && exitCodes.length > 0);
  assert.equal((lines[lines.length - 1] as { reason?: string }).reason, "no_result");
  assert.deepEqual(exitCodes, [1]);
});

test("interrupt then a result-less exhaustion ends the turn normally: turn_end results:0, no fatal", async () => {
  const sdk = new FakeSdk();
  const ctl = controllableQuery(() => Promise.resolve(undefined));
  sdk.enqueue(ctl.query);
  const { stdin, lines, exitCodes } = harness(sdk);
  send(stdin, { op: "user_turn", query_id: 1, text: "hi" });
  await waitFor(() => sdk.calls.length === 1);
  ctl.push({ type: "system", subtype: "init", session_id: "s" });
  await waitFor(() => lines.some((l) => (l as { ev?: string }).ev === "msg"));
  send(stdin, { op: "interrupt", id: 9 });
  await waitFor(() => lines.some((l) => (l as { ev?: string }).ev === "interrupted"));
  ctl.end();
  await waitFor(() => lines.some((l) => (l as { ev?: string }).ev === "turn_end"));
  const turnEnd = lines.find((l) => (l as { ev?: string }).ev === "turn_end");
  assert.deepEqual(turnEnd, { ev: "turn_end", query_id: 1, results: 0 });
  assert.equal(lines.some((l) => (l as { ev?: string }).ev === "fatal"), false);
  assert.deepEqual(exitCodes, []);
});

test("more than one result message fatals as multi_result", async () => {
  const sdk = new FakeSdk();
  sdk.enqueue(
    scriptedQuery([
      { type: "system", subtype: "init", session_id: "s" },
      { type: "result", session_id: "s" },
      { type: "result", session_id: "s" },
    ]),
  );
  const { stdin, lines, exitCodes } = harness(sdk);
  send(stdin, { op: "user_turn", query_id: 1, text: "hi" });
  // writeLine now resolves on the write's completion callback (M6), which
  // can settle after the stdout 'data' event that populates `lines` — so
  // wait for exitCodes too, not just the fatal line, before asserting on it.
  await waitFor(() => lines.some((l) => (l as { ev?: string }).ev === "fatal") && exitCodes.length > 0);
  assert.equal((lines[lines.length - 1] as { reason?: string }).reason, "multi_result");
  assert.deepEqual(exitCodes, [1]);
});

test("a session_id that drifts from the resumed session fatals as session_drift", async () => {
  const sdk = new FakeSdk();
  sdk.enqueue(
    scriptedQuery([
      { type: "system", subtype: "init", session_id: "sess-A" },
      { type: "assistant", session_id: "sess-B" },
    ]),
  );
  const { stdin, lines, exitCodes } = harness(sdk);
  send(stdin, { op: "user_turn", query_id: 1, text: "hi" });
  // writeLine now resolves on the write's completion callback (M6), which
  // can settle after the stdout 'data' event that populates `lines` — so
  // wait for exitCodes too, not just the fatal line, before asserting on it.
  await waitFor(() => lines.some((l) => (l as { ev?: string }).ev === "fatal") && exitCodes.length > 0);
  assert.equal((lines[lines.length - 1] as { reason?: string }).reason, "session_drift");
  assert.deepEqual(exitCodes, [1]);
});

test("the SDK iterator rejecting fatals as query_error", async () => {
  const sdk = new FakeSdk();
  sdk.enqueue(scriptedQuery([{ type: "system", subtype: "init", session_id: "s" }], { throwAfter: new Error("boom") }));
  const { stdin, lines, exitCodes } = harness(sdk);
  send(stdin, { op: "user_turn", query_id: 1, text: "hi" });
  // writeLine now resolves on the write's completion callback (M6), which
  // can settle after the stdout 'data' event that populates `lines` — so
  // wait for exitCodes too, not just the fatal line, before asserting on it.
  await waitFor(() => lines.some((l) => (l as { ev?: string }).ev === "fatal") && exitCodes.length > 0);
  assert.deepEqual(lines[lines.length - 1], { ev: "fatal", reason: "query_error", detail: "Error" });
  assert.deepEqual(exitCodes, [1]);
});

test("a projected message line over the 8 MiB cap fatals as line_too_large", async () => {
  const sdk = new FakeSdk();
  sdk.enqueue(scriptedQuery([{ type: "assistant", huge: "x".repeat(MAX_LINE_BYTES) }]));
  const { stdin, lines, exitCodes } = harness(sdk);
  send(stdin, { op: "user_turn", query_id: 1, text: "hi" });
  // writeLine now resolves on the write's completion callback (M6), which
  // can settle after the stdout 'data' event that populates `lines` — so
  // wait for exitCodes too, not just the fatal line, before asserting on it.
  await waitFor(() => lines.some((l) => (l as { ev?: string }).ev === "fatal") && exitCodes.length > 0);
  assert.equal((lines[lines.length - 1] as { reason?: string }).reason, "line_too_large");
  assert.deepEqual(exitCodes, [1]);
});

test("interrupt: ok true, sdk_return null when interrupt() resolves undefined", async () => {
  const sdk = new FakeSdk();
  sdk.enqueue(neverEndingQuery(() => Promise.resolve(undefined)));
  const { stdin, lines } = harness(sdk);
  send(stdin, { op: "user_turn", query_id: 1, text: "hi" }, { op: "interrupt", id: 42 });
  await waitFor(() => lines.some((l) => (l as { ev?: string }).ev === "interrupted"));
  const ev = lines.find((l) => (l as { ev?: string }).ev === "interrupted");
  assert.deepEqual(ev, { ev: "interrupted", id: 42, ok: true, sdk_return: null, note: "adapter-derived" });
});

test("interrupt: sdk_return carries the projected resolved value when not undefined", async () => {
  const sdk = new FakeSdk();
  sdk.enqueue(neverEndingQuery(() => Promise.resolve({ still_queued: [] })));
  const { stdin, lines } = harness(sdk);
  send(stdin, { op: "user_turn", query_id: 1, text: "hi" }, { op: "interrupt", id: 1 });
  await waitFor(() => lines.some((l) => (l as { ev?: string }).ev === "interrupted"));
  const ev = lines.find((l) => (l as { ev?: string }).ev === "interrupted") as { sdk_return?: unknown };
  assert.deepEqual(ev.sdk_return, { still_queued: [] });
});

test("interrupt: ok false when interrupt() rejects; never SDK confirmation", async () => {
  const sdk = new FakeSdk();
  sdk.enqueue(neverEndingQuery(() => Promise.reject(new Error("nope"))));
  const { stdin, lines } = harness(sdk);
  send(stdin, { op: "user_turn", query_id: 1, text: "hi" }, { op: "interrupt", id: 2 });
  await waitFor(() => lines.some((l) => (l as { ev?: string }).ev === "interrupted"));
  const ev = lines.find((l) => (l as { ev?: string }).ev === "interrupted");
  assert.deepEqual(ev, { ev: "interrupted", id: 2, ok: false, sdk_return: null, note: "adapter-derived" });
});

test("interrupt with no in-flight query still answers, ok false", async () => {
  const { stdin, lines } = harness(new FakeSdk());
  send(stdin, { op: "interrupt", id: 5 });
  await waitFor(() => lines.some((l) => (l as { ev?: string }).ev === "interrupted"));
  const ev = lines.find((l) => (l as { ev?: string }).ev === "interrupted");
  assert.deepEqual(ev, { ev: "interrupted", id: 5, ok: false, sdk_return: null, note: "adapter-derived" });
});

test("a second user_turn while one is in flight fatals as busy", async () => {
  const sdk = new FakeSdk();
  sdk.enqueue(neverEndingQuery(() => Promise.resolve(undefined)));
  const { stdin, lines, exitCodes } = harness(sdk);
  send(stdin, { op: "user_turn", query_id: 1, text: "hi" }, { op: "user_turn", query_id: 2, text: "again" });
  // writeLine now resolves on the write's completion callback (M6), which
  // can settle after the stdout 'data' event that populates `lines` — so
  // wait for exitCodes too, not just the fatal line, before asserting on it.
  await waitFor(() => lines.some((l) => (l as { ev?: string }).ev === "fatal") && exitCodes.length > 0);
  assert.equal((lines[lines.length - 1] as { reason?: string }).reason, "busy");
  assert.deepEqual(exitCodes, [1]);
});

test("an unknown op fatals as protocol, with no detail field", async () => {
  const { stdin, lines, exitCodes } = harness(new FakeSdk());
  send(stdin, { op: "not_a_real_op" });
  // writeLine now resolves on the write's completion callback (M6), which
  // can settle after the stdout 'data' event that populates `lines` — so
  // wait for exitCodes too, not just the fatal line, before asserting on it.
  await waitFor(() => lines.some((l) => (l as { ev?: string }).ev === "fatal") && exitCodes.length > 0);
  const ev = lines.find((l) => (l as { ev?: string }).ev === "fatal") as Record<string, unknown>;
  assert.equal(ev.reason, "protocol");
  assert.equal("detail" in ev, false);
  assert.deepEqual(exitCodes, [1]);
});

test("a malformed input line fatals as protocol", async () => {
  const { stdin, lines, exitCodes } = harness(new FakeSdk());
  stdin.write("{not json\n");
  // writeLine now resolves on the write's completion callback (M6), which
  // can settle after the stdout 'data' event that populates `lines` — so
  // wait for exitCodes too, not just the fatal line, before asserting on it.
  await waitFor(() => lines.some((l) => (l as { ev?: string }).ev === "fatal") && exitCodes.length > 0);
  assert.deepEqual(exitCodes, [1]);
});

test("unterminated input past the 8 MiB cap fatals as protocol, never grows memory unbounded", async () => {
  const { stdin, lines, exitCodes } = harness(new FakeSdk());
  stdin.write("x".repeat(MAX_LINE_BYTES + 1)); // no trailing newline, ever
  // writeLine now resolves on the write's completion callback (M6), which
  // can settle after the stdout 'data' event that populates `lines` — so
  // wait for exitCodes too, not just the fatal line, before asserting on it.
  await waitFor(() => lines.some((l) => (l as { ev?: string }).ev === "fatal") && exitCodes.length > 0);
  const fatal = lines.find((l) => (l as { ev?: string }).ev === "fatal") as Record<string, unknown>;
  assert.equal(fatal.reason, "protocol");
  assert.deepEqual(exitCodes, [1]);
});

test("shutdown with nothing in flight exits 0 with no fatal", async () => {
  const { stdin, lines, exitCodes, donePromise } = harness(new FakeSdk());
  send(stdin, { op: "shutdown" });
  await donePromise;
  assert.deepEqual(exitCodes, [0]);
  assert.equal(lines.some((l) => (l as { ev?: string }).ev === "fatal"), false);
});

test("stdin EOF aborts an in-flight query and exits 0 without a fatal", async () => {
  let interruptCalled = false;
  const sdk = new FakeSdk();
  sdk.enqueue(
    neverEndingQuery(() => {
      interruptCalled = true;
      return Promise.resolve(undefined);
    }),
  );
  const { stdin, lines, exitCodes, donePromise } = harness(sdk);
  send(stdin, { op: "user_turn", query_id: 1, text: "hi" });
  await waitFor(() => sdk.calls.length === 1);
  stdin.end();
  await donePromise;
  assert.deepEqual(exitCodes, [0]);
  assert.equal(interruptCalled, true);
  assert.equal(lines.some((l) => (l as { ev?: string }).ev === "fatal"), false);
});
