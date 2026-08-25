// Fake Anthropic Messages API for the P2 e2e rig (claude_e2e.rs): SSE
// streaming, zero deps, fully offline. The stack under test — the real
// claude-sdk-helper, the real pinned SDK, the real vendored CLI — is
// pointed here via ANTHROPIC_BASE_URL, so the ONLY fake is at the network
// boundary, the documented public HTTP contract.
//
// Behavior:
// - Prints the bound port to stdout as a single line (listen on argv[2],
//   0 = ephemeral), then serves until killed.
// - POST /v1/messages* answers a minimal valid SSE turn whose text is
//   "fixture reply <n>" (n = 1-based request ordinal).
// - STALL: if the request body contains the marker "SOT-STALL", the stream
//   sends message_start + one text delta and then HOLDS the connection open
//   until the client disconnects — a deterministic in-flight window for
//   the kill scenario. No timers.
// - DRIP: marker "SOT-DRIP" keeps the stream FLOWING — one text delta
//   every 500ms, forever — the realistic in-flight window for the
//   interrupt scenario (an abort against a live stream, not a dead one).
// - Everything else answers 200 {} (the CLI probes e.g. HEAD /api/hello).
// - Every request is logged to stderr for post-mortem.
import http from "node:http";

const PORT = Number(process.argv[2] ?? 0);
let nmsg = 0;

function event(res, name, data) {
  res.write(`event: ${name}\ndata: ${JSON.stringify(data)}\n\n`);
}

const srv = http.createServer((req, res) => {
  let body = "";
  req.on("data", (c) => (body += c));
  req.on("end", () => {
    console.error(`[fake-api] ${req.method} ${req.url} bytes=${body.length}`);
    if (!(req.method === "POST" && req.url.startsWith("/v1/messages"))) {
      res.writeHead(200, { "content-type": "application/json" });
      res.end("{}");
      return;
    }
    nmsg += 1;
    let parsed = {};
    try {
      parsed = JSON.parse(body);
    } catch {}
    const id = `msg_fake_${nmsg}`;
    const model = parsed.model ?? "claude-fake";
    res.writeHead(200, { "content-type": "text/event-stream" });
    event(res, "message_start", {
      type: "message_start",
      message: { id, type: "message", role: "assistant", model, content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: 10, output_tokens: 1 } },
    });
    event(res, "content_block_start", { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } });
    event(res, "content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: `fixture reply ${nmsg}` } });
    if (body.includes("SOT-STALL")) {
      console.error(`[fake-api] turn=${nmsg} stalling (holding the stream open)`);
      req.on("close", () => console.error(`[fake-api] turn=${nmsg} stalled client disconnected`));
      return; // never finish: the in-flight window stays open
    }
    if (body.includes("SOT-DRIP")) {
      console.error(`[fake-api] turn=${nmsg} dripping (500ms deltas forever)`);
      const drip = setInterval(() => {
        event(res, "content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text: " drip" } });
      }, 500);
      req.on("close", () => {
        clearInterval(drip);
        console.error(`[fake-api] turn=${nmsg} dripping client disconnected`);
      });
      return;
    }
    event(res, "content_block_stop", { type: "content_block_stop", index: 0 });
    event(res, "message_delta", { type: "message_delta", delta: { stop_reason: "end_turn", stop_sequence: null }, usage: { output_tokens: 5 } });
    event(res, "message_stop", { type: "message_stop" });
    res.end();
  });
});

srv.listen(PORT, "127.0.0.1", () => {
  console.log(String(srv.address().port));
});
