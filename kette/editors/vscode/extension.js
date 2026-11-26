const vscode = require("vscode");
const child_process = require("child_process");
const fs = require("fs");
const path = require("path");

const LEGEND = new vscode.SemanticTokensLegend(
  [
    "keyword",
    "variable",
    "parameter",
    "property",
    "function",
    "method",
    "namespace",
    "enumMember",
    "string",
    "number",
    "comment",
    "operator",
  ],
  []
);

let client = null;
let output = null;
let diagnostics = null;
let status = null;
let lastKetteEditor = null;
let loadedFiles = null;
let refreshingLoadedFiles = false;

class CommandsViewProvider {
  constructor(context) {
    this.context = context;
  }

  resolveWebviewView(view) {
    view.webview.options = { enableScripts: true };
    view.webview.html = `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<style>
  body {
    padding: 0 10px 14px;
    color: var(--vscode-foreground);
    font-family: var(--vscode-font-family);
    font-size: var(--vscode-font-size);
  }
  h3 {
    margin: 14px 0 6px;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: .06em;
    text-transform: uppercase;
    opacity: .7;
  }
  button {
    display: block;
    width: 100%;
    margin: 4px 0;
    padding: 6px 8px;
    text-align: left;
    border: 0;
    border-radius: 2px;
    cursor: pointer;
    color: var(--vscode-button-foreground);
    background: var(--vscode-button-background);
  }
  button:hover { background: var(--vscode-button-hoverBackground); }
  button.secondary {
    color: var(--vscode-button-secondaryForeground);
    background: var(--vscode-button-secondaryBackground);
  }
  button.secondary:hover { background: var(--vscode-button-secondaryHoverBackground); }
  p.hint { margin: 12px 0 0; font-size: 11px; opacity: .65; }
</style>
</head>
<body>
  <h3>Evaluate</h3>
  <button data-command="kette.evalSelection">Eval Selection / Line</button>
  <button data-command="kette.evalBuffer">Eval Buffer</button>

  <h3>Image</h3>
  <button data-command="kette.loadFile" class="secondary">Load File</button>
  <button data-command="kette.openRepl" class="secondary">Open REPL</button>
  <button data-command="kette.showTranscript" class="secondary">Show Transcript</button>
  <button data-command="kette.imageInfo" class="secondary">Image Info</button>
  <button data-command="kette.restart" class="secondary">Restart Image</button>

  <p class="hint">Eval sends text to the running image. It does not re-evaluate on every keystroke.</p>
<script>
  const vscode = acquireVsCodeApi();
  for (const button of document.querySelectorAll("button")) {
    button.addEventListener("click", () => {
      vscode.postMessage({ command: button.dataset.command });
    });
  }
</script>
</body>
</html>`;
    view.webview.onDidReceiveMessage(async (message) => {
      if (!message || typeof message.command !== "string") {
        return;
      }
      try {
        await vscode.commands.executeCommand(message.command);
      } catch (error) {
        vscode.window.showErrorMessage(`Kette: ${error.message}`);
      }
    });
  }
}

class LoadedFilesProvider {
  constructor() {
    this.items = [];
    this.emitter = new vscode.EventEmitter();
    this.onDidChangeTreeData = this.emitter.event;
  }

  setFiles(files, root) {
    const seen = new Set();
    const items = [];
    for (const entry of files) {
      const file = path.isAbsolute(entry) ? entry : path.resolve(root, entry);
      if (seen.has(file)) {
        continue;
      }
      seen.add(file);
      const relative = path.relative(root, file);
      const inside = relative.length > 0 && !relative.startsWith("..") && !path.isAbsolute(relative);
      const item = new vscode.TreeItem(
        inside ? relative : file,
        vscode.TreeItemCollapsibleState.None
      );
      if (inside) {
        const directory = path.dirname(relative);
        item.description = directory === "." ? "" : directory;
      } else {
        item.description = "outside workspace";
      }
      item.resourceUri = vscode.Uri.file(file);
      item.tooltip = file;
      item.command = {
        command: "vscode.open",
        title: "Open",
        arguments: [vscode.Uri.file(file)],
      };
      items.push(item);
    }
    this.items = items;
    this.emitter.fire();
  }

  getTreeItem(item) {
    return item;
  }

  getChildren() {
    return this.items;
  }
}

async function refreshLoadedFiles() {
  if (!client || !loadedFiles || refreshingLoadedFiles) {
    return;
  }
  refreshingLoadedFiles = true;
  try {
    const info = await client.request("kette/imageInfo", {});
    loadedFiles.setFiles(info.files || [], info.root || findRoot());
  } catch (_) {
    // The image may be reloading; the next refresh picks it up.
  } finally {
    refreshingLoadedFiles = false;
  }
}

