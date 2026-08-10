package dev.bolide.ide;

import android.app.Activity;
import android.app.AlertDialog;
import android.content.Context;
import android.content.Intent;
import android.content.res.AssetManager;
import android.graphics.Insets;
import android.graphics.Typeface;
import android.graphics.drawable.GradientDrawable;
import android.os.Build;
import android.os.Bundle;
import android.text.Editable;
import android.text.Spannable;
import android.text.SpannableString;
import android.text.SpannableStringBuilder;
import android.text.TextWatcher;
import android.text.style.ForegroundColorSpan;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.view.WindowInsets;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputMethodManager;
import android.widget.Button;
import android.widget.EditText;
import android.widget.FrameLayout;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;
import android.widget.Toast;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.List;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;

public final class MainActivity extends Activity implements BolideNative.IoBridge {
    private static final int BG = 0xFF0B1020;
    private static final int SURFACE = 0xFF151B2E;
    private static final int SURFACE_2 = 0xFF202840;
    private static final int OUTLINE = 0xFF2D3855;
    private static final int TEXT = 0xFFE8ECF5;
    private static final int MUTED = 0xFF8C96AD;
    private static final int ACCENT = 0xFF61D6C4;
    private static final int ACCENT_DARK = 0xFF082723;
    private static final int ERROR = 0xFFFF6B7A;
    private static final String STARTER = "// Bolide Android IDE\n"
            + "let name: str = input(\"你的名字: \" );\n"
            + "print(f\"你好，{name}！\");\n";

    private final ExecutorService nativeExecutor = Executors.newSingleThreadExecutor();
    private final List<String> replHistory = new ArrayList<>();
    private final Object inputMonitor = new Object();

