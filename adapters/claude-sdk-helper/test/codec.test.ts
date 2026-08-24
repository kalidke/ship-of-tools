import assert from "node:assert/strict";
import { test } from "node:test";
import { project, SerializerError } from "../src/codec.js";

test("passes primitives and finite fractional numbers through unchanged", () => {
  assert.equal(project("hi"), "hi");
  assert.equal(project(true), true);
  assert.equal(project(false), false);
  assert.equal(project(null), null);
  assert.equal(project(0), 0);
  assert.equal(project(-3.5), -3.5);
  assert.equal(project(1.2345e2 / 100), 1.2345);
});

test("walks only enumerable own properties of plain objects", () => {
  const obj: Record<string, unknown> = { own: 1 };
  Object.defineProperty(obj, "hidden", { value: 2, enumerable: false });
  assert.deepEqual(project(obj), { own: 1 });
});

test("does not walk inherited enumerable properties", () => {
  const key = "__test_inherited__";
  (Object.prototype as unknown as Record<string, unknown>)[key] = "nope";
  try {
    assert.deepEqual(project({ own: 1 }), { own: 1 });
  } finally {
    delete (Object.prototype as unknown as Record<string, unknown>)[key];
  }
});

test("omits undefined and function object properties; nulls them in arrays", () => {
  const fn = () => 1;
  assert.deepEqual(project({ a: 1, b: undefined, c: fn }), { a: 1 });
  assert.deepEqual(project([1, undefined, fn, 2]), [1, null, null, 2]);
});

test("sparse array holes become null", () => {
  const arr = [1, , 3]; // eslint-disable-line no-sparse-arrays
  assert.deepEqual(project(arr), [1, null, 3]);
});

test("applies toJSON exactly once per node", () => {
  const value = {
    toJSON: () => ({ toJSON: () => "should never run" }),
  };
  // The outer node's toJSON result is walked directly as a plain object; its
  // own `toJSON` property is a function value and is therefore omitted, not
  // re-invoked as a second toJSON pass.
  assert.deepEqual(project(value), {});
});

test("toJSON is honored on nested values too, each as its own node", () => {
  const inner = { toJSON: () => "2026-08-24" };
  assert.deepEqual(project({ when: inner }), { when: "2026-08-24" });
});

test("throws SerializerError on bigint", () => {
  assert.throws(() => project(10n), SerializerError);
  assert.throws(() => project({ n: 10n }), SerializerError);
});

test("throws SerializerError on non-finite numbers", () => {
  assert.throws(() => project(NaN), SerializerError);
  assert.throws(() => project(Infinity), SerializerError);
  assert.throws(() => project(-Infinity), SerializerError);
});

test("throws SerializerError on a reference cycle", () => {
  const obj: Record<string, unknown> = { a: 1 };
  obj.self = obj;
  assert.throws(() => project(obj), SerializerError);

  const arr: unknown[] = [1];
  arr.push(arr);
  assert.throws(() => project(arr), SerializerError);
});

test("does not treat a shared (non-cyclic) reference as a cycle", () => {
  const shared = { v: 1 };
  assert.deepEqual(project({ a: shared, b: shared }), { a: { v: 1 }, b: { v: 1 } });
});

test("catches a cycle hidden behind toJSON, which manufactures a fresh wrapper every call", () => {
  // a.toJSON() returns a NEW object literal each call, so post-toJSON
  // identity never repeats — cycle detection must key on the PRE-toJSON
  // reference (`a` itself) to catch this, or it recurses until the real JS
  // call stack overflows instead of failing cleanly.
  const a: { toJSON: () => { self: unknown } } = {
    toJSON: () => ({ self: a }),
  };
  assert.throws(() => project(a), SerializerError);
});

test("depth bound: a structure nested well past the bound fatals cleanly, not a stack overflow", () => {
  let deep: unknown = { bottom: true };
  for (let i = 0; i < 500; i++) deep = { child: deep };
  assert.throws(() => project(deep), SerializerError);
});

test("depth bound: a structure well within the bound is unaffected", () => {
  let ok: unknown = { bottom: true };
  for (let i = 0; i < 50; i++) ok = { child: ok };
  assert.doesNotThrow(() => project(ok));
});

test("throws SerializerError on values that are neither array, plain object, nor primitive", () => {
  assert.throws(() => project(new Map()), SerializerError);
  assert.throws(() => project(new Set()), SerializerError);
  assert.throws(() => project(Symbol("x")), SerializerError);
  class NotPlain {
    x = 1;
  }
  assert.throws(() => project(new NotPlain()), SerializerError);
});

test("arrays project element-wise, recursing into nested structures", () => {
  assert.deepEqual(project([1, "two", { three: 3 }, [4, 5]]), [1, "two", { three: 3 }, [4, 5]]);
});
