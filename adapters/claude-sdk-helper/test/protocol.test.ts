import assert from "node:assert/strict";
import { test } from "node:test";
import {
  LineReader,
  LineTooLargeError,
  MAX_LINE_BYTES,
  fatalLine,
  helloLine,
  interruptedLine,
  msgLine,
  parseOp,
  turnEndLine,
} from "../src/protocol.js";

test("parseOp accepts a well-formed user_turn", () => {
  const r = parseOp(JSON.stringify({ op: "user_turn", query_id: 1, text: "hi" }));
  assert.deepEqual(r, { ok: true, value: { op: "user_turn", query_id: 1, text: "hi" } });
});

test("parseOp accepts a well-formed interrupt", () => {
  const r = parseOp(JSON.stringify({ op: "interrupt", id: 7 }));
  assert.deepEqual(r, { ok: true, value: { op: "interrupt", id: 7 } });
});

test("parseOp accepts shutdown", () => {
  const r = parseOp(JSON.stringify({ op: "shutdown" }));
  assert.deepEqual(r, { ok: true, value: { op: "shutdown" } });
});

test("parseOp rejects malformed JSON", () => {
  assert.deepEqual(parseOp("{not json"), { ok: false });
});

test("parseOp rejects an unknown op", () => {
  assert.deepEqual(parseOp(JSON.stringify({ op: "wat" })), { ok: false });
});

test("parseOp rejects a top-level JSON array or scalar", () => {
  assert.deepEqual(parseOp("[]"), { ok: false });
  assert.deepEqual(parseOp("42"), { ok: false });
});

test("parseOp rejects user_turn with a non-u53 query_id or non-string text", () => {
  assert.deepEqual(parseOp(JSON.stringify({ op: "user_turn", query_id: -1, text: "hi" })), { ok: false });
  assert.deepEqual(parseOp(JSON.stringify({ op: "user_turn", query_id: 1.5, text: "hi" })), { ok: false });
  assert.deepEqual(parseOp(JSON.stringify({ op: "user_turn", query_id: 1, text: 5 })), { ok: false });
  assert.deepEqual(parseOp(JSON.stringify({ op: "user_turn", text: "hi" })), { ok: false });
});

test("parseOp rejects interrupt with a non-u53 id", () => {
  assert.deepEqual(parseOp(JSON.stringify({ op: "interrupt", id: -1 })), { ok: false });
  assert.deepEqual(parseOp(JSON.stringify({ op: "interrupt", id: "1" })), { ok: false });
});

test("parseOp accepts u53 at the boundary and rejects one past it", () => {
  const max = Number.MAX_SAFE_INTEGER;
  assert.equal(parseOp(JSON.stringify({ op: "interrupt", id: max })).ok, true);
  assert.equal(parseOp(JSON.stringify({ op: "interrupt", id: max + 1024 })).ok, false);
});

test("helloLine carries protocol 1 and the sdk version", () => {
  const line = helloLine("0.3.241");
  assert.equal(line.endsWith("\n"), true);
  assert.deepEqual(JSON.parse(line), { ev: "hello", protocol: 1, sdk_version: "0.3.241" });
});

test("msgLine wraps the projected body", () => {
  const line = msgLine({ type: "assistant" });
  assert.deepEqual(JSON.parse(line), { ev: "msg", body: { type: "assistant" } });
});

test("turnEndLine carries the query id", () => {
  assert.deepEqual(JSON.parse(turnEndLine(9)), { ev: "turn_end", query_id: 9 });
});

test("interruptedLine always carries adapter-derived note, and null sdk_return renders as null", () => {
  assert.deepEqual(JSON.parse(interruptedLine(3, true, null)), {
    ev: "interrupted",
    id: 3,
    ok: true,
    sdk_return: null,
    note: "adapter-derived",
  });
  assert.deepEqual(JSON.parse(interruptedLine(3, false, null)).ok, false);
});

test("fatalLine omits detail for reason protocol, matching the spec's own example", () => {
  const parsed = JSON.parse(fatalLine("protocol"));
  assert.deepEqual(parsed, { ev: "fatal", reason: "protocol" });
  assert.equal("detail" in parsed, false);
});

test("fatalLine includes detail when given", () => {
  assert.deepEqual(JSON.parse(fatalLine("busy", "already in flight")), {
    ev: "fatal",
    reason: "busy",
    detail: "already in flight",
  });
});

test("a line exceeding the 8 MiB cap throws LineTooLargeError", () => {
  const huge = "x".repeat(MAX_LINE_BYTES);
  assert.throws(() => msgLine({ huge }), LineTooLargeError);
});

test("a line at exactly the cap does not throw", () => {
  // Budget for the JSON wrapper `{"ev":"msg","body":"..."}` around the string.
  const overhead = Buffer.byteLength(JSON.stringify({ ev: "msg", body: "" }), "utf8");
  const body = "x".repeat(MAX_LINE_BYTES - overhead);
  assert.doesNotThrow(() => msgLine(body));
});

test("LineReader buffers partial lines and yields complete ones in order", () => {
  const r = new LineReader();
  assert.deepEqual(r.push('{"a":1}\n{"b":2'), ['{"a":1}']);
  assert.deepEqual(r.push('}\n{"c":3}\n'), ['{"b":2}', '{"c":3}']);
  assert.deepEqual(r.push(""), []);
});

test("LineReader trims a trailing carriage return", () => {
  const r = new LineReader();
  assert.deepEqual(r.push('{"a":1}\r\n'), ['{"a":1}']);
});

test("LineReader handles multiple lines arriving in one chunk", () => {
  const r = new LineReader();
  assert.deepEqual(r.push("one\ntwo\nthree\n"), ["one", "two", "three"]);
});
