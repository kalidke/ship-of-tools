/**
 * Pure projection codec for Ship's Log P2 helper-protocol 1. No SDK import —
 * this module is testable without credentials.
 *
 * project() turns an arbitrary SDK value into the JSON tree that rides in
 * "msg"/"body" and "interrupted"/"sdk_return":
 *   - toJSON() is applied once per node, if present.
 *   - Only enumerable own properties are walked.
 *   - undefined/function values are OMITTED from objects, become null in
 *     arrays; sparse array holes become null too.
 *   - bigint, a non-finite number, a reference cycle, exceeding the depth
 *     bound, or anything that is still neither a JSON primitive, a plain
 *     object, nor an array after the toJSON step, throws SerializerError.
 *   - Finite fractional numbers pass through unchanged.
 *
 * Cycle detection keys on PRE-toJSON identity (the reference as received),
 * not the value toJSON() returns. A toJSON that manufactures a fresh
 * wrapper object each call (e.g. `a.toJSON = () => ({ self: a })`) would
 * otherwise never repeat an object identity in the post-toJSON stack, and
 * the walk would recurse until the real JS call stack overflows instead of
 * failing cleanly. Keying on the original reference catches it on the
 * second visit to `a`, before `a.toJSON()` is even called again. MAX_DEPTH
 * is a second, independent backstop against any other unbounded-recursion
 * shape (deeply nested non-repeating structures included).
 */

export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };

export class SerializerError extends Error {
  constructor(reason: string) {
    super(reason);
    this.name = "SerializerError";
  }
}

const MAX_DEPTH = 256;

function isPlainObject(v: object): boolean {
  const proto = Object.getPrototypeOf(v);
  return proto === Object.prototype || proto === null;
}

function resolveToJSON(v: unknown): unknown {
  if (v !== null && typeof v === "object") {
    const fn = (v as { toJSON?: unknown }).toJSON;
    if (typeof fn === "function") return (fn as () => unknown).call(v);
  }
  return v;
}

function projectNode(raw: unknown, stack: Set<object>, depth: number): Json {
  if (depth > MAX_DEPTH) throw new SerializerError("max depth exceeded");
  const isObjectRaw = raw !== null && typeof raw === "object";
  if (isObjectRaw && stack.has(raw)) throw new SerializerError("cycle");
  const v = resolveToJSON(raw);
  if (v === null) return null;
  const t = typeof v;
  if (t === "string" || t === "boolean") return v as string | boolean;
  if (t === "number") {
    if (!Number.isFinite(v as number)) throw new SerializerError("non-finite number");
    return v as number;
  }
  if (t === "bigint") throw new SerializerError("bigint");
  if (t === "object") {
    const obj = v as object;
    // isObjectRaw is guaranteed true here: resolveToJSON only ever turns an
    // object `raw` into an object `v` (primitives pass through untouched).
    const identity = raw as object;
    if (Array.isArray(obj)) {
      stack.add(identity);
      const out: Json[] = [];
      for (let i = 0; i < obj.length; i++) {
        if (!(i in obj)) {
          out.push(null);
          continue;
        }
        const el = (obj as unknown[])[i];
        if (el === undefined || typeof el === "function") {
          out.push(null);
          continue;
        }
        out.push(projectNode(el, stack, depth + 1));
      }
      stack.delete(identity);
      return out;
    }
    if (!isPlainObject(obj)) throw new SerializerError("non-plain-object/non-array after projection");
    stack.add(identity);
    const out: { [key: string]: Json } = {};
    for (const key of Object.keys(obj)) {
      const val = (obj as Record<string, unknown>)[key];
      if (val === undefined || typeof val === "function") continue;
      out[key] = projectNode(val, stack, depth + 1);
    }
    stack.delete(identity);
    return out;
  }
  // undefined, function, or symbol reaching the root of a projection call.
  throw new SerializerError("non-plain-object/non-array after projection");
}

/** Project an arbitrary SDK value into a JSON-safe tree per helper-protocol 1. */
export function project(value: unknown): Json {
  return projectNode(value, new Set(), 0);
}
