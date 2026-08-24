/**
 * Ship's Log P2 adapter: the per-capsule helper process. Owns one Claude
 * Agent SDK session across many turns; speaks NDJSON helper-protocol 1 on
 * stdio. See ../README.md for the normative protocol.
 *
 * The real SDK is wired behind SdkLike so `run()` is testable with a fake.
 */

import { query as sdkQuery, type SDKUserMessage } from "@anthropic-ai/claude-agent-sdk";
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";
import { project, type Json } from "./codec.js";
import {
  LineReader,
  LineTooLargeError,
  fatalLine,
  helloLine,
  interruptedLine,
  msgLine,
  parseOp,
  turnEndLine,
  type FatalReason,
} from "./protocol.js";

export interface SdkQueryOpts {
  prompt: AsyncIterable<SDKUserMessage>;
  options: {
    permissionMode: "bypassPermissions";
    allowDangerouslySkipPermissions: true;
    includePartialMessages: false;
    model?: string;
    resume?: string;
  };
}

export interface SdkQuery extends AsyncIterable<Record<string, unknown>> {
  interrupt(): Promise<unknown>;
}

export interface SdkLike {
  query(opts: SdkQueryOpts): SdkQuery;
}

const realSdk: SdkLike = {
  query: (opts) => sdkQuery(opts) as unknown as SdkQuery,
};

/**
 * Streaming-input mode, one message, then done: the exact SDKUserMessage
 * shape from sdk.d.ts (only `type`, `message`, `parent_tool_use_id` are
 * required; every other field is optional and omitted here). This is what
 * makes `interrupt()` a *supported* call per the SDK's own doc note ("only
 * supported when streaming input/output is used") while still satisfying
 * the exactly-one-result-then-exhaustion turn contract: one input in the
 * iterable is one result barrier, same as single-prompt mode was.
 */
async function* oneShotPrompt(text: string): AsyncGenerator<SDKUserMessage> {
  yield { type: "user", message: { role: "user", content: text }, parent_tool_use_id: null };
}

/** "<from package>": our own pinned dependency entry, the source of truth for the exact SDK build in use. */
function helperSdkVersion(): string {
  const require = createRequire(import.meta.url);
  const pkg = require("../../package.json") as { dependencies?: Record<string, string> };
  return pkg.dependencies?.["@anthropic-ai/claude-agent-sdk"] ?? "unknown";
}

const SDK_VERSION = helperSdkVersion();

/**
 * 64 MiB per turn (sum of every emitted "msg" line's wire bytes, reset per
 * turn) — the spool bound from ADR 0040's Codec section. Exceeding it is a
 * terminal fatal ("turn_too_large"), never a truncation: the line that
 * would cross the bound is never written.
 *
 * HELPER_TEST_TURN_CAP overrides the default — test-only, read once at
 * module load, and never consulted by the real capsule wiring (the capsule
 * only ever sets HELPER_MODEL). It exists because 64 MiB of scripted test
 * messages would be impractical to actually construct and push through a
 * PassThrough in a unit test; a lowered cap lets the same code path be
 * exercised with a handful of small messages instead.
 */
const DEFAULT_TURN_SPOOL_CAP_BYTES = 64 * 1024 * 1024;
export const TURN_SPOOL_CAP_BYTES = readTurnSpoolCap();

function readTurnSpoolCap(): number {
  const override = process.env.HELPER_TEST_TURN_CAP;
  if (override === undefined) return DEFAULT_TURN_SPOOL_CAP_BYTES;
  const n = Number(override);
  return Number.isFinite(n) && n > 0 ? n : DEFAULT_TURN_SPOOL_CAP_BYTES;
}

/**
 * Runs the helper protocol loop over injected I/O. Resolves once the loop
 * has decided to end (fatal, shutdown, or stdin EOF) and has called `exit`;
 * `exit` defaults to process.exit but is injectable so tests can observe the
 * decision without killing the test runner.
 */
