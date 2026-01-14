import * as vscode from 'vscode';
import * as path from 'path';

export function activate(context: vscode.ExtensionContext) {
    console.log('Bolide extension is now active');

    // Register the run command
    const runCommand = vscode.commands.registerCommand('bolide.run', async () => {
        const editor = vscode.window.activeTextEditor;

        if (!editor) {
            vscode.window.showErrorMessage('No active editor found');
            return;
        }

        const document = editor.document;

        if (document.languageId !== 'bolide') {
            vscode.window.showErrorMessage('Current file is not a Bolide file (.bl)');
            return;
        }

        // Save the file before running
        await document.save();

        const filePath = document.fileName;
        const config = vscode.workspace.getConfiguration('bolide');
        let executablePath = config.get<string>('executablePath') || '';

        // If executable path is not set, try to find it or prompt user
        if (!executablePath) {
            const result = await vscode.window.showWarningMessage(
                'Bolide executable path is not configured. Would you like to configure it now?',
                'Configure',
                'Cancel'
            );

            if (result === 'Configure') {
                const selectedPath = await vscode.window.showOpenDialog({
                    canSelectFiles: true,
                    canSelectFolders: false,
                    canSelectMany: false,
                    title: 'Select Bolide Executable',
                    filters: process.platform === 'win32'
                        ? { 'Executable': ['exe'] }
                        : { 'Executable': ['*'] }
                });

                if (selectedPath && selectedPath.length > 0) {
                    executablePath = selectedPath[0].fsPath;
                    await config.update('executablePath', executablePath, vscode.ConfigurationTarget.Global);
                } else {
                    return;
                }
            } else {
                return;
            }
        }

        // Get or create terminal
        let terminal = vscode.window.terminals.find(t => t.name === 'Bolide');
        if (!terminal) {
            terminal = vscode.window.createTerminal('Bolide');
        }

        terminal.show();

        // Run the Bolide file
        // Use & operator for PowerShell to handle paths with spaces
        let command: string;
        if (process.platform === 'win32') {
            command = `& "${executablePath}" run "${filePath}"`;
        } else {
            command = `"${executablePath}" run "${filePath}"`;
        }
        terminal.sendText(command);
    });

    context.subscriptions.push(runCommand);
}

export function deactivate() { }
