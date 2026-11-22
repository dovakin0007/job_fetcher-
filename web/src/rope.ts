/**
 * A small chunked-sequence rope for the resume source editor.
 *
 * Large markdown documents stay snappy because edits splice chunk arrays
 * instead of copying the whole string, and every edit records its inverse so
 * undo/redo are structural (no snapshots). The op stacks serialise to plain
 * JSON, which is what the backend persists per editing session.
 */

export type EditOp =
  | { t: "insert"; pos: number; text: string }
  | { t: "delete"; pos: number; text: string };

const CHUNK = 512;
const MAX_OPS = 100;

function clamp(n: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, n));
}

export class Rope {
  private chunks: string[];
  private len: number;

  constructor(chunks: string[] = []) {
    this.chunks = chunks.filter((c) => c.length > 0);
    this.len = this.chunks.reduce((n, c) => n + c.length, 0);
  }

  static fromString(text: string, chunkSize = CHUNK): Rope {
    if (!text) return new Rope([]);
    const chunks: string[] = [];
    for (let i = 0; i < text.length; i += chunkSize) {
      chunks.push(text.slice(i, i + chunkSize));
    }
    return new Rope(chunks);
  }

  get length(): number {
    return this.len;
  }

  toString(): string {
    return this.chunks.join("");
  }

  charAt(index: number): string {
    if (index < 0 || index >= this.len) return "";
    let offset = index;
    for (const chunk of this.chunks) {
      if (offset < chunk.length) return chunk[offset] ?? "";
      offset -= chunk.length;
    }
    return "";
  }

  slice(start: number, end = this.len): string {
    const a = clamp(start, 0, this.len);
    const b = clamp(end, 0, this.len);
    if (b <= a) return "";
    let out = "";
    let pos = 0;
    for (const chunk of this.chunks) {
      const end2 = pos + chunk.length;
      if (end2 > a && pos < b) {
        out += chunk.slice(Math.max(0, a - pos), Math.max(0, b - pos));
      }
      pos = end2;
      if (pos >= b) break;
    }
    return out;
  }

  insert(pos: number, text: string): void {
    if (!text) return;
    const at = clamp(pos, 0, this.len);
    const [left, right] = this.splitAt(at);
    const middle = Rope.fromString(text).chunks;
    this.chunks = Rope.rebalance([...left, ...middle, ...right]);
    this.len += text.length;
  }

  /** Deletes [start, end) and returns the removed text. */
  delete(start: number, end: number): string {
    const a = clamp(start, 0, this.len);
    const b = clamp(end, 0, this.len);
    if (b <= a) return "";
    const [left, midRight] = this.splitAt(a);
    const midRope = new Rope(midRight);
    const removed = midRope.slice(0, b - a);
    const [, right] = midRope.splitAt(b - a);
    this.chunks = Rope.rebalance([...left, ...right]);
    this.len -= removed.length;
    return removed;
  }

  private splitAt(index: number): [string[], string[]] {
    if (index <= 0) return [[], [...this.chunks]];
    if (index >= this.len) return [[...this.chunks], []];
    const left: string[] = [];
    const right: string[] = [];
    let pos = 0;
    let done = false;
    for (const chunk of this.chunks) {
      if (done) {
        right.push(chunk);
        continue;
      }
      if (pos + chunk.length <= index) {
        left.push(chunk);
        pos += chunk.length;
        continue;
      }
      const cut = index - pos;
      if (cut > 0) left.push(chunk.slice(0, cut));
      if (cut < chunk.length) right.push(chunk.slice(cut));
      done = true;
      pos += chunk.length;
    }
    return [left, right];
  }

  private static rebalance(chunks: string[]): string[] {
    const out: string[] = [];
    let carry = "";
    const push = (text: string) => {
      if (text.length === 0) return;
      if (text.length >= CHUNK) {
        if (carry) {
          out.push(carry);
          carry = "";
        }
        for (let i = 0; i < text.length; i += CHUNK) out.push(text.slice(i, i + CHUNK));
        return;
      }
      carry += text;
      if (carry.length >= CHUNK) {
        out.push(carry);
        carry = "";
      }
    };
    for (const chunk of chunks) {
      // Re-split oversized chunks so one giant paste cannot wedge the rope.
      push(chunk);
    }
    if (carry) out.push(carry);
    return out;
  }
}