export function run(
  sdk: SdkLike,
  stdin: NodeJS.ReadableStream,
  stdout: NodeJS.WritableStream,
  env: NodeJS.ProcessEnv,
  exit: (code: number) => void = (code) => process.exit(code),
): Promise<void> {
  return new Promise((resolveRun) => {
    let pinnedSessionId: string | null = null;
    let inFlight: { queryId: number; handle: SdkQuery; interrupted: boolean } | null = null;
    let exiting = false;
    const reader = new LineReader();

    /**
     * Resolves on the write's own completion callback — the data has been
     * handed to the OS, not merely queued in this process — not on write()'s
     * boolean return value. That return value only reports whether the
     * internal buffer is under the highWaterMark; on platforms where a pipe
     * write can complete asynchronously even when it returns true, resolving
     * on that instead could let a caller call process.exit() (e.g. after the
     * final fatal line) before the bytes actually left the process.
     */
    function writeLine(text: string): Promise<void> {
      return new Promise((resolve, reject) => {
        stdout.write(text, (err) => (err ? reject(err) : resolve()));
      });
    }

    /** Builds and writes one line; on LineTooLargeError it fails the helper and returns false. */
    async function emit(build: () => string): Promise<boolean> {
      let text: string;
      try {
        text = build();
      } catch (err) {
        if (err instanceof LineTooLargeError) {
          await fail("line_too_large", "a line exceeded the 8 MiB cap");
          return false;
        }
        throw err;
      }
      await writeLine(text);
      return true;
    }

    async function fail(reason: FatalReason, detail?: string): Promise<void> {
      if (exiting) return;
      exiting = true;
      await writeLine(fatalLine(reason, detail));
      resolveRun();
      exit(1);
    }

    function shutdown(): void {
      if (exiting) return;
      exiting = true;
      resolveRun();
      exit(0);
    }

    async function handleUserTurn(op: { query_id: number; text: string }): Promise<void> {
      if (inFlight) return fail("busy", "a user_turn was received while one was already in flight");
      const options: SdkQueryOpts["options"] = {
        permissionMode: "bypassPermissions",
        allowDangerouslySkipPermissions: true,
        includePartialMessages: false,
      };
      if (env.HELPER_MODEL) options.model = env.HELPER_MODEL;
      if (pinnedSessionId !== null) options.resume = pinnedSessionId;
      const handle = sdk.query({ prompt: oneShotPrompt(op.text), options });
      inFlight = { queryId: op.query_id, handle, interrupted: false };
      let resultCount = 0;
      let spooledBytes = 0;
      try {
        for await (const raw of handle) {
          if (exiting) {
            inFlight = null;
            return;
          }
          const m = raw as Record<string, unknown>;
          if (typeof m.session_id === "string") {
            if (pinnedSessionId === null) {
              if (m.type === "system" && m.subtype === "init") pinnedSessionId = m.session_id;
            } else if (m.session_id !== pinnedSessionId) {
              inFlight = null;
              return fail("session_drift", "a message's session_id did not match the resumed session");
            }
          }
          if (m.type === "result" && ++resultCount > 1) {
            inFlight = null;
            return fail("multi_result", "more than one result message was observed in a single turn");
          }
          let body: Json;
          try {
            body = project(raw);
          } catch {
            inFlight = null;
            return fail("serializer", "an SDK message could not be projected to JSON");
          }
          let text: string;
          try {
            text = msgLine(body);
          } catch (err) {
            inFlight = null;
            if (err instanceof LineTooLargeError) {
              return fail("line_too_large", "a projected message line exceeded the 8 MiB cap");
            }
            throw err;
          }
          spooledBytes += Buffer.byteLength(text, "utf8");
          if (spooledBytes > TURN_SPOOL_CAP_BYTES) {
            inFlight = null;
            return fail("turn_too_large", "the turn's cumulative msg output exceeded the per-turn spool cap");
          }
          await writeLine(text);
        }
      } catch (err) {
        inFlight = null;
        return fail("query_error", err instanceof Error ? err.constructor.name : "unknown error");
      }
      const wasInterrupted = inFlight.interrupted;
      inFlight = null;
      if (resultCount === 0) {
        if (wasInterrupted) {
          await emit(() => turnEndLine(op.query_id, 0));
          return;
        }
        return fail("no_result", "the turn's iterator exhausted without a result message");
      }
      await emit(() => turnEndLine(op.query_id, resultCount));
    }

    async function handleInterrupt(op: { id: number }): Promise<void> {
      if (!inFlight) {
        await emit(() => interruptedLine(op.id, false, null));
        return;
      }
      inFlight.interrupted = true;
      const handle = inFlight.handle;
      let ok: boolean;
      let raw: unknown;
      try {
        raw = await handle.interrupt();
        ok = true;
      } catch {
        ok = false;
        raw = undefined;
      }
      let sdkReturn: Json | null = null;
      if (ok && raw !== undefined) {
        try {
          sdkReturn = project(raw);
        } catch {
          return fail("serializer", "the interrupt() return value could not be projected to JSON");
        }
      }
      await emit(() => interruptedLine(op.id, ok, sdkReturn));
    }

    async function handleLine(line: string): Promise<void> {
      if (exiting) return;
      const parsed = parseOp(line);
      if (!parsed.ok) {
        await fail("protocol");
        return;
      }
      const op = parsed.value;
      if (op.op === "user_turn") {
        await handleUserTurn(op);
        return;
      }
      if (op.op === "interrupt") {
        await handleInterrupt(op);
        return;
      }
      shutdown();
    }

    stdin.setEncoding("utf8");
    stdin.on("data", (chunk: string) => {
      let lines: string[];
      try {
        lines = reader.push(chunk);
      } catch (err) {
        if (err instanceof LineTooLargeError) {
          void fail("protocol", "input exceeded the 8 MiB line cap without completing a line");
          return;
        }
        throw err;
      }
      for (const line of lines) void handleLine(line);
    });
    stdin.on("end", () => {
      if (exiting) return;
      exiting = true;
      if (inFlight) void inFlight.handle.interrupt().catch(() => {});
      resolveRun();
      exit(0);
    });

    void emit(() => helloLine(SDK_VERSION));
  });
}

function isMainModule(): boolean {
  return process.argv[1] !== undefined && import.meta.url === pathToFileURL(process.argv[1]).href;
}

if (isMainModule()) {
  void run(realSdk, process.stdin, process.stdout, process.env);
}
