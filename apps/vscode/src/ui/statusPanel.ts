import * as vscode from "vscode";
import { RepoiseEvent, asTerminal } from "../core/types";
import { describeEvent } from "../core/display";
import { runIndex, IndexSummary } from "../core/operations";
import { runStatus, StatusOutcome } from "../core/status";
import { buildMcpDefinition, mcpServersSnippet } from "../core/mcp";
import { runCli } from "../core/run";
import { WatchInfo } from "../core/watchPolicy";
import { CliManager, isCliSession } from "./cliManager";
import { WatchManager } from "./watchManager";
import { log, truncate } from "./output";
import { buildHtml } from "./html";

interface RootModel {
  name: string;
  path: string;
  status: StatusOutcome | null;
  mcpJson: string | null;
}

interface PanelState {
  cli: string;
  roots: { name: string; path: string }[];
  selected: string | null;
  status: StatusOutcome | null;
  watch: WatchInfo;
  mcp: string | null;
  indexRunning: boolean;
  watchRunning: boolean;
  log: string[];
}

const MAX_LOG_LINES = 200;

/**
 * The single operations panel: per-root status, index runs, owned watch
 * lifecycle and the MCP definition for the selected root.
 */
export class StatusPanel implements vscode.Disposable {
  private panel: vscode.WebviewPanel | undefined;
  private roots: RootModel[] = [];
  private selected: string | null = null;
  private indexRunning = new Set<string>();
  private logLines: string[] = [];

  constructor(
    private readonly cli: CliManager,
    private readonly watch: WatchManager,
    private readonly output: vscode.OutputChannel,
  ) {}

  private selectedModel(): RootModel | undefined {
    if (this.selected === null) {
      return undefined;
    }
    return this.roots.find((r) => r.path === this.selected);
  }

  private pushLog(line: string): void {
    this.logLines.push(line);
    if (this.logLines.length > MAX_LOG_LINES) {
      this.logLines.splice(0, this.logLines.length - MAX_LOG_LINES);
    }
    log(this.output, line);
  }

  private buildState(): PanelState {
    const session = this.cliResolveSnapshot();
    const model = this.selectedModel();
    return {
      cli: session,
      roots: this.roots.map((r) => ({ name: r.name, path: r.path })),
      selected: this.selected,
      status: model?.status ?? null,
      watch: this.watch.info(this.selected ?? ""),
      mcp: model?.mcpJson ?? null,
      indexRunning:
        this.selected !== null && this.indexRunning.has(this.selected),
      watchRunning:
        this.selected !== null && this.watch.isRunning(this.selected),
      log: [...this.logLines],
    };
  }

  private cliResolveSnapshot(): string {
    const cached = this.cli.current();
    if (cached === null) {
      return "CLI not resolved yet (press Refresh).";
    }
    if (!isCliSession(cached)) {
      return `CLI unavailable: ${cached.message}`;
    }
    const problems = cached.problems.length
      ? ` — notes: ${cached.problems.join("; ")}`
      : "";
    return `Repoise CLI v${cached.version} (${cached.source}) at ${cached.binary}${problems}`;
  }

  reveal(): Thenable<vscode.WebviewPanel> {
    if (this.panel !== undefined) {
      return Promise.resolve(this.panel);
    }
    const panel = vscode.window.createWebviewPanel(
      "repoise.status",
      "Repoise",
      vscode.ViewColumn.Beside,
      { enableScripts: true, retainContextWhenHidden: true },
    );
    this.panel = panel;
    panel.webview.html = buildHtml();
    panel.webview.onDidReceiveMessage((message) => {
      void this.handleMessage(message);
    });
    panel.onDidDispose(() => {
      this.panel = undefined;
    });
    void this.syncRoots();
    return Promise.resolve(panel);
  }

  private async handleMessage(message: {
    type?: string;
    path?: string;
  }): Promise<void> {
    switch (message.type) {
      case "ready":
        await this.syncRoots();
        break;
      case "select-root":
        if (
          message.path !== undefined &&
          this.roots.some((r) => r.path === message.path)
        ) {
          this.selected = message.path;
          await this.refreshSelected();
          void this.maybeAutoStartWatch();
        }
        break;
      case "refresh":
        await this.refreshAll();
        break;
      case "index":
        await this.triggerIndex();
        break;
      case "watch-start":
        await this.triggerWatchStart();
        break;
      case "watch-stop":
        await this.triggerWatchStop();
        break;
      case "doctor":
        await this.triggerDoctor();
        break;
      case "mcp-copy":
        await this.triggerMcpCopy();
        break;
      default:
        break;
    }
  }

