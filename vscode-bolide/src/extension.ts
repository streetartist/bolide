import * as vscode from 'vscode';
import * as path from 'path';

// 获取可执行文件路径的辅助函数
async function getExecutablePath(config: vscode.WorkspaceConfiguration): Promise<string | undefined> {
    let executablePath = config.get<string>('executablePath') || '';

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
                return undefined;
            }
        } else {
            return undefined;
        }
    }

    return executablePath;
}

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

        await document.save();

        const filePath = document.fileName;
        const config = vscode.workspace.getConfiguration('bolide');
        const executablePath = await getExecutablePath(config);

        if (!executablePath) {
            return;
        }

        let terminal = vscode.window.terminals.find(t => t.name === 'Bolide');
        if (!terminal) {
            terminal = vscode.window.createTerminal('Bolide');
        }

        terminal.show();

        let command: string;
        if (process.platform === 'win32') {
            command = `& "${executablePath}" run "${filePath}"`;
        } else {
            command = `"${executablePath}" run "${filePath}"`;
        }
        terminal.sendText(command);
    });

    context.subscriptions.push(runCommand);

    // Register the build command
    const buildCommand = vscode.commands.registerCommand('bolide.build', async () => {
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

        await document.save();

        const filePath = document.fileName;
        const config = vscode.workspace.getConfiguration('bolide');
        const executablePath = await getExecutablePath(config);

        if (!executablePath) {
            return;
        }

        // 生成输出文件名（去掉 .bl 扩展名）
        const outputName = path.basename(filePath, '.bl');
        const outputDir = path.dirname(filePath);
        // Windows 上添加 .exe 扩展名
        const exeName = process.platform === 'win32' ? `${outputName}.exe` : outputName;
        const outputPath = path.join(outputDir, exeName);

        let terminal = vscode.window.terminals.find(t => t.name === 'Bolide');
        if (!terminal) {
            terminal = vscode.window.createTerminal('Bolide');
        }

        terminal.show();

        // 编译并运行
        let command: string;
        if (process.platform === 'win32') {
            // PowerShell 兼容语法：使用 ; if ($?) 来实现条件执行
            command = `& "${executablePath}" compile "${filePath}" -o "${outputPath}"; if ($?) { & "${outputPath}" }`;
        } else {
            command = `"${executablePath}" compile "${filePath}" -o "${outputPath}" && "${outputPath}"`;
        }
        terminal.sendText(command);

        vscode.window.showInformationMessage(`Building and running: ${exeName}`);
    });

    context.subscriptions.push(buildCommand);
}

export function deactivate() { }
