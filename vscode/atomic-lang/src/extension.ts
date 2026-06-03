import * as vscode from 'vscode';
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  Executable,
} from 'vscode-languageclient/node';

let client: LanguageClient | undefined;

export function activate(context: vscode.ExtensionContext) {
  const config = vscode.workspace.getConfiguration('atomic.lsp');
  const enabled = config.get<boolean>('enabled', true);

  if (!enabled) {
    console.log('Atomic LSP disabled by configuration');
    return;
  }

  const lspPath = config.get<string>('path', 'atomic-lsp');

  const serverOptions: ServerOptions = {
    run: {
      command: lspPath,
      options: { env: { ...process.env } },
    } as Executable,
    debug: {
      command: lspPath,
      options: { env: { ...process.env, RUST_LOG: 'debug' } },
    } as Executable,
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [
      { scheme: 'file', language: 'atomic' },
      { scheme: 'untitled', language: 'atomic' },
    ],
    synchronize: {
      fileEvents: vscode.workspace.createFileSystemWatcher(
        '**/*.{at,atom}'
      ),
    },
    outputChannelName: 'Atomic Language Server',
    traceOutputChannel: vscode.window.createOutputChannel(
      'Atomic LSP Trace'
    ),
  };

  client = new LanguageClient(
    'atomic-lsp',
    'Atomic Language Server',
    serverOptions,
    clientOptions
  );

  client.start().catch((err) => {
    vscode.window.showWarningMessage(
      `Atomic LSP failed to start: ${err.message}`
    );
  });

  context.subscriptions.push(
    vscode.commands.registerCommand('atomic.restartLsp', async () => {
      if (client) {
        await client.stop();
        await client.start();
        vscode.window.showInformationMessage('Atomic LSP restarted');
      }
    })
  );
}

export function deactivate(): Thenable<void> | undefined {
  if (client) {
    return client.stop();
  }
}
