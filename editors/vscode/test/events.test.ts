import { strict as assert } from "node:assert";
import { test } from "node:test";

import { SseParser, contextValue, describe, label } from "../src/events";

test("parses server-sent events across chunks", () => {
  const parser = new SseParser();
  assert.deepEqual(parser.push("event: run_started\r\nid: 1\r\ndata: {\"a\""), []);
  const events = parser.push(":1}\r\n\r\n: keep-alive\n\nevent: x\ndata: a\ndata: b\n\n");
  assert.deepEqual(events, [
    { event: "run_started", id: "1", data: '{"a":1}' },
    { event: "x", data: "a\nb" },
  ]);
});

test("describes events as one line", () => {
  assert.equal(describe({ event: { type: "phase_started", phase: "build" } }), "● build");
  assert.equal(
    describe({ event: { type: "agent_text", role: "coder", text: "Done.\n\n  All good." } }),
    "  coder: Done. All good.",
  );
  assert.equal(describe({ event: { type: "agent_text", role: "coder", text: " " } }), undefined);
  assert.equal(
    describe({ event: { type: "run_finished", status: "ready" } }),
    "■ run finished: ready",
  );
  assert.equal(describe({ event: { type: "budget_updated" } }), undefined);
});

test("task actions depend on the run", () => {
  const row = (running: boolean, status?: string, gate?: string) => ({
    task: { id: "0123456789", title: "t", status: "review" },
    number: 3,
    running,
    run: status ? { status, pending_approval: gate ?? null } : null,
  });
  assert.equal(contextValue(row(false)), "idle");
  assert.equal(contextValue(row(true, "running")), "running");
  assert.equal(contextValue(row(false, "paused", "plan")), "idle resumable approval");
  assert.equal(contextValue(row(false, "finished")), "idle");
  assert.equal(label(row(false)), "003");
  assert.equal(label({ ...row(false), number: null }), "01234567");
});
