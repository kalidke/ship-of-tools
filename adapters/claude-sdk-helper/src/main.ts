/**
 * Ship's Log P2 adapter: the per-capsule helper process. Owns one Claude
 * Agent SDK session across many turns; speaks NDJSON helper-protocol 1 on
 * stdio. See ../README.md for the normative protocol.
 *
 * The real SDK is wired behind SdkLike so `run()` is testable with a fake.
 */

import { query as sdkQuery } from "@anthropic-ai/claude-agent-sdk";
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
  prompt: string;
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

/** "<from package>": our own pinned dependency entry, the source of truth for the exact SDK build in use. */
function helperSdkVersion(): string {
  const require = createRequire(import.meta.url);
  const pkg = require("../../package.json") as { dependencies?: Record<string, string> };
  return pkg.dependencies?.["@anthropic-ai/claude-agent-sdk"] ?? "unknown";
}

const SDK_VERSION = helperSdkVersion();

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
    let inFlight: { queryId: number; handle: SdkQuery } | null = null;
    let exiting = false;
    const reader = new LineReader();

    function writeLine(text: string): Promise<void> {
      return new Promise((resolve) => {
        if (stdout.write(text)) resolve();
        else stdout.once("drain", () => resolve());
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
      const handle = sdk.query({ prompt: op.text, options });
      inFlight = { queryId: op.query_id, handle };
      let resultCount = 0;
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
          if (!(await emit(() => msgLine(body)))) {
            inFlight = null;
            return;
          }
        }
      } catch (err) {
        inFlight = null;
        return fail("query_error", err instanceof Error ? err.constructor.name : "unknown error");
      }
      inFlight = null;
      if (resultCount === 0) return fail("no_result", "the turn's iterator exhausted without a result message");
      await emit(() => turnEndLine(op.query_id));
    }

    async function handleInterrupt(op: { id: number }): Promise<void> {
      if (!inFlight) {
        await emit(() => interruptedLine(op.id, false, null));
        return;
      }
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
      for (const line of reader.push(chunk)) void handleLine(line);
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
