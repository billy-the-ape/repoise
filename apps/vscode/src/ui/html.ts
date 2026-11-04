/**
 * Operations panel markup. All dynamic data crosses the webview boundary via
 * postMessage and is rendered exclusively with textContent — repository
 * content is untrusted and never becomes HTML or script.
 */
export function buildHtml(): string {
  return `<!DOCTYPE html>
<html>
<head>
<meta charset="UTF-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline';">
<style>
  body { font-family: var(--vscode-font-family); color: var(--vscode-foreground); display: flex; flex-direction: column; gap: 10px; padding: 10px; }
  h1 { font-size: 15px; margin: 0; }
  h2 { font-size: 13px; margin: 0 0 6px 0; }
  section { border: 1px solid var(--vscode-panel-border); border-radius: 6px; padding: 8px 10px; }
  .row { display: flex; gap: 8px; align-items: center; flex-wrap: wrap; margin: 6px 0; }
  button { background: var(--vscode-button-background); color: var(--vscode-button-foreground); border: none; padding: 3px 10px; border-radius: 3px; cursor: pointer; }
  button:disabled { opacity: 0.5; cursor: default; }
  select { background: var(--vscode-input-background); color: var(--vscode-input-foreground); border: 1px solid var(--vscode-input-border); }
  pre { white-space: pre-wrap; word-break: break-word; font-size: 12px; max-height: 240px; overflow: auto; margin: 6px 0; }
  .muted { opacity: 0.75; font-size: 12px; margin: 4px 0; }
  .error { color: var(--vscode-errorForeground); font-size: 12px; }
  table { border-collapse: collapse; }
  td { padding: 2px 10px 2px 0; font-size: 12px; text-align: left; vertical-align: top; }
  td:first-child { opacity: 0.8; }
</style>
</head>
<body>
  <h1>Repoise</h1>
  <div id="cli" class="muted"></div>
  <section>
    <div class="row">
      <label for="root">Workspace root</label>
      <select id="root"></select>
      <button id="refresh">Refresh</button>
      <button id="index">Index now</button>
      <button id="watch-start">Start watch</button>
      <button id="watch-stop">Stop watch</button>
      <button id="doctor">Doctor&hellip;</button>
    </div>
  </section>
  <section>
    <h2>Status</h2>
    <div id="status-error" class="error"></div>
    <div id="watch-note" class="muted"></div>
    <table><tbody id="status-body"></tbody></table>
  </section>
  <section>
    <h2>Run log</h2>
    <pre id="log"></pre>
  </section>
  <section>
    <h2>MCP server definition</h2>
    <p class="muted">stdio definition for the selected root. Read-only by default; <code>repoise.mcp.refresh</code> enables the refresh tool. VS Code 1.103+ discovers it through the editor MCP provider; other clients can register the same JSON.</p>
    <pre id="mcp"></pre>
    <div class="row"><button id="mcp-copy">Copy JSON</button></div>
  </section>
<script>
  const vscode = acquireVsCodeApi();
  const $ = (id) => document.getElementById(id);
  function row(label, value, cls) {
    const tr = document.createElement("tr");
    const td1 = document.createElement("td");
    td1.textContent = label;
    const td2 = document.createElement("td");
    td2.textContent = value === null || value === undefined ? "\\u2014" : String(value);
    if (cls) { td2.classList.add(cls); }
    tr.appendChild(td1);
    tr.appendChild(td2);
    return tr;
  }
  function render(p) {
    $("cli").textContent = p.cli;
    const sel = $("root");
    sel.textContent = "";
    for (const r of p.roots) {
      const option = document.createElement("option");
      option.value = r.path;
      option.textContent = r.name;
      sel.appendChild(option);
    }
    if (p.selected && p.roots.some((r) => r.path === p.selected)) { sel.value = p.selected; }
    const st = p.status;
    $("status-error").textContent = st && !st.ok ? (st.detail || st.problem) : "";
    $("watch-note").textContent = p.watch ? ("watch: " + p.watch.status + (p.watch.lastOutcome ? " \\u2014 " + p.watch.lastOutcome : "")) : "";
    const body = $("status-body");
    body.textContent = "";
    if (st && st.ok) {
      const s = st.status;
      if (s.scope) {
        body.appendChild(row("root", s.scope.root));
        body.appendChild(row("scope", s.scope.repoId + " / " + s.scope.worktreeId + " (" + s.scope.snapshotMode + ")"));
      }
      if (s.snapshot) {
        const dirty = s.snapshot.dirtyCount ? " (" + s.snapshot.dirtyCount + " dirty)" : "";
        body.appendChild(row("branch", (s.snapshot.branch || "?") + dirty));
      }
      if (s.freshness) {
        const reasons = s.freshness.reasons.length ? ": " + s.freshness.reasons.join(", ") : "";
        body.appendChild(row("freshness", s.freshness.status + reasons, s.freshness.status !== "fresh" ? "error" : ""));
      }
      if (s.writer) {
        const owner = s.writer.owner ? " (" + s.writer.owner.operation + ", pid " + s.writer.owner.pid + ")" : "";
        body.appendChild(row("writer", s.writer.state + owner));
      }
      if (s.index) {
        body.appendChild(row("index", "generation " + s.index.generationId + ", " + s.index.files + " files, " + s.index.chunks + " chunks"));
        const pending = s.index.vectorsPending ? " (" + s.index.vectorsPending + " pending)" : "";
        body.appendChild(row("vectors", s.index.embeddingCoverage + pending + " [scope " + s.index.embeddingScope + "]", s.index.embeddingCoverage === "partial" || s.index.embeddingCoverage === "profile-unavailable" ? "error" : ""));
      } else {
        body.appendChild(row("index", "missing", "error"));
      }
      if (s.lastError) { body.appendChild(row("last error", s.lastError, "error")); }
    }
    $("mcp").textContent = p.mcp || "";
    $("log").textContent = p.log.join("\\n");
    const canAct = !!p.selected && !p.indexRunning;
    $("index").disabled = !canAct;
    // Drive the watch buttons from the supervisor status, not just the
    // live-process flag: Stop must stay enabled while a start is probing or
    // a bounded restart is pending, and Start must be disabled then.
    const watchStatus = p.watch ? p.watch.status : "idle";
    const watchActive =
      watchStatus === "starting" ||
      watchStatus === "running" ||
      watchStatus === "waiting-restart";
    $("watch-start").disabled = !p.selected || watchActive;
    $("watch-stop").disabled = !p.selected || !watchActive;
    $("mcp-copy").disabled = !p.mcp;
  }
  window.addEventListener("message", (e) => {
    const m = e.data;
    if (m && m.type === "state") { render(m.payload); }
  });
  $("refresh").onclick = () => vscode.postMessage({ type: "refresh" });
  $("root").onchange = () => vscode.postMessage({ type: "select-root", path: $("root").value });
  $("index").onclick = () => vscode.postMessage({ type: "index" });
  $("watch-start").onclick = () => vscode.postMessage({ type: "watch-start" });
  $("watch-stop").onclick = () => vscode.postMessage({ type: "watch-stop" });
  $("doctor").onclick = () => vscode.postMessage({ type: "doctor" });
  $("mcp-copy").onclick = () => vscode.postMessage({ type: "mcp-copy" });
  vscode.postMessage({ type: "ready" });
</script>
</body>
</html>`;
}
