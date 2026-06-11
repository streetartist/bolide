import * as vscode from 'vscode';
import * as path from 'path';
import * as fs from 'fs';

/**
 * 查找 bolide 可执行文件，按以下优先级：
 * 1. VS Code 设置 bolide.executablePath
 * 2. 系统 PATH 环境变量中的 bolide
 * 3. 当前工作区目录下的 bolide
 * 4. 以上都没有则弹出配置提示
 */
async function resolveExecutablePath(config: vscode.WorkspaceConfiguration): Promise<string | undefined> {
    // 1. 插件设置中的路径
    const configured = config.get<string>('executablePath') || '';
    if (configured && fs.existsSync(configured)) {
        return configured;
    }

    const exeName = process.platform === 'win32' ? 'bolide.exe' : 'bolide';

    // 2. 系统 PATH 环境变量
    // 在终端中 which/where 查找
    const pathResult = await new Promise<string | undefined>((resolve) => {
        const { exec } = require('child_process');
        const cmd = process.platform === 'win32'
            ? `where ${exeName} 2>nul`
            : `which ${exeName} 2>/dev/null`;
        exec(cmd, (err: any, stdout: string) => {
            if (!err && stdout.trim()) {
                // 取第一行
                const first = stdout.trim().split('\n')[0].trim();
                if (fs.existsSync(first)) {
                    resolve(first);
                    return;
                }
            }
            resolve(undefined);
        });
    });

    if (pathResult) {
        return pathResult;
    }

    // 3. 当前工作区目录
    const workspaceFolders = vscode.workspace.workspaceFolders;
    if (workspaceFolders && workspaceFolders.length > 0) {
        const cwd = workspaceFolders[0].uri.fsPath;
        // 也检查 target/release 下的（开发期常见位置）
        const candidates = [
            path.join(cwd, exeName),
            path.join(cwd, 'target', 'release', exeName),
        ];
        for (const candidate of candidates) {
            if (fs.existsSync(candidate)) {
                return candidate;
            }
        }
    }

    // 4. 都没找到 → 弹出配置提示
    const result = await vscode.window.showWarningMessage(
        `Bolide executable (${exeName}) not found in config, PATH, or workspace.`,
        'Configure Path',
        'Cancel'
    );

    if (result === 'Configure Path') {
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
            const newPath = selectedPath[0].fsPath;
            await config.update('executablePath', newPath, vscode.ConfigurationTarget.Global);
            return newPath;
        }
    }

    return undefined;
}

/**
 * 在 Bolide 终端中执行命令
 */
function runInTerminal(executablePath: string, args: string[], cwd?: string): vscode.Terminal {
    let terminal = vscode.window.terminals.find(t => t.name === 'Bolide');
    if (!terminal) {
        terminal = vscode.window.createTerminal('Bolide');
    }
    terminal.show();

    // 如果指定了工作目录，先 cd
    if (cwd) {
        if (process.platform === 'win32') {
            terminal.sendText(`cd "${cwd}"`);
        } else {
            terminal.sendText(`cd '${cwd}'`);
        }
    }

    const quotedExe = process.platform === 'win32'
        ? `& "${executablePath}"`
        : `'${executablePath}'`;

    const quotedArgs = args.map(a => `"${a}"`).join(' ');
    terminal.sendText(`${quotedExe} ${quotedArgs}`);

    return terminal;
}

export function activate(context: vscode.ExtensionContext) {
    console.log('Bolide extension activated');

    // =========================================
    // 状态栏按钮
    // =========================================
    const runStatusBar = vscode.window.createStatusBarItem(
        vscode.StatusBarAlignment.Right,
        100
    );
    runStatusBar.command = 'bolide.run';
    runStatusBar.text = '$(play) Bolide Run';
    runStatusBar.tooltip = 'Run current Bolide file (JIT)';
    context.subscriptions.push(runStatusBar);

    const buildStatusBar = vscode.window.createStatusBarItem(
        vscode.StatusBarAlignment.Right,
        99
    );
    buildStatusBar.command = 'bolide.build';
    buildStatusBar.text = '$(package) Bolide Build';
    buildStatusBar.tooltip = 'Compile and run current Bolide file (AOT)';
    context.subscriptions.push(buildStatusBar);

    // 只在 Bolide 文件激活时显示状态栏按钮
    function updateStatusBar(editor: vscode.TextEditor | undefined) {
        if (editor && editor.document.languageId === 'bolide') {
            runStatusBar.show();
            buildStatusBar.show();
        } else {
            runStatusBar.hide();
            buildStatusBar.hide();
        }
    }

    // 监听活跃编辑器变化
    updateStatusBar(vscode.window.activeTextEditor);
    context.subscriptions.push(
        vscode.window.onDidChangeActiveTextEditor(updateStatusBar)
    );

    // =========================================
    // bolide.run — JIT 运行
    // =========================================
    const runCommand = vscode.commands.registerCommand('bolide.run', async () => {
        const editor = vscode.window.activeTextEditor;
        if (!editor) {
            vscode.window.showErrorMessage('No active editor');
            return;
        }
        if (editor.document.languageId !== 'bolide') {
            vscode.window.showErrorMessage('Current file is not a Bolide file (.bl)');
            return;
        }

        await editor.document.save();

        const filePath = editor.document.fileName;
        const config = vscode.workspace.getConfiguration('bolide');
        const exe = await resolveExecutablePath(config);
        if (!exe) { return; }

        runInTerminal(exe, ['run', filePath], path.dirname(filePath));
    });
    context.subscriptions.push(runCommand);

    // =========================================
    // bolide.build — AOT 编译并运行
    // =========================================
    const buildCommand = vscode.commands.registerCommand('bolide.build', async () => {
        const editor = vscode.window.activeTextEditor;
        if (!editor) {
            vscode.window.showErrorMessage('No active editor');
            return;
        }
        if (editor.document.languageId !== 'bolide') {
            vscode.window.showErrorMessage('Current file is not a Bolide file (.bl)');
            return;
        }

        await editor.document.save();

        const filePath = editor.document.fileName;
        const fileDir = path.dirname(filePath);
        const baseName = path.basename(filePath, '.bl');
        const exeName = process.platform === 'win32' ? `${baseName}.exe` : baseName;
        const outputPath = path.join(fileDir, exeName);

        const config = vscode.workspace.getConfiguration('bolide');
        const exe = await resolveExecutablePath(config);
        if (!exe) { return; }

        // 用终端执行：先编译，成功后再运行
        let terminal = vscode.window.terminals.find(t => t.name === 'Bolide');
        if (!terminal) {
            terminal = vscode.window.createTerminal('Bolide');
        }
        terminal.show();

        if (process.platform === 'win32') {
            terminal.sendText(`cd "${fileDir}"`);
            terminal.sendText(`& "${exe}" compile "${filePath}" -o "${outputPath}"; if ($?) { & "${outputPath}" }`);
        } else {
            terminal.sendText(`cd '${fileDir}'`);
            terminal.sendText(`'${exe}' compile '${filePath}' -o '${outputPath}' && '${outputPath}'`);
        }
    });
    context.subscriptions.push(buildCommand);
}

export function deactivate() {}