  /** Rebuild the root list from the workspace folders, keeping state. */
  async syncRoots(): Promise<void> {
    const folders = (vscode.workspace.workspaceFolders ?? []).map((f) => ({
      name: f.name,
      path: f.uri.fsPath,
    }));
    const previous = new Map(this.roots.map((r) => [r.path, r]));
    this.roots = folders.map(
      (folder) =>
        previous.get(folder.path) ?? {
          name: folder.name,
          path: folder.path,
          status: null,
          mcpJson: null,
        },
    );
    if (
      this.selected === null ||
      !this.roots.some((r) => r.path === this.selected)
    ) {
      this.selected = this.roots[0]?.path ?? null;
    }
    if (this.panel !== undefined) {
      void this.refreshSelected();
    }
    this.sendState();
    void this.maybeAutoStartWatch();
  }

  /**
   * `repoise.watch.autoStart`: start an extension-owned watch for the
   * selected root when the panel opens or the selection changes. The watch
   * supervisor probes the authoritative scope lock first, so this never
   * spawns a competing writer, and restarts stay bounded.
   */
  private async maybeAutoStartWatch(): Promise<void> {
    const path = this.selected;
    if (path === null || !vscode.workspace.isTrusted) {
      return;
    }
    const autoStart =
      vscode.workspace
        .getConfiguration("repoise")
        .get<boolean>("watch.autoStart") ?? false;
    if (
      !autoStart ||
      this.watch.isRunning(path) ||
      this.watch.timerPending(path)
    ) {
      return;
    }
    this.pushLog(`auto-starting owned watch for ${path}`);
    await this.watch.start(path);
  }

  async refreshAll(): Promise<void> {
    await Promise.all(this.roots.map((root) => this.refreshRootModel(root)));
    const session = await this.cli.resolve();
    if (isCliSession(session) && !session.compatible) {
      await vscode.window.showWarningMessage(
        `The resolved Repoise CLI is not fully compatible with this extension: ${session.problems.join("; ")}`,
      );
    }
    this.sendState();
  }

  private async refreshSelected(): Promise<void> {
    const model = this.selectedModel();
    if (model !== undefined) {
      await this.refreshRootModel(model);
    }
    this.sendState();
  }

  private async refreshRootModel(model: RootModel): Promise<void> {
    const session = await this.cli.resolve();
    if (!isCliSession(session)) {
      model.status = {
        ok: false,
        problem: session.problem,
        detail: session.message,
      };
      model.mcpJson = null;
      return;
    }
    model.status = await runStatus(session.binary, model.path);
    if (model.status.ok) {
      const scope = model.status.status;
      this.cli.setWorktreeId(model.path, scope.scope.worktreeId);
      const refresh =
        vscode.workspace
          .getConfiguration("repoise")
          .get<boolean>("mcp.refresh") ?? false;
      model.mcpJson = mcpServersSnippet(
        buildMcpDefinition(session.binary, model.path, {
          refresh,
          worktreeId: scope.scope.worktreeId,
        }),
      );
    } else {
      model.mcpJson = null;
    }
  }

  async triggerIndex(): Promise<void> {
    const path = this.selected;
    if (path === null || this.indexRunning.has(path)) {
      return;
    }
    const session = await this.cli.resolve();
    if (!isCliSession(session)) {
      this.pushLog(`index unavailable: ${session.message}`);
      this.sendState();
      return;
    }
    this.indexRunning.add(path);
    this.pushLog(`index started for ${path}`);
    this.sendState();
    const summary = await runIndex(session.binary, path, (event) =>
      this.onCliEvent(path, event),
    );
    this.indexRunning.delete(path);
    this.pushLog(indexSummaryLine(summary));
    await this.refreshRootModel(
      this.selectedModel() ?? { name: "", path, status: null, mcpJson: null },
    );
    this.sendState();
    const failed = summary.terminal === "failed" || summary.incomplete;
    if (failed) {
      await vscode.window.showErrorMessage(
        `Repoise index for ${path} did not finish cleanly (exit ${summary.exitCode ?? "n/a"}). See the Repoise output channel.`,
      );
    }
  }

