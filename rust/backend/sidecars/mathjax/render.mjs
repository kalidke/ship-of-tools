// MathJax sidecar — line-delimited JSON over stdio.
//
// Per ADR 0012 the Ship of Tools frontend renders inline LaTeX as MathJax-SVG, so
// the backend stands up this node process once and pipes requests through
// it. Stdio framing: one JSON object per request line on stdin, one JSON
// object per response line on stdout. The backend wraps this in a tokio
// supervisor (see rust/backend/src/mathjax.rs).
//
// Wire format:
//   request:  {"id":<u64>, "tex":"<latex>", "display":<bool>}
//   ok:       {"id":<u64>, "svg":"<svg>", "ex":<exFactor>}
//   err:      {"id":<u64>, "error":"<message>"}
//
// We never emit JSON on stderr; stderr is for free-text diagnostics only.
// The supervisor uses stdout-only as the result channel.

import { mathjax } from "mathjax-full/js/mathjax.js";
import { TeX } from "mathjax-full/js/input/tex.js";
import { SVG } from "mathjax-full/js/output/svg.js";
import { liteAdaptor } from "mathjax-full/js/adaptors/liteAdaptor.js";
import { RegisterHTMLHandler } from "mathjax-full/js/handlers/html.js";
import { AllPackages } from "mathjax-full/js/input/tex/AllPackages.js";

const adaptor = liteAdaptor();
RegisterHTMLHandler(adaptor);

const tex = new TeX({ packages: AllPackages });
const svgOut = new SVG({ fontCache: "none" }); // fontCache "none" so each
// SVG is self-contained — resvg won't see cross-document <defs> refs.
const doc = mathjax.document("", { InputJax: tex, OutputJax: svgOut });

process.stderr.write("mathjax sidecar ready\n");

let buf = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk) => {
  buf += chunk;
  let idx;
  while ((idx = buf.indexOf("\n")) >= 0) {
    const line = buf.slice(0, idx);
    buf = buf.slice(idx + 1);
    if (!line.trim()) continue;
    handleLine(line);
  }
});

process.stdin.on("end", () => {
  process.exit(0);
});

function handleLine(line) {
  let req;
  try {
    req = JSON.parse(line);
  } catch (e) {
    // Unparseable input — emit a generic error so the supervisor can advance
    // its in-flight queue. id=0 means "unattributed", caller should drop.
    process.stdout.write(JSON.stringify({ id: 0, error: `bad request: ${e.message}` }) + "\n");
    return;
  }
  const { id, tex: latex, display } = req;
  if (typeof id !== "number" || typeof latex !== "string") {
    process.stdout.write(JSON.stringify({ id: id || 0, error: "missing id/tex" }) + "\n");
    return;
  }

  try {
    const node = doc.convert(latex, { display: !!display, em: 16, ex: 8 });
    // Pull out the actual <svg>...</svg> string. liteAdaptor's outerHTML
    // wraps in a <mjx-container> by default; we strip down to the bare svg
    // so resvg/usvg consumes it directly without DOM glue.
    let html = adaptor.outerHTML(node);
    const svgStart = html.indexOf("<svg");
    const svgEnd = html.lastIndexOf("</svg>");
    if (svgStart >= 0 && svgEnd > svgStart) {
      html = html.slice(svgStart, svgEnd + "</svg>".length);
    }
    // MathJax SVG uses ex-units; tell the caller the conversion factor so
    // it can size the output relative to surrounding text. Best-effort:
    // pulled from the root style attribute when present.
    const exMatch = html.match(/style="vertical-align: -?[\d.]+ex.*?"/);
    process.stdout.write(JSON.stringify({ id, svg: html, ex: 8 }) + "\n");
  } catch (e) {
    process.stdout.write(JSON.stringify({ id, error: `mathjax: ${e.message}` }) + "\n");
  }
}
