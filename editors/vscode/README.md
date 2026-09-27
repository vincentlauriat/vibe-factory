# Vibe Factory for VS Code

Drive Vibe Factory from the editor: the task board in the activity bar, runs, approvals and
live events in an output channel. The extension is a client of `vibe serve`, so start the
server in the project first:

```sh
vibe serve            # prints the address and writes .vibe/server.token
```

The extension reads `.vibe/server.token` from the first workspace folder, so there is
nothing to configure for a server on the default address. Settings:

| Setting | Default | Meaning |
|---------|---------|---------|
| `vibe.serverUrl` | `http://127.0.0.1:7777` | address of `vibe serve` |
| `vibe.token` | empty | API token; empty reads `.vibe/server.token` |

## What it does

* **Tasks view** (rocket icon): every task with its status; a spinning icon while it runs,
  a bell when it waits for an approval. Inline actions: run, resume, cancel, approve,
  reject (asks for the reason). Refreshes every 3 seconds.
* **Follow events**: selecting a task streams its events into the "Vibe Factory" output
  channel, with the model's text as it is written.
* **New task** and **Open the web UI** in the view's title bar.

Approving or rejecting resumes the run, as the web UI does.

## Build

```sh
npm install
npm test              # compiles and runs the unit tests
npx @vscode/vsce package   # builds vibe-factory-<version>.vsix to install
```

Open this folder in VS Code and press F5 to try it in an extension development host.