class ReplViewProvider {
  constructor(context) {
    this.context = context;
    this.view = null;
    this.history = [];
    this.historyIndex = 0;
  }

  resolveWebviewView(view) {
    this.view = view;
    view.webview.options = { enableScripts: true };
    view.webview.html = this.html();
    view.webview.onDidReceiveMessage(async (message) => {
      if (!message || typeof message.command !== "string") {
        return;
      }
      if (message.command === "clear") {
        view.webview.postMessage({ type: "clear" });
        return;
      }
      if (message.command !== "eval") {
        return;
      }
      const source = String(message.source || "");
      if (source.trim().length === 0) {
        return;
      }
      this.history.push(source);
      this.historyIndex = this.history.length;
      let result = null;
      try {
        if (!client) {
          throw new Error("image is not running");
        }
        result = await client.request("kette/eval", { source });
        refreshLoadedFiles();
      } catch (error) {
        result = { ok: false, error: error.message, transcript: "" };
      }
      view.webview.postMessage({ type: "result", result });
    });
  }

  html() {
    return `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<style>
  body {
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    height: 100vh;
    color: var(--vscode-foreground);
    background: var(--vscode-panel-background);
    font-family: var(--vscode-editor-font-family);
    font-size: var(--vscode-editor-font-size);
  }
  #output {
    flex: 1;
    overflow: auto;
    margin: 0;
    padding: 8px 10px;
    white-space: pre-wrap;
    word-break: break-word;
    font-family: inherit;
  }
  .entry { margin: 0 0 2px; }
  .echo { color: var(--vscode-textLink-foreground); }
  .ok { color: var(--vscode-terminal-ansiGreen); }
  .err { color: var(--vscode-errorForeground); }
  .muted { color: var(--vscode-descriptionForeground); }
  #composer {
    border-top: 1px solid var(--vscode-panel-border);
    padding: 6px 8px;
    display: grid;
    gap: 6px;
  }
  #input {
    width: 100%;
    box-sizing: border-box;
    min-height: 48px;
    resize: vertical;
    padding: 6px 8px;
    color: var(--vscode-input-foreground);
    background: var(--vscode-input-background);
    border: 1px solid var(--vscode-input-border, transparent);
    outline: none;
    font-family: inherit;
    font-size: inherit;
  }
  #input:focus { border-color: var(--vscode-focusBorder); }
  #buttons { display: flex; align-items: center; gap: 6px; }
  button {
    padding: 4px 10px;
    border: 0;
    border-radius: 2px;
    cursor: pointer;
    color: var(--vscode-button-foreground);
    background: var(--vscode-button-background);
    font-family: var(--vscode-font-family);
  }
  button:hover { background: var(--vscode-button-hoverBackground); }
  button.secondary {
    color: var(--vscode-button-secondaryForeground);
    background: var(--vscode-button-secondaryBackground);
  }
  button.secondary:hover { background: var(--vscode-button-secondaryHoverBackground); }
  #hint { margin-left: auto; font-family: var(--vscode-font-family); font-size: 11px; }
</style>
</head>
<body>
  <pre id="output"></pre>
  <div id="composer">
    <textarea id="input" rows="3" spellcheck="false" placeholder="Kette expression or definition"></textarea>
    <div id="buttons">
      <button id="eval">Eval</button>
      <button id="clear" class="secondary">Clear</button>
      <span id="hint" class="muted">Cmd/Ctrl+Enter to eval, Enter for newline</span>
    </div>
  </div>
<script>
  const vscode = acquireVsCodeApi();
  const output = document.getElementById("output");
  const input = document.getElementById("input");
  const evalButton = document.getElementById("eval");
  const clearButton = document.getElementById("clear");
  let pending = false;
  let history = [];
  let historyIndex = 0;

  function append(text, className) {
    if (text === null || text === undefined || text === "") {
      return;
    }
    const entry = document.createElement("div");
    entry.className = "entry " + (className || "");
    entry.textContent = text;
    output.appendChild(entry);
    output.scrollTop = output.scrollHeight;
  }

  function submit(command) {
    const source = input.value;
    if (pending || source.trim().length === 0) {
      return;
    }
    pending = true;
    history.push(source);
    historyIndex = history.length;
    append("> " + source.replace(/\\n/g, "\\n> "), "echo");
    input.value = "";
    vscode.postMessage({ command, source });
  }

  evalButton.addEventListener("click", () => submit("eval"));
  clearButton.addEventListener("click", () => {
    output.textContent = "";
    input.focus();
  });

  input.addEventListener("keydown", (event) => {
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      submit("eval");
      return;
    }
    if (event.key === "ArrowUp" && input.selectionStart === 0 && input.selectionEnd === 0) {
      if (historyIndex > 0) {
        historyIndex -= 1;
        input.value = history[historyIndex] || "";
      }
      event.preventDefault();
      return;
    }
    if (event.key === "ArrowDown" && input.selectionStart === input.value.length) {
      if (historyIndex < history.length - 1) {
        historyIndex += 1;
        input.value = history[historyIndex] || "";
      } else {
        historyIndex = history.length;
        input.value = "";
      }
      event.preventDefault();
    }
  });

  window.addEventListener("message", (event) => {
    const message = event.data;
    if (!message || message.type !== "result") {
      return;
    }
    pending = false;
    const result = message.result || {};
    if (result.transcript) {
      append(result.transcript, "");
    }
    if (result.ok) {
      const value = result.value === null || result.value === undefined ? "null" : result.value;
      append("=> " + value, "ok");
    } else {
      append(result.error || "evaluation failed", "err");
    }
    input.focus();
  });

  append("Kette image REPL", "muted");
  input.focus();
</script>
</body>
</html>`;
  }
}