    private File workspaceDir;
    private File bolideHomeDir;
    private File currentFile;
    private BolideEditor editor;
    private BolideEditor replInput;
    private TextView fileLabel;
    private TextView fileStatus;
    private TextView appStatus;
    private TextView console;
    private TextView replTranscript;
    private TextView replStatus;
    private TextView replPrompt;
    private ScrollView consoleScroll;
    private ScrollView replScroll;
    private LinearLayout editorPane;
    private LinearLayout replPane;
    private LinearLayout inputBar;
    private TextView inputPrompt;
    private EditText inputField;
    private Button editorTab;
    private Button replTab;
    private Button saveButton;
    private Button runButton;
    private Button replRunButton;
    private volatile boolean nativeOutputToRepl;
    private boolean loadingDocument;
    private boolean dirty;
    private boolean replBusy;
    private CountDownLatch inputLatch;
    private String inputValue = "";
    private int historyIndex;

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        getWindow().setSoftInputMode(android.view.WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE);
        workspaceDir = new File(getFilesDir(), "workspace");
        if (!workspaceDir.exists() && !workspaceDir.mkdirs()) {
            toast("无法创建工作目录");
        }
        bolideHomeDir = new File(getFilesDir(), "bolide_home");
        try {
            copyAssetTree(getAssets(), "std", new File(bolideHomeDir, "std"));
        } catch (IOException error) {
            toast("安装标准库失败: " + error.getMessage());
        }
        ensureStarterFile();
        buildUi();
        String last = getPreferences(MODE_PRIVATE).getString("last_file", "main.bl");
        File initial = safeWorkspaceFile(last);
        if (initial == null || !initial.isFile()) initial = new File(workspaceDir, "main.bl");
        openFile(initial);
        showEditor();
    }

    private void buildUi() {
        LinearLayout root = column(BG);
        applySystemBarInsets(root);

        LinearLayout brand = row(SURFACE, Gravity.CENTER_VERTICAL);
        brand.setPadding(dp(16), 0, dp(14), 0);
        TextView title = label("BOLIDE", 20, ACCENT);
        title.setTypeface(Typeface.DEFAULT_BOLD);
        title.setLetterSpacing(0.08f);
        brand.addView(title, lp(0, dp(54), 1));
        appStatus = label("就绪", 12, MUTED);
        brand.addView(appStatus, wrap(dp(54)));
        root.addView(brand, matchWrap());

        LinearLayout tabs = row(SURFACE, Gravity.CENTER);
        tabs.setPadding(dp(8), 0, dp(8), dp(6));
        editorTab = tabButton("代码", v -> showEditor());
        replTab = tabButton("终端", v -> showRepl());
        tabs.addView(editorTab, weighted(dp(40), 1, 3));
        tabs.addView(replTab, weighted(dp(40), 1, 3));
        root.addView(tabs, matchWrap());

        FrameLayout content = new FrameLayout(this);
        editorPane = buildEditorPane();
        replPane = buildReplPane();
        content.addView(editorPane, frameMatch());
        content.addView(replPane, frameMatch());
        root.addView(content, lp(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1));

        inputBar = buildInputBar();
        inputBar.setVisibility(View.GONE);
        root.addView(inputBar, matchWrap());
        setContentView(root);
    }

    private LinearLayout buildEditorPane() {
        LinearLayout pane = column(BG);

        LinearLayout actions = row(SURFACE_2, Gravity.CENTER_VERTICAL);
        actions.setPadding(dp(8), dp(6), dp(8), dp(6));
        actions.addView(actionButton("打开", v -> chooseFile()), weighted(dp(42), 1, 4));
        actions.addView(actionButton("新建", v -> promptNewFile()), weighted(dp(42), 1, 4));
        saveButton = actionButton("保存", v -> saveCurrentFile(true));
        actions.addView(saveButton, weighted(dp(42), 1, 4));
        runButton = primaryButton("▶ 运行", v -> runCurrentFile());
        actions.addView(runButton, weighted(dp(42), 1.2f, 4));
        pane.addView(actions, matchWrap());

        LinearLayout fileBar = row(BG, Gravity.CENTER_VERTICAL);
        fileBar.setPadding(dp(14), 0, dp(14), 0);
        fileLabel = label("", 13, TEXT);
        fileLabel.setTypeface(Typeface.MONOSPACE);
        fileBar.addView(fileLabel, lp(0, dp(40), 1));
        fileStatus = label("已保存", 11, MUTED);
        fileBar.addView(fileStatus, wrap(dp(40)));
        pane.addView(fileBar, matchWrap());

        editor = new BolideEditor(this);
        editor.setHint("在这里编写 Bolide 代码…");
        editor.addTextChangedListener(new TextWatcher() {
            @Override public void beforeTextChanged(CharSequence s, int start, int count, int after) {}
            @Override public void onTextChanged(CharSequence s, int start, int before, int count) {}
            @Override public void afterTextChanged(Editable s) {
                if (!loadingDocument && !dirty) {
                    dirty = true;
                    updateFileHeader();
                }
            }
        });
        pane.addView(editor, lp(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1));

        LinearLayout consoleHeader = row(SURFACE_2, Gravity.CENTER_VERTICAL);
        consoleHeader.setPadding(dp(14), 0, dp(8), 0);
        TextView caption = label("运行输出", 12, MUTED);
        consoleHeader.addView(caption, lp(0, dp(38), 1));
        consoleHeader.addView(compactButton("清空", v -> console.setText("")), wrap(dp(34)));
        pane.addView(consoleHeader, matchWrap());

        console = outputView();
        console.setText("点击“运行”执行当前文件。\n");
        consoleScroll = new ScrollView(this);
        consoleScroll.setFillViewport(true);
        consoleScroll.setBackgroundColor(SURFACE);
        consoleScroll.addView(console, matchWrap());
        pane.addView(consoleScroll, new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, dp(148)));
        return pane;
    }

    private LinearLayout buildReplPane() {
        LinearLayout pane = column(BG);

        LinearLayout sessionBar = row(SURFACE_2, Gravity.CENTER_VERTICAL);
        sessionBar.setPadding(dp(14), dp(4), dp(8), dp(4));
        replStatus = label("会话就绪 · 变量会保留", 12, MUTED);
        sessionBar.addView(replStatus, lp(0, dp(38), 1));
        sessionBar.addView(compactButton("清屏", v -> clearReplTranscript()), wrap(dp(38)));
        sessionBar.addView(compactButton("重启", v -> resetRepl()), wrap(dp(38)));
        pane.addView(sessionBar, matchWrap());

        replTranscript = outputView();
        replTranscript.setText(replBanner());
        replScroll = new ScrollView(this);
        replScroll.setFillViewport(true);
        replScroll.setBackgroundColor(BG);
        replScroll.addView(replTranscript, matchWrap());
        pane.addView(replScroll, lp(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1));

        LinearLayout terminalInput = column(SURFACE);
        terminalInput.setPadding(dp(10), dp(7), dp(10), dp(5));
        LinearLayout promptRow = row(SURFACE, Gravity.TOP);
        replPrompt = label(">>>", 14, ACCENT);
        replPrompt.setTypeface(Typeface.MONOSPACE, Typeface.BOLD);
        replPrompt.setGravity(Gravity.TOP);
        replPrompt.setPadding(0, dp(12), dp(8), 0);
        promptRow.addView(replPrompt, wrap(dp(52)));

        replInput = new BolideEditor(this);
        replInput.setHint("输入 Bolide 代码");
        replInput.setHorizontallyScrolling(false);
        replInput.setMinHeight(dp(48));
        replInput.setMaxHeight(dp(144));
        replInput.setPadding(dp(4), dp(8), dp(6), dp(8));
        // On a phone the IME enter key should always insert a newline. Running
        // code is an explicit action on the adjacent button.
        replInput.setImeOptions(EditorInfo.IME_ACTION_NONE
                | EditorInfo.IME_FLAG_NO_ENTER_ACTION
                | EditorInfo.IME_FLAG_NO_EXTRACT_UI);
        promptRow.addView(replInput, lp(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1));
        replRunButton = primaryButton("执行", v -> executeRepl());
        promptRow.addView(replRunButton, sized(dp(68), dp(44), 4));
        terminalInput.addView(promptRow, matchWrap());

        LinearLayout helper = row(SURFACE, Gravity.CENTER_VERTICAL);
        TextView hint = label("输入法回车换行 · 点“执行”运行", 10, MUTED);
        helper.addView(hint, lp(0, dp(34), 1));
        helper.addView(compactButton("↑", v -> previousHistory()), sized(dp(38), dp(34), 2));
        helper.addView(compactButton("↓", v -> nextHistory()), sized(dp(38), dp(34), 2));
        terminalInput.addView(helper, matchWrap());
        pane.addView(terminalInput, matchWrap());
        return pane;
    }

    private LinearLayout buildInputBar() {
        LinearLayout outer = column(SURFACE_2);
        outer.setPadding(dp(10), dp(6), dp(10), dp(8));
        inputPrompt = label("程序输入", 12, ACCENT);
        outer.addView(inputPrompt, matchWrap());
        LinearLayout inputRow = row(SURFACE_2, Gravity.CENTER_VERTICAL);
        inputField = new EditText(this);
        inputField.setSingleLine(true);
        inputField.setTextColor(TEXT);
        inputField.setHintTextColor(MUTED);
        inputField.setBackground(shape(BG, 8));
        inputField.setPadding(dp(10), 0, dp(10), 0);
        inputField.setImeOptions(EditorInfo.IME_ACTION_SEND);
        inputField.setOnEditorActionListener((v, action, event) -> {
            submitProgramInput();
            return true;
        });
        inputRow.addView(inputField, lp(0, dp(46), 1));
        inputRow.addView(primaryButton("发送", v -> submitProgramInput()), sized(dp(72), dp(42), 8));
        outer.addView(inputRow, matchWrap());
        return outer;
    }

    private void applySystemBarInsets(View root) {
        root.setOnApplyWindowInsetsListener((view, insets) -> {
            int top;
            int bottom;
            boolean keyboardVisible = false;
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                Insets bars = insets.getInsets(WindowInsets.Type.systemBars());
                Insets ime = insets.getInsets(WindowInsets.Type.ime());
                top = bars.top;
                bottom = Math.max(bars.bottom, ime.bottom);
                keyboardVisible = insets.isVisible(WindowInsets.Type.ime());
            } else {
                top = insets.getSystemWindowInsetTop();
                bottom = insets.getSystemWindowInsetBottom();
            }
            view.setPadding(0, top, 0, bottom);
            if (keyboardVisible && replPane != null && replPane.getVisibility() == View.VISIBLE) {
                scrollBottom(replScroll);
            }
            return insets;
        });
        root.requestApplyInsets();
    }

    private void showEditor() {
        editorPane.setVisibility(View.VISIBLE);
        replPane.setVisibility(View.GONE);
        setSelectedTab(editorTab, replTab);
    }

    private void showRepl() {
        saveCurrentFile(false);
        editorPane.setVisibility(View.GONE);
        replPane.setVisibility(View.VISIBLE);
        setSelectedTab(replTab, editorTab);
        replInput.requestFocus();
    }

    private void setSelectedTab(Button selected, Button other) {
        selected.setTextColor(ACCENT);
        selected.setBackground(shape(SURFACE_2, 9));
        other.setTextColor(MUTED);
        other.setBackground(shape(SURFACE, 9));
    }

    private void ensureStarterFile() {
        File starter = new File(workspaceDir, "main.bl");
        if (!starter.exists()) {
            try {
                writeUtf8(starter, STARTER);
            } catch (IOException e) {
                toast("创建示例失败: " + e.getMessage());
            }
        }
    }

    private void openFile(File file) {
        saveCurrentFile(false);
        try {
            currentFile = file;
            loadingDocument = true;
            editor.setText(readUtf8(file));
            editor.setSelection(0);
            editor.highlightNow();
            loadingDocument = false;
            dirty = false;
            updateFileHeader();
            String name = relativeName(file);
            getPreferences(MODE_PRIVATE).edit().putString("last_file", name).apply();
        } catch (IOException e) {
            loadingDocument = false;
            toast("打开失败: " + e.getMessage());
        }
    }

    private void updateFileHeader() {
        if (currentFile == null || fileLabel == null) return;
        fileLabel.setText(relativeName(currentFile));
        fileStatus.setText(dirty ? "未保存" : "已保存");
        fileStatus.setTextColor(dirty ? ACCENT : MUTED);
        if (saveButton != null) saveButton.setText(dirty ? "保存 •" : "保存");
    }

    private boolean saveCurrentFile(boolean notify) {
        if (currentFile == null || editor == null) return false;
        try {
            writeUtf8(currentFile, editor.getText().toString());
            dirty = false;
            updateFileHeader();
            if (notify) toast("已保存 " + relativeName(currentFile));
            return true;
        } catch (IOException e) {
            toast("保存失败: " + e.getMessage());
            return false;
        }
    }

    private void chooseFile() {
        saveCurrentFile(false);
        List<File> files = workspaceFiles();
        String[] names = files.stream().map(this::relativeName).toArray(String[]::new);
        new AlertDialog.Builder(this)
                .setTitle("工作区文件")
                .setItems(names, (dialog, which) -> {
                    openFile(files.get(which));
                    showEditor();
                })
                .setNegativeButton("取消", null)
                .show();
    }

    private void promptNewFile() {
        EditText name = new EditText(this);
        name.setHint("例如 utils/math.bl");
        name.setSingleLine(true);
        name.setTextColor(TEXT);
        name.setHintTextColor(MUTED);
        name.setPadding(dp(20), dp(8), dp(20), dp(8));
        new AlertDialog.Builder(this)
                .setTitle("新建 Bolide 文件")
                .setView(name)
                .setPositiveButton("创建", (dialog, which) -> createFile(name.getText().toString()))
                .setNegativeButton("取消", null)
                .show();
    }

    private void createFile(String rawName) {
        String name = rawName.trim().replace('\\', '/');
        if (name.isEmpty()) {
            toast("请输入文件名");
            return;
        }
        if (!name.endsWith(".bl")) name += ".bl";
        File file = safeWorkspaceFile(name);
        if (file == null) {
            toast("文件名无效，不能离开 workspace");
            return;
        }
        if (file.exists()) {
            toast("文件已存在");
            return;
        }
        try {
            File parent = file.getParentFile();
            if (parent != null) Files.createDirectories(parent.toPath());
            writeUtf8(file, "// " + file.getName() + "\n");
            openFile(file);
            showEditor();
            editor.requestFocus();
        } catch (IOException e) {
            toast("创建失败: " + e.getMessage());
        }
    }

    private File safeWorkspaceFile(String name) {
        try {
            File candidate = new File(workspaceDir, name).getCanonicalFile();
            String root = workspaceDir.getCanonicalPath() + File.separator;
            return candidate.getPath().startsWith(root) ? candidate : null;
        } catch (IOException e) {
            return null;
        }
    }

    private static String readUtf8(File file) throws IOException {
        return new String(Files.readAllBytes(file.toPath()), StandardCharsets.UTF_8);
    }

    private static void writeUtf8(File file, String content) throws IOException {
        Files.write(file.toPath(), content.getBytes(StandardCharsets.UTF_8));
    }

    private static void copyAssetTree(AssetManager assets, String assetPath, File target)
            throws IOException {
        String[] children = assets.list(assetPath);
        if (children != null && children.length > 0) {
            if (!target.isDirectory() && !target.mkdirs()) {
                throw new IOException("无法创建目录 " + target.getName());
            }
            for (String child : children) {
                copyAssetTree(assets, assetPath + "/" + child, new File(target, child));
            }
            return;
        }

        File parent = target.getParentFile();
        if (parent != null && !parent.isDirectory() && !parent.mkdirs()) {
            throw new IOException("无法创建目录 " + parent.getName());
        }
        try (InputStream input = assets.open(assetPath);
             FileOutputStream output = new FileOutputStream(target, false)) {
            byte[] buffer = new byte[16 * 1024];
            int read;
            while ((read = input.read(buffer)) >= 0) {
                if (read > 0) output.write(buffer, 0, read);
            }
        }
    }

    private List<File> workspaceFiles() {
        List<File> result = new ArrayList<>();
        try (java.util.stream.Stream<Path> paths = Files.walk(workspaceDir.toPath())) {
            paths.filter(Files::isRegularFile)
                    .filter(path -> path.getFileName().toString().endsWith(".bl"))
                    .sorted(Comparator.comparing(Path::toString))
                    .map(Path::toFile)
                    .forEach(result::add);
        } catch (IOException e) {
            toast("读取工作区失败: " + e.getMessage());
        }
        return result;
    }

    private void runCurrentFile() {
        if (!saveCurrentFile(false) || currentFile == null) return;
        showEditor();
        nativeOutputToRepl = false;
        setRunningUi(true, false);
        console.setText("▶ " + relativeName(currentFile) + "\n\n");
        String path = currentFile.getAbsolutePath();
        nativeExecutor.execute(() -> {
            String status;
            try {
                status = BolideNative.runFile(path, bolideHomeDir.getAbsolutePath(), this);
            } catch (Throwable error) {
                status = "原生运行失败: " + readableError(error);
            }
            String finalStatus = status;
            runOnUiThread(() -> {
                appendConsole("\n" + finalStatus + "\n");
                setRunningUi(false, false);
            });
        });
    }

    private void executeRepl() {
        if (replBusy) return;
        String code = replInput.getText().toString();
        String command = code.trim();
        if (command.isEmpty()) return;
        if (command.equals(":help")) {
            appendRepl("命令：:help  :clear  :reset\n输入法回车换行；点“执行”运行代码。\n");
            replInput.setText("");
            return;
        }
        if (command.equals(":clear")) {
            clearReplTranscript();
            replInput.setText("");
            return;
        }
        if (command.equals(":reset")) {
            resetRepl();
            return;
        }
        nativeOutputToRepl = true;
        replHistory.add(code);
        historyIndex = replHistory.size();
        appendReplCode(code);
        replInput.setText("");
        keepReplInputActive();
        setRunningUi(true, true);
        String baseDir = workspaceDir.getAbsolutePath();
        nativeExecutor.execute(() -> {
            String result;
            try {
                result = BolideNative.evalRepl(
                        code, baseDir, bolideHomeDir.getAbsolutePath(), this);
            } catch (Throwable error) {
                result = "错误：原生 REPL 失败: " + readableError(error);
            }
            String finalResult = result;
            runOnUiThread(() -> {
                if (!finalResult.isEmpty()) appendReplResult(finalResult + "\n");
                setRunningUi(false, true);
                keepReplInputActive();
            });
        });
    }

    private String readableError(Throwable error) {
        String message = error.getMessage();
        return message == null || message.isEmpty() ? error.getClass().getSimpleName() : message;
    }

    private void setRunningUi(boolean running, boolean repl) {
        if (repl) {
            replBusy = running;
            replRunButton.setEnabled(!running);
            replRunButton.setAlpha(running ? 0.5f : 1f);
            replPrompt.setText(running ? "•••" : ">>>");
            replStatus.setText(running ? "正在编译并执行…" : "会话就绪 · 变量会保留");
        } else {
            runButton.setEnabled(!running);
            runButton.setAlpha(running ? 0.5f : 1f);
        }
        appStatus.setText(running ? "运行中" : "就绪");
        appStatus.setTextColor(running ? ACCENT : MUTED);
    }

    private void resetRepl() {
        if (replBusy) return;
        nativeExecutor.execute(BolideNative::resetRepl);
        replHistory.clear();
        historyIndex = 0;
        replInput.setText("");
        replTranscript.setText(replBanner() + "会话已重启。\n");
        replStatus.setText("新会话 · 变量已清除");
        scrollBottom(replScroll);
    }

    private void clearReplTranscript() {
        replTranscript.setText(replBanner());
        scrollBottom(replScroll);
    }

    private String replBanner() {
        return "Bolide Android REPL\n输入 :help 查看命令。\n\n";
    }

    private void keepReplInputActive() {
        replInput.requestFocus();
        replInput.post(() -> {
            replInput.requestFocus();
            InputMethodManager keyboard =
                    (InputMethodManager) getSystemService(Context.INPUT_METHOD_SERVICE);
            keyboard.showSoftInput(replInput, InputMethodManager.SHOW_IMPLICIT);
        });
    }

    private void previousHistory() {
        if (replBusy || replHistory.isEmpty()) return;
        historyIndex = Math.max(0, historyIndex - 1);
        setReplHistoryText(replHistory.get(historyIndex));
    }

    private void nextHistory() {
        if (replBusy || replHistory.isEmpty()) return;
        historyIndex = Math.min(replHistory.size(), historyIndex + 1);
        setReplHistoryText(historyIndex == replHistory.size() ? "" : replHistory.get(historyIndex));
    }

    private void setReplHistoryText(String value) {
        replInput.setText(value);
        replInput.setSelection(value.length());
        replInput.highlightNow();
    }

    private void appendReplCode(String code) {
        String[] lines = code.split("\\n", -1);
        SpannableStringBuilder entry = new SpannableStringBuilder();
        for (int index = 0; index < lines.length; index++) {
            String prefix = index == 0 ? ">>> " : "... ";
            SpannableString prompt = new SpannableString(prefix);
            prompt.setSpan(new ForegroundColorSpan(ACCENT), 0, prompt.length(),
                    Spannable.SPAN_EXCLUSIVE_EXCLUSIVE);
            entry.append(prompt);
            entry.append(BolideHighlighter.highlight(lines[index]));
            entry.append("\n");
        }
        replTranscript.append(entry);
        scrollBottom(replScroll);
    }

    private void appendReplResult(String text) {
        SpannableString value = new SpannableString(text);
        int color = text.startsWith("错误") ? ERROR : ACCENT;
        value.setSpan(new ForegroundColorSpan(color), 0, value.length(),
                Spannable.SPAN_EXCLUSIVE_EXCLUSIVE);
        appendRepl(value);
    }

    @Override
    public void onNativeOutput(String text) {
        boolean toRepl = nativeOutputToRepl;
        runOnUiThread(() -> {
            if (toRepl) appendRepl(text); else appendConsole(text);
        });
    }

    @Override
    public String onNativeInput(String prompt) {
        CountDownLatch latch = new CountDownLatch(1);
        synchronized (inputMonitor) {
            inputLatch = latch;
            inputValue = "";
        }
        runOnUiThread(() -> {
            appStatus.setText("等待输入");
            appStatus.setTextColor(ACCENT);
            inputPrompt.setText(prompt == null || prompt.isEmpty() ? "程序正在等待输入" : prompt);
            inputField.setText("");
            inputBar.setVisibility(View.VISIBLE);
            // Wait until the newly visible bar has been laid out. Requesting
            // focus before that can open the IME while leaving no active text
            // cursor on edge-to-edge Android versions.
            inputField.post(() -> {
                inputField.requestFocus();
                inputField.setSelection(inputField.getText().length());
                ((InputMethodManager) getSystemService(Context.INPUT_METHOD_SERVICE))
                        .showSoftInput(inputField, InputMethodManager.SHOW_IMPLICIT);
            });
        });
        try {
            latch.await();
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
        }
        synchronized (inputMonitor) {
            return inputValue;
        }
    }

    @Override
    public boolean onNativeGuiRequest(String title) {
        CountDownLatch launched = new CountDownLatch(1);
        AtomicBoolean success = new AtomicBoolean(false);
        runOnUiThread(() -> {
            try {
                Intent intent = new Intent(this, BolideGuiActivity.class);
                intent.putExtra(BolideGuiActivity.EXTRA_TITLE,
                        title == null || title.isEmpty() ? "Bolide GUI" : title);
                intent.addFlags(Intent.FLAG_ACTIVITY_REORDER_TO_FRONT
                        | Intent.FLAG_ACTIVITY_SINGLE_TOP);
                startActivity(intent);
                success.set(true);
            } catch (Throwable error) {
                toast("无法打开 GUI: " + readableError(error));
            } finally {
                launched.countDown();
            }
        });
        try {
            return launched.await(5, TimeUnit.SECONDS) && success.get();
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
            return false;
        }
    }

    @Override
    public void onNativeGuiClosed() {
        if (BolideGuiActivity.returnCurrentToIde()) return;
        runOnUiThread(() -> {
            Intent intent = new Intent(this, MainActivity.class);
            intent.addFlags(Intent.FLAG_ACTIVITY_REORDER_TO_FRONT
                    | Intent.FLAG_ACTIVITY_SINGLE_TOP);
            startActivity(intent);
        });
    }

    private void submitProgramInput() {
        CountDownLatch latch;
        String value = inputField.getText().toString();
        synchronized (inputMonitor) {
            latch = inputLatch;
            if (latch == null) return;
            inputValue = value;
            inputLatch = null;
        }
        inputBar.setVisibility(View.GONE);
        appStatus.setText("运行中");
        if (nativeOutputToRepl) appendRepl(value + "\n"); else appendConsole(value + "\n");
        latch.countDown();
    }

    private void appendConsole(CharSequence text) {
        console.append(text);
        scrollBottom(consoleScroll);
    }

    private void appendRepl(CharSequence text) {
        replTranscript.append(text);
        scrollBottom(replScroll);
    }

    private void scrollBottom(ScrollView scroll) {
        // fullScroll(FOCUS_DOWN) performs focus navigation and used to steal
        // the cursor from the REPL EditText. Move only the scroll position.
        scroll.post(() -> {
            View child = scroll.getChildAt(0);
            if (child == null) return;
            int bottom = Math.max(0, child.getHeight() - scroll.getHeight());
            scroll.scrollTo(0, bottom);
        });
    }

    private String relativeName(File file) {
        return workspaceDir.toPath().relativize(file.toPath()).toString().replace('\\', '/');
    }

    private LinearLayout row(int color, int gravity) {
        LinearLayout layout = new LinearLayout(this);
        layout.setOrientation(LinearLayout.HORIZONTAL);
        layout.setGravity(gravity);
        layout.setBackgroundColor(color);
        return layout;
    }

    private LinearLayout column(int color) {
        LinearLayout layout = new LinearLayout(this);
        layout.setOrientation(LinearLayout.VERTICAL);
        layout.setBackgroundColor(color);
        return layout;
    }

    private TextView label(String text, int sp, int color) {
        TextView view = new TextView(this);
        view.setText(text);
        view.setTextSize(sp);
        view.setTextColor(color);
        view.setGravity(Gravity.CENTER_VERTICAL);
        return view;
    }

    private TextView outputView() {
        TextView view = label("", 13, TEXT);
        view.setTypeface(Typeface.MONOSPACE);
        view.setTextIsSelectable(true);
        view.setGravity(Gravity.TOP | Gravity.START);
        view.setPadding(dp(14), dp(12), dp(14), dp(18));
        view.setBackgroundColor(BG);
        view.setLineSpacing(dp(2), 1f);
        return view;
    }

    private Button tabButton(String text, View.OnClickListener listener) {
        Button button = baseButton(text, listener);
        button.setTextSize(13);
        button.setTextColor(MUTED);
        button.setBackground(shape(SURFACE, 9));
        return button;
    }

    private Button actionButton(String text, View.OnClickListener listener) {
        Button button = baseButton(text, listener);
        button.setBackground(shape(SURFACE, 9));
        return button;
    }

    private Button compactButton(String text, View.OnClickListener listener) {
        Button button = baseButton(text, listener);
        button.setTextSize(11);
        button.setTextColor(MUTED);
        button.setBackgroundColor(android.graphics.Color.TRANSPARENT);
        button.setPadding(dp(9), 0, dp(9), 0);
        return button;
    }

    private Button primaryButton(String text, View.OnClickListener listener) {
        Button button = baseButton(text, listener);
        button.setTextColor(ACCENT_DARK);
        button.setTypeface(Typeface.DEFAULT, Typeface.BOLD);
        button.setBackground(shape(ACCENT, 9));
        return button;
    }

    private Button baseButton(String text, View.OnClickListener listener) {
        Button button = new Button(this);
        button.setText(text);
        button.setTextSize(12);
        button.setTextColor(TEXT);
        button.setAllCaps(false);
        button.setMinWidth(0);
        button.setMinimumWidth(0);
        button.setMinHeight(0);
        button.setMinimumHeight(0);
        button.setPadding(dp(8), 0, dp(8), 0);
        button.setGravity(Gravity.CENTER);
        button.setStateListAnimator(null);
        button.setOnClickListener(listener);
        return button;
    }

    private GradientDrawable shape(int color, int radiusDp) {
        GradientDrawable drawable = new GradientDrawable();
        drawable.setColor(color);
        drawable.setCornerRadius(dp(radiusDp));
        if (color == SURFACE) drawable.setStroke(dp(1), OUTLINE);
        return drawable;
    }

    private LinearLayout.LayoutParams matchWrap() {
        return new LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT);
    }

    private LinearLayout.LayoutParams lp(int width, int height, float weight) {
        return new LinearLayout.LayoutParams(width, height, weight);
    }

    private LinearLayout.LayoutParams wrap(int height) {
        return new LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, height);
    }

    private LinearLayout.LayoutParams weighted(int height, float weight, int marginDp) {
        LinearLayout.LayoutParams params = new LinearLayout.LayoutParams(0, height, weight);
        params.setMargins(dp(marginDp), 0, dp(marginDp), 0);
        return params;
    }

    private LinearLayout.LayoutParams sized(int width, int height, int leftMarginDp) {
        LinearLayout.LayoutParams params = new LinearLayout.LayoutParams(width, height);
        params.setMargins(dp(leftMarginDp), 0, 0, 0);
        return params;
    }

    private FrameLayout.LayoutParams frameMatch() {
        return new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT);
    }

    private int dp(int value) {
        return Math.round(value * getResources().getDisplayMetrics().density);
    }

    private void toast(String message) {
        Toast.makeText(this, message, Toast.LENGTH_SHORT).show();
    }

    @Override
    protected void onPause() {
        saveCurrentFile(false);
        super.onPause();
    }

    @Override
    protected void onDestroy() {
        synchronized (inputMonitor) {
            if (inputLatch != null) {
                inputLatch.countDown();
                inputLatch = null;
            }
        }
        nativeExecutor.shutdownNow();
        super.onDestroy();
    }
}
