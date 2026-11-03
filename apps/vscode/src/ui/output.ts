import * as vscode from "vscode";

export function createOutputChannel(
  context: vscode.ExtensionContext,
): vscode.OutputChannel {
  const channel = vscode.window.createOutputChannel("Repoise");
  context.subscriptions.push(channel);
  return channel;
}

export function log(channel: vscode.OutputChannel, message: string): void {
  channel.appendLine(message);
}

export function truncate(text: string, max = 400): string {
  const trimmed = text.trim().replace(/\s+/g, " ");
  return trimmed.length > max ? `${trimmed.slice(0, max)}…` : trimmed;
}