class RpcClient {
  constructor(command, args, cwd) {
    this.nextId = 1;
    this.pending = new Map();
    this.buffer = Buffer.alloc(0);
    this.exited = false;
    this.process = child_process.spawn(command, args, {
      cwd,
      stdio: ["pipe", "pipe", "pipe"],
    });
    this.process.stdout.on("data", (data) => this.onData(data));
    this.process.stderr.on("data", (data) => {
      const text = data.toString().trimEnd();
      if (text.length > 0) {
        output.appendLine(text);
      }
    });
    this.process.on("exit", (code) => {
      this.exited = true;
      if (status) {
        status.text = "Kette image: stopped";
      }
      output.appendLine(`image exited with ${code}`);
      for (const pending of this.pending.values()) {
        pending.reject(new Error("image exited"));
      }
      this.pending.clear();
    });
    this.process.on("error", (error) => {
      output.appendLine(`image error: ${error.message}`);
    });
  }

  onData(data) {
    this.buffer = Buffer.concat([this.buffer, data]);
    while (true) {
      const headerEnd = this.buffer.indexOf("\r\n\r\n");
      if (headerEnd < 0) {
        return;
      }
      const header = this.buffer.slice(0, headerEnd).toString("ascii");
      let length = null;
      for (const line of header.split("\r\n")) {
        const [name, value] = line.split(":", 2);
        if (name && value && name.toLowerCase() === "content-length") {
          length = Number(value.trim());
        }
      }
      if (length === null) {
        this.buffer = this.buffer.slice(headerEnd + 4);
        continue;
      }
      const bodyStart = headerEnd + 4;
      if (this.buffer.length < bodyStart + length) {
        return;
      }
      const body = this.buffer.slice(bodyStart, bodyStart + length).toString("utf8");
      this.buffer = this.buffer.slice(bodyStart + length);
      let message = null;
      try {
        message = JSON.parse(body);
      } catch (error) {
        output.appendLine(`invalid image message: ${error.message}`);
        continue;
      }
      this.onMessage(message);
    }
  }

  onMessage(message) {
    if (message.id !== undefined && message.method === undefined) {
      const pending = this.pending.get(message.id);
      if (!pending) {
        return;
      }
      this.pending.delete(message.id);
      if (message.error) {
        pending.reject(new Error(message.error.message || "image error"));
      } else {
        pending.resolve(message.result);
      }
      return;
    }
    if (message.method === "textDocument/publishDiagnostics") {
      publishDiagnostics(message.params);
      return;
    }
    if (message.method === "window/showMessage") {
      const params = message.params || {};
      const text = params.message || "Kette image";
      if (params.type === 1) {
        vscode.window.showErrorMessage(text);
      } else if (params.type === 2) {
        vscode.window.showWarningMessage(text);
      } else {
        vscode.window.showInformationMessage(text);
      }
    }
  }

  send(message) {
    if (this.exited) {
      throw new Error("image is not running");
    }
    const body = Buffer.from(JSON.stringify(message), "utf8");
    this.process.stdin.write(`Content-Length: ${body.length}\r\n\r\n`);
    this.process.stdin.write(body);
  }