/** Undo/redo as inverse operation *groups* over a Rope: one textarea change
 * (insert, delete, or replace) is exactly one undo step. */
export class UndoStack {
  undo: EditOp[][] = [];
  redo: EditOp[][] = [];

  get canUndo(): boolean {
    return this.undo.length > 0;
  }

  get canRedo(): boolean {
    return this.redo.length > 0;
  }

  push(step: EditOp[]): void {
    if (step.length === 0) return;
    this.undo.push(step);
    if (this.undo.length > MAX_OPS) this.undo.splice(0, this.undo.length - MAX_OPS);
    this.redo.length = 0;
  }

  static invert(op: EditOp): EditOp {
    return op.t === "insert"
      ? { t: "delete", pos: op.pos, text: op.text }
      : { t: "insert", pos: op.pos, text: op.text };
  }

  private static apply(rope: Rope, op: EditOp): void {
    if (op.t === "insert") rope.insert(op.pos, op.text);
    else rope.delete(op.pos, op.pos + op.text.length);
  }

  undoOnce(rope: Rope): boolean {
    const step = this.undo.pop();
    if (!step) return false;
    for (let i = step.length - 1; i >= 0; i--) {
      UndoStack.apply(rope, UndoStack.invert(step[i]));
    }
    this.redo.push(step);
    if (this.redo.length > MAX_OPS) this.redo.splice(0, this.redo.length - MAX_OPS);
    return true;
  }

  redoOnce(rope: Rope): boolean {
    const step = this.redo.pop();
    if (!step) return false;
    for (const op of step) UndoStack.apply(rope, op);
    this.undo.push(step);
    if (this.undo.length > MAX_OPS) this.undo.splice(0, this.undo.length - MAX_OPS);
    return true;
  }

  serialize(): { undo: EditOp[][]; redo: EditOp[][] } {
    return { undo: this.undo, redo: this.redo };
  }

  static load(data: { undo?: unknown; redo?: unknown } | null | undefined): UndoStack {
    const stack = new UndoStack();
    if (Array.isArray(data?.undo)) {
      stack.undo = data.undo
        .filter((step): step is EditOp[] => Array.isArray(step) && step.every(UndoStack.isOp))
        .slice(-MAX_OPS);
    }
    if (Array.isArray(data?.redo)) {
      stack.redo = data.redo
        .filter((step): step is EditOp[] => Array.isArray(step) && step.every(UndoStack.isOp))
        .slice(-MAX_OPS);
    }
    return stack;
  }

  private static isOp(op: unknown): op is EditOp {
    if (typeof op !== "object" || op === null) return false;
    const o = op as Record<string, unknown>;
    if (o.t !== "insert" && o.t !== "delete") return false;
    if (typeof o.pos !== "number" || typeof o.text !== "string") return false;
    return true;
  }
}

/** Minimal edit script turning `before` into `after` (common prefix/suffix). */
export function diffEdit(before: string, after: string): EditOp[] {
  if (before === after) return [];
  let prefix = 0;
  const minLen = Math.min(before.length, after.length);
  while (prefix < minLen && before[prefix] === after[prefix]) prefix++;
  let suffix = 0;
  while (
    suffix < minLen - prefix &&
    before[before.length - 1 - suffix] === after[after.length - 1 - suffix]
  ) {
    suffix++;
  }
  const ops: EditOp[] = [];
  if (before.length - prefix - suffix > 0) {
    ops.push({
      t: "delete",
      pos: prefix,
      text: before.slice(prefix, before.length - suffix),
    });
  }
  const inserted = after.slice(prefix, after.length - suffix);
  if (inserted) ops.push({ t: "insert", pos: prefix, text: inserted });
  return ops;
}

/** Applies a textarea change to the rope and records one undo step. */
export function applyEdit(rope: Rope, stack: UndoStack, before: string, after: string): void {
  const ops = diffEdit(before, after);
  if (ops.length === 0) return;
  for (const op of ops) {
    if (op.t === "insert") rope.insert(op.pos, op.text);
    else rope.delete(op.pos, op.pos + op.text.length);
  }
  stack.push(ops);
}
