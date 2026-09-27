// Vibe Factory for VS Code: a client of `vibe serve`.
//
// The task tree, the commands and the event output all go through the
// server's HTTP API (see docs/src/user/cli.md, `vibe serve`); the extension
// keeps no state of its own besides what it shows.

import * as fs from "fs";
import * as path from "path";
import * as vscode from "vscode";

import { Envelope, SseParser, TaskRow, contextValue, describe, label } from "./events";

/** How often the task tree is refreshed. */
const REFRESH_MS = 3000;

function workspaceRoot(): string | undefined {
  return vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
}

function settings(): { url: string; token: string } {
  const config = vscode.workspace.getConfiguration("vibe");
  const url = String(config.get("serverUrl") ?? "http://127.0.0.1:7777").replace(/\/+$/, "");
  let token = String(config.get("token") ?? "").trim();
  const root = workspaceRoot();
  if (!token && root) {
    try {
      token = fs.readFileSync(path.join(root, ".vibe", "server.token"), "utf8").trim();
    } catch {
      // No server running in this workspace yet.
    }
  }
  return { url, token };
}

class ApiError extends Error {}

async function api<T>(method: string, route: string, body?: unknown): Promise<T> {
  const { url, token } = settings();
  if (!token) {
    throw new ApiError("no token: start `vibe serve` in the workspace or set vibe.token");
  }
  let response: Response;
  try {
    response = await fetch(`${url}/api${route}`, {
      method,
      headers: {
        Authorization: `Bearer ${token}`,
        ...(body === undefined ? {} : { "Content-Type": "application/json" }),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
  } catch {
    throw new ApiError(`cannot reach ${url}: is \`vibe serve\` running?`);
  }
  const data = (await response.json().catch(() => ({}))) as { error?: string };
  if (!response.ok) {
    throw new ApiError(data.error ?? `HTTP ${response.status}`);
  }
  return data as T;
}

class TaskItem extends vscode.TreeItem {
  constructor(readonly row: TaskRow) {
    super(`${label(row)} ${row.task.title}`, vscode.TreeItemCollapsibleState.None);
    const approval = row.run?.pending_approval;
    this.description = approval
      ? `waiting for approval of the ${approval}`
      : row.task.status.replace(/_/g, " ");
    this.contextValue = contextValue(row);
    this.iconPath = new vscode.ThemeIcon(
      row.running
        ? "sync~spin"
        : approval
          ? "bell"
          : row.task.status === "ready" || row.task.status === "done"
            ? "pass"
            : row.task.status === "failed"
              ? "error"
              : "circle-outline",
    );
    this.tooltip = `${row.task.title}\n${this.description}`;
    this.command = { command: "vibe.follow", title: "Follow events", arguments: [this] };
  }
}

class TaskTree implements vscode.TreeDataProvider<TaskItem> {
  private readonly changed = new vscode.EventEmitter<void>();
  readonly onDidChangeTreeData = this.changed.event;
  private error: string | undefined;

  refresh(): void {
    this.changed.fire();
  }

  getTreeItem(item: TaskItem): vscode.TreeItem {
    return item;
  }

  async getChildren(): Promise<TaskItem[]> {
    try {
      const rows = await api<TaskRow[]>("GET", "/tasks");
      this.error = undefined;
      return rows.map((row) => new TaskItem(row));
    } catch (e) {
      if (this.error !== String(e)) {
        this.error = String(e);
        void vscode.window.setStatusBarMessage(`Vibe: ${(e as Error).message}`, 5000);
      }
      return [];
    }
  }
}

/** Follows one task's events into an output channel. */
class Follower {
  private abort: AbortController | undefined;
  private task: string | undefined;

  constructor(private readonly output: vscode.OutputChannel) {}

  async follow(task: string, title: string): Promise<void> {
    if (this.task === task) {
      this.output.show(true);
      return;
    }
    this.stop();
    this.task = task;
    const abort = new AbortController();
    this.abort = abort;
    const { url, token } = settings();
    this.output.clear();
    this.output.appendLine(`Following ${title}`);
    this.output.show(true);
    try {
      const response = await fetch(
        `${url}/api/tasks/${task}/stream?token=${encodeURIComponent(token)}`,
        { signal: abort.signal },
      );
      if (!response.ok || !response.body) {
        this.output.appendLine(`Cannot follow the task: HTTP ${response.status}`);
        return;
      }
      const parser = new SseParser();
      const decoder = new TextDecoder();
      let streaming = false;
      for await (const chunk of response.body as unknown as AsyncIterable<Uint8Array>) {
        for (const message of parser.push(decoder.decode(chunk, { stream: true }))) {
          const envelope = JSON.parse(message.data) as Envelope;
          if (envelope.event.type === "agent_delta") {
            const delta = envelope.event.delta as { text?: string } | undefined;
            this.output.append(delta?.text ?? "");
            streaming = true;
            continue;
          }
          if (streaming) {
            this.output.appendLine("");
            streaming = false;
          }
          const line = describe(envelope);
          if (line) this.output.appendLine(line);
        }
      }
    } catch (e) {
      if (!abort.signal.aborted) this.output.appendLine(`Stream closed: ${(e as Error).message}`);
    }
  }

  stop(): void {
    this.abort?.abort();
    this.abort = undefined;
    this.task = undefined;
  }
}

export function activate(context: vscode.ExtensionContext): void {
  const tree = new TaskTree();
  const output = vscode.window.createOutputChannel("Vibe Factory");
  const follower = new Follower(output);
  context.subscriptions.push(
    vscode.window.registerTreeDataProvider("vibeTasks", tree),
    output,
    { dispose: () => follower.stop() },
  );
  const timer = setInterval(() => tree.refresh(), REFRESH_MS);
  context.subscriptions.push({ dispose: () => clearInterval(timer) });

  const act =
    (run: (item: TaskItem) => Promise<unknown>, done: string) =>
    async (item?: TaskItem): Promise<void> => {
      if (!item) {
        void vscode.window.showInformationMessage("Select a task in the Vibe Factory view.");
        return;
      }
      try {
        await run(item);
        void vscode.window.setStatusBarMessage(`Vibe: ${done}`, 3000);
      } catch (e) {
        void vscode.window.showErrorMessage(`Vibe: ${(e as Error).message}`);
      }
      tree.refresh();
    };
  const register = (name: string, handler: (...args: never[]) => unknown) =>
    context.subscriptions.push(vscode.commands.registerCommand(name, handler));

  register("vibe.refresh", () => tree.refresh());
  register("vibe.newTask", async () => {
    const title = await vscode.window.showInputBox({ prompt: "Title of the new task" });
    if (!title?.trim()) return;
    const description = await vscode.window.showInputBox({
      prompt: "Description (optional)",
    });
    try {
      await api("POST", "/tasks", { title: title.trim(), description: description ?? "" });
      tree.refresh();
    } catch (e) {
      void vscode.window.showErrorMessage(`Vibe: ${(e as Error).message}`);
    }
  });
  register(
    "vibe.run",
    act(async (item) => {
      await api("POST", `/tasks/${item.row.task.id}/run`, {});
      void follower.follow(item.row.task.id, item.row.task.title);
    }, "running"),
  );
  register(
    "vibe.resume",
    act(async (item) => {
      await api("POST", `/tasks/${item.row.task.id}/run`, { resume: true });
      void follower.follow(item.row.task.id, item.row.task.title);
    }, "resuming"),
  );
  register(
    "vibe.cancel",
    act((item) => api("POST", `/tasks/${item.row.task.id}/cancel`, {}), "cancelling"),
  );
  register(
    "vibe.approve",
    act(async (item) => {
      await api("POST", `/tasks/${item.row.task.id}/approve`, {});
      await api("POST", `/tasks/${item.row.task.id}/run`, { resume: true });
    }, "approved, resuming"),
  );
  register(
    "vibe.reject",
    act(async (item) => {
      const reason = await vscode.window.showInputBox({
        prompt: "Why? The agents will work from this reason.",
      });
      if (!reason?.trim()) throw new ApiError("a rejection needs a reason");
      await api("POST", `/tasks/${item.row.task.id}/reject`, { reason: reason.trim() });
      await api("POST", `/tasks/${item.row.task.id}/run`, { resume: true });
    }, "rejected, resuming"),
  );
  register("vibe.follow", (item?: TaskItem) => {
    if (item) void follower.follow(item.row.task.id, item.row.task.title);
  });
  register("vibe.openWebUi", () => {
    const { url, token } = settings();
    void vscode.env.openExternal(vscode.Uri.parse(`${url}/#token=${token}`));
  });
}

export function deactivate(): void {
  // Subscriptions are disposed by VS Code.
}