  async triggerWatchStart(): Promise<void> {
    const path = this.selected;
    if (path === null) {
      return;
    }
    this.pushLog(`watch start requested for ${path}`);
    await this.watch.start(path);
    await this.refreshRootModel(
      this.selectedModel() ?? { name: "", path, status: null, mcpJson: null },
    );
    this.sendState();
  }

  async triggerWatchStop(): Promise<void> {
    const path = this.selected;
    if (path === null) {
      return;
    }
    this.pushLog(`watch stop requested for ${path}`);
    this.watch.stop(path);
    this.sendState();
    // The stream's terminal `cancelled` event drives the final refresh.
  }

  private async triggerDoctor(): Promise<void> {
    const path = this.selected;
    const session = await this.cli.resolve();
    if (path === null || !isCliSession(session)) {
      await vscode.window.showInformationMessage(
        isCliSession(session)
          ? "Select a workspace root first."
          : `CLI unavailable: ${session.message}`,
      );
      return;
    }
    const lines: string[] = [];
    const handle = runCli({
      binary: session.binary,
      args: ["doctor", path],
      onStdoutLine: (line) => lines.push(line),
      onStderr: (text) => lines.push(text),
    });
    const result = await handle.promise;
    this.output.show(true);
    log(
      this.output,
      `--- repoise doctor ${path} (exit ${result.exitCode ?? "n/a"}) ---`,
    );
    for (const line of lines) {
      log(this.output, line);
    }
    await vscode.window.showInformationMessage(
      `Repoise doctor output written to the Repoise output channel (exit ${result.exitCode ?? "n/a"}).`,
    );
  }

  async triggerMcpCopy(): Promise<void> {
    const model = this.selectedModel();
    if (model === undefined || model.mcpJson === null) {
      await vscode.window.showInformationMessage(
        "No MCP definition available for the selected root yet — refresh first.",
      );
      return;
    }
    await vscode.env.clipboard.writeText(model.mcpJson);
    await vscode.window.showInformationMessage(
      "MCP server JSON copied to the clipboard.",
    );
  }

  /** Called by the watch supervisor for every stream event of an owned run. */
  onCliEvent(root: string, event: RepoiseEvent): void {
    const line = describeEvent(event);
    if (line !== null) {
      this.pushLog(line);
    }
    if (
      event.type === "completed" ||
      event.type === "cancelled" ||
      event.type === "failed"
    ) {
      const terminal = asTerminal(event.payload);
      if (event.type === "failed") {
        this.pushLog(
          `terminal failure detail: ${terminal.message ?? "operational"}${
            terminal.reason ? ` [reason ${terminal.reason}]` : ""
          }`,
        );
      }
      void this.refreshForRoot(root);
    } else if (event.type === "progress") {
      if (
        event.payload.branchSwitch === true ||
        event.payload.configChange === true
      ) {
        this.pushLog("scope inputs changed — refreshing status");
        void this.refreshForRoot(root);
      }
    }
    this.sendState();
  }

  private async refreshForRoot(root: string): Promise<void> {
    const model = this.roots.find((r) => r.path === root);
    if (model !== undefined) {
      await this.refreshRootModel(model);
    }
    this.sendState();
  }

  /** Watch state changed for a root (start/stop/restart/external). */
  onWatchState(root: string): void {
    const model = this.roots.find((r) => r.path === root);
    if (model !== undefined && root === this.selected) {
      void this.refreshForRoot(root);
      return;
    }
    this.sendState();
  }

  sendState(): void {
    if (this.panel === undefined) {
      return;
    }
    void this.panel.webview.postMessage({
      type: "state",
      payload: this.buildState(),
    });
  }

  dispose(): void {
    this.panel?.dispose();
    this.panel = undefined;
    this.roots = [];
    this.indexRunning.clear();
    this.logLines = [];
  }
}

function indexSummaryLine(summary: IndexSummary): string {
  if (summary.incomplete) {
    return `index did not finish cleanly (exit ${summary.exitCode ?? "n/a"}, no terminal event)`;
  }
  switch (summary.terminal) {
    case "completed":
      return `index completed${summary.scopeRoot ? ` (scope ${summary.scopeRoot})` : ""}`;
    case "cancelled":
      return "index cancelled";
    case "failed":
      return `index failed: ${summary.failedMessage ?? "operational"}${
        summary.failedReason ? ` [reason ${summary.failedReason}]` : ""
      }${summary.stderr ? ` — stderr: ${truncate(summary.stderr, 200)}` : ""}`;
    default:
      return `index ended with exit ${summary.exitCode ?? "n/a"}`;
  }
}
