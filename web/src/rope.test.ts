import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { Rope, UndoStack, diffEdit, applyEdit } from "./rope.js";

// Deterministic PRNG (mulberry32) so the fuzz run is reproducible.
function rng(seed: number): () => number {
  let s = seed >>> 0;
  return () => {
    s |= 0;
    s = (s + 0x6d2b79f5) | 0;
    let t = Math.imul(s ^ (s >>> 15), 1 | s);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const ALPHABET = "abcdef \n#*-0123456789";

function randomText(rand: () => number, maxLen: number): string {
  const len = Math.floor(rand() * maxLen);
  let out = "";
  for (let i = 0; i < len; i++) out += ALPHABET[Math.floor(rand() * ALPHABET.length)];
  return out;
}

describe("Rope", () => {
  it("matches naive string ops under random edits", () => {
    const rand = rng(42);
    let model = "The quick brown fox\n# Markdown *test*\n";
    const rope = Rope.fromString(model);
    for (let i = 0; i < 2000; i++) {
      const kind = rand();
      const pos = Math.floor(rand() * (model.length + 1));
      if (kind < 0.5) {
        const text = randomText(rand, 24);
        model = model.slice(0, pos) + text + model.slice(pos);
        rope.insert(pos, text);
      } else {
        const end = pos + Math.floor(rand() * 24);
        model = model.slice(0, pos) + model.slice(end);
        rope.delete(pos, end);
      }
      assert.equal(rope.toString(), model);
      assert.equal(rope.length, model.length);
      if (model.length > 0) {
        const at = Math.floor(rand() * model.length);
        assert.equal(rope.charAt(at), model[at]);
        const b = at + Math.floor(rand() * 24);
        assert.equal(rope.slice(at, b), model.slice(at, b));
      }
    }
  });

  it("clamps out-of-range edits and slices", () => {
    const rope = Rope.fromString("abc");
    rope.insert(99, "d");
    assert.equal(rope.toString(), "abcd");
    assert.equal(rope.delete(-5, 2), "ab");
    assert.equal(rope.toString(), "cd");
    assert.equal(rope.slice(5, 9), "");
    assert.equal(rope.charAt(99), "");
  });

  it("diffEdit produces minimal scripts", () => {
    assert.deepEqual(diffEdit("abc", "abc"), []);
    assert.deepEqual(diffEdit("ab", "acb"), [{ t: "insert", pos: 1, text: "c" }]);
    const ops = diffEdit("hello world", "hello brave world");
    assert.equal(ops.length, 1);
    assert.equal(ops[0].t, "insert");
  });
});

describe("UndoStack", () => {
  it("undoes and redoes whole keystrokes", () => {
    const rope = Rope.fromString("");
    const stack = new UndoStack();
    applyEdit(rope, stack, "", "Hello");
    applyEdit(rope, stack, "Hello", "Hello brave");
    applyEdit(rope, stack, "Hello brave", "Hello brave new world");
    assert.ok(stack.canUndo);
    assert.ok(!stack.canRedo);
    assert.ok(stack.undoOnce(rope));
    assert.equal(rope.toString(), "Hello brave");
    assert.ok(stack.undoOnce(rope));
    assert.equal(rope.toString(), "Hello");
    assert.ok(stack.redoOnce(rope));
    assert.equal(rope.toString(), "Hello brave");
    // New edit clears the redo stack.
    applyEdit(rope, stack, "Hello brave", "Hello brave!");
    assert.ok(!stack.canRedo);
    assert.ok(stack.undoOnce(rope));
    assert.equal(rope.toString(), "Hello brave");
  });

  it("undoes replacements in one step", () => {
    const rope = Rope.fromString("aaa bbb");
    const stack = new UndoStack();
    applyEdit(rope, stack, "aaa bbb", "aaa ccc");
    assert.equal(rope.toString(), "aaa ccc");
    assert.ok(stack.undoOnce(rope));
    assert.equal(rope.toString(), "aaa bbb");
  });

  it("survives serialisation round-trips", () => {
    const rope = Rope.fromString("abcdef");
    const stack = new UndoStack();
    applyEdit(rope, stack, "abcdef", "abXYZef");
    const saved = JSON.parse(JSON.stringify(stack.serialize()));
    const restored = UndoStack.load(saved);
    assert.ok(restored.canUndo);
    assert.ok(restored.undoOnce(rope));
    assert.equal(rope.toString(), "abcdef");
    const bad = UndoStack.load({ undo: [{ t: "nope", pos: 0, text: "x" }], redo: null });
    assert.ok(!bad.canUndo);
  });

  it("matches a naive model through fuzzed undo/redo", () => {
    const rand = rng(7);
    const rope = Rope.fromString("seed text here");
    const stack = new UndoStack();
    const history: string[] = ["seed text here"];
    for (let i = 0; i < 500; i++) {
      const roll = rand();
      if (roll < 0.6) {
        const cur = history[history.length - 1];
        const pos = Math.floor(rand() * (cur.length + 1));
        const next =
          rand() < 0.5
            ? cur.slice(0, pos) + randomText(rand, 12) + cur.slice(pos)
            : cur.slice(0, pos) + cur.slice(pos + Math.floor(rand() * 12));
        applyEdit(rope, stack, cur, next);
        history.push(next);
      } else if (roll < 0.8 && stack.canUndo) {
        stack.undoOnce(rope);
        history.push(rope.toString());
      } else if (stack.canRedo) {
        stack.redoOnce(rope);
        history.push(rope.toString());
      }
      assert.equal(rope.toString(), history[history.length - 1]);
    }
  });
});