  request(method, params) {
    const id = this.nextId++;
    const promise = new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
    });
    this.send({ jsonrpc: "2.0", id, method, params });
    return promise;
  }

  notify(method, params) {
    this.send({ jsonrpc: "2.0", method, params });
  }
}

function publishDiagnostics(params) {
  const uri = vscode.Uri.parse(params.uri);
  const items = params.diagnostics.map((item) => {
    const range = new vscode.Range(
      item.range.start.line,
      item.range.start.character,
      item.range.end.line,
      item.range.end.character
    );
    const diagnostic = new vscode.Diagnostic(
      range,
      item.message,
      item.severity === 1 ? vscode.DiagnosticSeverity.Error : vscode.DiagnosticSeverity.Warning
    );
    diagnostic.source = "kette";
    return diagnostic;
  });
  diagnostics.set(uri, items);
}

function findRoot() {
  const configured = vscode.workspace.getConfiguration("kette").get("root");
  if (configured && configured.length > 0) {
    return configured;
  }
  for (const folder of vscode.workspace.workspaceFolders || []) {
    let directory = folder.uri.fsPath;
    while (true) {
      if (fs.existsSync(path.join(directory, "kette", "std", "startup.ktt"))) {
        return directory;
      }
      const parent = path.dirname(directory);
      if (parent === directory) {
        break;
      }
      directory = parent;
    }
  }
  return (vscode.workspace.workspaceFolders || [])[0]?.uri.fsPath || process.cwd();
}

function activeEditor() {
  const editor = vscode.window.activeTextEditor;
  if (editor && editor.document.languageId === "kette") {
    lastKetteEditor = editor;
    return editor;
  }
  if (lastKetteEditor && !lastKetteEditor.document.isClosed) {
    return lastKetteEditor;
  }
  vscode.window.showInformationMessage("Kette: no active .ktt editor");
  return null;
}

function selectedOrBuffer(editor) {
  if (!editor.selection.isEmpty) {
    return editor.document.getText(editor.selection);
  }
  const line = editor.document.lineAt(editor.selection.active.line);
  return line.text;
}

async function showResult(label, result) {
  if (!result) {
    return;
  }
  if (result.transcript && result.transcript.length > 0) {
    output.append(result.transcript);
  }
  if (result.ok) {
    output.appendLine(`${label}: ${result.value}`);
    status.text = "Kette image: ready";
  } else {
    output.appendLine(`${label} failed: ${result.error}`);
    status.text = "Kette image: error";
    output.show(true);
  }
}

async function syncDocument(document) {
  if (!client || document.languageId !== "kette") {
    return;
  }
  client.notify("textDocument/didOpen", {
    textDocument: {
      uri: document.uri.toString(),
      languageId: "kette",
      version: document.version,
      text: document.getText(),
    },
  });
}

