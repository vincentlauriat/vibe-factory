// Pure helpers of the extension: server-sent event parsing, event lines and
// task row states. No `vscode` import, so they run under `node --test`.

/** One server-sent event. */
export interface SseMessage {
  event: string;
  id?: string;
  data: string;
}

/** Incremental parser of a text/event-stream body. */
export class SseParser {
  private buffer = "";

  /** Feed text; returns the events it completes. */
  push(text: string): SseMessage[] {
    this.buffer += text.replace(/\r/g, "");
    const out: SseMessage[] = [];
    let end: number;
    while ((end = this.buffer.indexOf("\n\n")) >= 0) {
      const block = this.buffer.slice(0, end);
      this.buffer = this.buffer.slice(end + 2);
      const message: SseMessage = { event: "message", data: "" };
      const data: string[] = [];
      for (const line of block.split("\n")) {
        if (!line || line.startsWith(":")) continue;
        const colon = line.indexOf(":");
        const field = colon < 0 ? line : line.slice(0, colon);
        let value = colon < 0 ? "" : line.slice(colon + 1);
        if (value.startsWith(" ")) value = value.slice(1);
        if (field === "event") message.event = value;
        else if (field === "id") message.id = value;
        else if (field === "data") data.push(value);
      }
      if (data.length) {
        message.data = data.join("\n");
        out.push(message);
      }
    }
    return out;
  }
}

/** The part of an envelope the extension reads. */
export interface Envelope {
  seq?: number;
  event: { type: string; [key: string]: unknown };
}

const text = (value: unknown): string => (typeof value === "string" ? value : "");
const flat = (value: string, max: number): string => {
  const one = value.replace(/\s+/g, " ").trim();
  return one.length > max ? one.slice(0, max) + "…" : one;
};

/** One output line for an event, or `undefined` when it is not shown. */
export function describe(envelope: Envelope): string | undefined {
  const e = envelope.event;
  switch (e.type) {
    case "run_started":
      return "▶ run started";
    case "phase_started":
      return `● ${text(e.phase)}`;
    case "phase_finished":
      return `  ${text(e.phase)} ${e.success ? "done" : "failed"}: ${flat(text(e.summary), 200)}`;
    case "agent_text":
      return text(e.text).trim() ? `  ${text(e.role)}: ${flat(text(e.text), 240)}` : undefined;
    case "tool_called":
      return `    → ${text(e.tool)} ${flat(JSON.stringify(e.input ?? {}), 140)}`;
    case "tool_returned":
      return e.is_error ? `    ← ${text(e.tool)} failed: ${flat(text(e.preview), 140)}` : undefined;
    case "subtask_updated":
      return `  ▸ subtask ${text(e.status).replace(/_/g, " ")}`;
    case "validation_finished":
      return `  ${e.passed ? "✓" : "✗"} ${text(e.command)}`;
    case "approval_requested":
      return `⏸ approval needed: the ${text(e.gate)}`;
    case "approval_resolved":
      return `  ${text(e.gate)} ${e.approved ? "approved" : "rejected"}`;
    case "paused":
      return `⏸ ${flat(text(e.reason), 200)}`;
    case "run_finished":
      return `■ run finished: ${text(e.status).replace(/_/g, " ")}`;
    default:
      return undefined;
  }
}

/** What the API says about one task. */
export interface TaskRow {
  task: { id: string; title: string; status: string };
  number?: number | null;
  running: boolean;
  run?: { status: string; pending_approval?: string | null } | null;
}

/** Context value of a task item: which actions apply. */
export function contextValue(row: TaskRow): string {
  const parts: string[] = [];
  if (row.running) {
    parts.push("running");
  } else {
    parts.push("idle");
    const status = row.run?.status;
    if (status && status !== "finished") parts.push("resumable");
  }
  if (row.run?.pending_approval) parts.push("approval");
  return parts.join(" ");
}

/** `003`, or the short id. */
export function label(row: TaskRow): string {
  return row.number ? String(row.number).padStart(3, "0") : row.task.id.slice(0, 8);
}
