/**
 * Pure NDJSON wire layer for Ship's Log P2 helper-protocol 1. No SDK import —
 * testable without credentials. See ../README.md for the normative protocol
 * this implements.
 */

import type { Json } from "./codec.js";

export const PROTOCOL_VERSION = 1;
export const MAX_LINE_BYTES = 8 * 1024 * 1024;

export type FatalReason =
  | "protocol"
  | "no_result"
  | "multi_result"
  | "session_drift"
  | "query_error"
  | "busy"
  | "serializer"
  | "line_too_large"
  | "turn_too_large";

export type InOp =
  | { op: "user_turn"; query_id: number; text: string }
  | { op: "interrupt"; id: number }
  | { op: "shutdown" };

export type ParsedOp = { ok: true; value: InOp } | { ok: false };

/** Thrown by the encode*Line helpers when a line exceeds MAX_LINE_BYTES after stringify. */
export class LineTooLargeError extends Error {
  constructor() {
    super("line exceeds the 8 MiB cap");
    this.name = "LineTooLargeError";
  }
}

function isU53(x: unknown): x is number {
  return typeof x === "number" && Number.isInteger(x) && x >= 0 && x <= Number.MAX_SAFE_INTEGER;
}

/** Parse one NDJSON input line into a typed op. ok:false ⇒ malformed or unknown op. */
export function parseOp(line: string): ParsedOp {
  let obj: unknown;
  try {
    obj = JSON.parse(line);
  } catch {
    return { ok: false };
  }
  if (typeof obj !== "object" || obj === null || Array.isArray(obj)) return { ok: false };
  const rec = obj as Record<string, unknown>;
  switch (rec.op) {
    case "user_turn":
      if (isU53(rec.query_id) && typeof rec.text === "string") {
        return { ok: true, value: { op: "user_turn", query_id: rec.query_id, text: rec.text } };
      }
      return { ok: false };
    case "interrupt":
      if (isU53(rec.id)) return { ok: true, value: { op: "interrupt", id: rec.id } };
      return { ok: false };
    case "shutdown":
      return { ok: true, value: { op: "shutdown" } };
    default:
      return { ok: false };
  }
}

/** The 8 MiB cap covers the wire bytes actually written, so it includes the trailing newline. */
function encodeEnvelope(obj: Record<string, unknown>): string {
  const line = JSON.stringify(obj) + "\n";
  if (Buffer.byteLength(line, "utf8") > MAX_LINE_BYTES) throw new LineTooLargeError();
  return line;
}

export function helloLine(sdkVersion: string): string {
  return encodeEnvelope({ ev: "hello", protocol: PROTOCOL_VERSION, sdk_version: sdkVersion });
}

export function msgLine(body: Json): string {
  return encodeEnvelope({ ev: "msg", body });
}

export function turnEndLine(queryId: number, results: number): string {
  return encodeEnvelope({ ev: "turn_end", query_id: queryId, results });
}

export function interruptedLine(id: number, ok: boolean, sdkReturn: Json | null): string {
  return encodeEnvelope({ ev: "interrupted", id, ok, sdk_return: sdkReturn, note: "adapter-derived" });
}

/** `detail` is omitted for reason "protocol", matching the spec's own example. */
export function fatalLine(reason: FatalReason, detail?: string): string {
  const obj: Record<string, unknown> = { ev: "fatal", reason };
  if (detail !== undefined) obj.detail = detail;
  return encodeEnvelope(obj);
}

/**
 * Buffers stdin chunks into complete \n-terminated NDJSON lines (a trailing
 * \r is trimmed). Input is capped at the same 8 MiB as an output line: a
 * peer that sends that much data without ever completing a line throws
 * LineTooLargeError rather than growing #buf without bound. The caller (see
 * main.ts) maps that to fatal reason "protocol" — a misbehaving peer, not
 * one of our own output lines being too large.
 */
export class LineReader {
  #buf = "";
  push(chunk: string): string[] {
    this.#buf += chunk;
    if (Buffer.byteLength(this.#buf, "utf8") > MAX_LINE_BYTES) throw new LineTooLargeError();
    const lines = this.#buf.split("\n");
    this.#buf = lines.pop() ?? "";
    return lines.map((l) => (l.endsWith("\r") ? l.slice(0, -1) : l));
  }
}