async function start(context) {
  output = vscode.window.createOutputChannel("Kette");
  diagnostics = vscode.languages.createDiagnosticCollection("kette");
  status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 0);
  status.text = "Kette image: connecting";
  status.show();

  const root = findRoot();
  const command = vscode.workspace.getConfiguration("kette").get("serverCommand") || "kettevm";
  const script = vscode.workspace.getConfiguration("kette").get("serverScript") || "kette/tools/kettels.ktt";
  const scriptPath = path.isAbsolute(script) ? script : path.join(root, script);
  if (!fs.existsSync(scriptPath)) {
    output.appendLine(`cannot find ${scriptPath}`);
    status.text = "Kette image: missing script";
    return;
  }
  client = new RpcClient(command, [scriptPath, "--", "--stdio"], root);
  context.subscriptions.push({ dispose: () => client && client.process.kill() });
  try {
    await client.request("initialize", {
      processId: process.pid,
      rootUri: vscode.Uri.file(root).toString(),
      capabilities: {},
    });
    client.notify("initialized", {});
    status.text = "Kette image: ready";
    await refreshLoadedFiles();
  } catch (error) {
    output.appendLine(`initialize failed: ${error.message}`);
    status.text = "Kette image: failed";
    output.show(true);
    return;
  }

  const provider = vscode.languages.registerDocumentSemanticTokensProvider(
    { language: "kette" },
    {
      async provideDocumentSemanticTokens(document, token) {
        try {
          const result = await client.request("textDocument/semanticTokens/full", {
            textDocument: { uri: document.uri.toString() },
          });
          if (token.isCancellationRequested) {
            return undefined;
          }
          return new vscode.SemanticTokens(new Uint32Array(result.data || []), String(Date.now()));
        } catch (error) {
          return undefined;
        }
      },
    },
    LEGEND
  );
  context.subscriptions.push(provider);

  context.subscriptions.push(
    vscode.commands.registerCommand("kette.evalSelection", async () => {
      const editor = activeEditor();
      if (!editor) return;
      const source = selectedOrBuffer(editor);
      const result = await client.request("kette/eval", {
        source,
        uri: editor.document.uri.toString(),
      });
      await showResult("eval", result);
      refreshLoadedFiles();
    }),
    vscode.commands.registerCommand("kette.evalBuffer", async () => {
      const editor = activeEditor();
      if (!editor) return;
      const result = await client.request("kette/eval", {
        source: editor.document.getText(),
        uri: editor.document.uri.toString(),
      });
      await showResult("eval buffer", result);
      refreshLoadedFiles();
    }),
    vscode.commands.registerCommand("kette.loadFile", async () => {
      const editor = activeEditor();
      if (!editor) return;
      const result = await client.request("kette/loadFile", {
        path: editor.document.uri.fsPath,
      });
      await showResult("load", result);
      refreshLoadedFiles();
    }),
    vscode.commands.registerCommand("kette.showTranscript", async () => {
      const result = await client.request("kette/transcript", {});
      if (result.text && result.text.length > 0) {
        output.append(result.text);
      }
      output.show(true);
    }),
    vscode.commands.registerCommand("kette.imageInfo", async () => {
      const result = await client.request("kette/imageInfo", {});
      vscode.window.showInformationMessage(JSON.stringify(result));
    }),
    vscode.commands.registerCommand("kette.restart", async () => {
      vscode.window.showInformationMessage("Kette: restarting image");
      await vscode.commands.executeCommand("workbench.action.reloadWindow");
    })
  );

  context.subscriptions.push(
    vscode.workspace.onDidOpenTextDocument((document) => syncDocument(document)),
    vscode.workspace.onDidChangeTextDocument((event) => {
      if (client && event.document.languageId === "kette") {
        client.notify("textDocument/didChange", {
          textDocument: {
            uri: event.document.uri.toString(),
            version: event.document.version,
          },
          contentChanges: [{ text: event.document.getText() }],
        });
      }
    }),
    vscode.workspace.onDidSaveTextDocument((document) => {
      if (client && document.languageId === "kette") {
        client.notify("textDocument/didSave", {
          textDocument: { uri: document.uri.toString() },
          text: document.getText(),
        });
      }
    }),
    vscode.workspace.onDidCloseTextDocument((document) => {
      if (client && document.languageId === "kette") {
        client.notify("textDocument/didClose", {
          textDocument: { uri: document.uri.toString() },
        });
      }
    })
  );

  for (const document of vscode.workspace.textDocuments) {
    if (document.languageId === "kette") {
      await syncDocument(document);
    }
  }
}

function activate(context) {
  context.subscriptions.push(
    vscode.window.registerWebviewViewProvider(
      "kette.commands",
      new CommandsViewProvider(context),
      { webviewOptions: { retainContextWhenHidden: true } }
    )
  );
  loadedFiles = new LoadedFilesProvider();
  context.subscriptions.push(
    vscode.window.registerTreeDataProvider("kette.loadedFiles", loadedFiles)
  );
  context.subscriptions.push(
    vscode.commands.registerCommand("kette.refreshLoadedFiles", async () => {
      await refreshLoadedFiles();
    })
  );
  context.subscriptions.push(
    vscode.window.registerWebviewViewProvider(
      "kette.repl",
      new ReplViewProvider(context),
      { webviewOptions: { retainContextWhenHidden: true } }
    )
  );
  context.subscriptions.push(
    vscode.commands.registerCommand("kette.openRepl", async () => {
      try {
        await vscode.commands.executeCommand("kette.repl.focus");
      } catch (error) {
        vscode.window.showErrorMessage(`Kette: ${error.message}`);
      }
    })
  );
  context.subscriptions.push(
    vscode.window.onDidChangeActiveTextEditor((editor) => {
      if (editor && editor.document.languageId === "kette") {
        lastKetteEditor = editor;
      }
    })
  );
  start(context).catch((error) => {
    if (output) {
      output.appendLine(`activate failed: ${error.message}`);
      output.show(true);
    }
  });
}

function deactivate() {
  if (!client) {
    return;
  }
  try {
    client.notify("exit", {});
  } catch (_) {}
  setTimeout(() => {
    if (client && !client.exited) {
      client.process.kill();
    }
  }, 250);
  client = null;
}

module.exports = { activate, deactivate };
