package dev.bolide.ide;

import android.content.Context;
import android.graphics.Typeface;
import android.text.Editable;
import android.text.TextWatcher;
import android.util.AttributeSet;
import android.view.Gravity;
import android.view.inputmethod.EditorInfo;
import android.widget.EditText;

public final class BolideEditor extends EditText {
    private boolean highlighting;
    private final Runnable highlightTask = () -> {
        Editable editable = getText();
        if (editable == null || highlighting) return;
        highlighting = true;
        BolideHighlighter.apply(editable);
        highlighting = false;
    };

    public BolideEditor(Context context) {
        this(context, null);
    }

    public BolideEditor(Context context, AttributeSet attrs) {
        super(context, attrs);
        setTypeface(Typeface.MONOSPACE);
        setTextSize(14);
        setTextColor(BolideHighlighter.COLOR_TEXT);
        setHintTextColor(0xFF65708A);
        setBackgroundColor(0xFF0B1020);
        setGravity(Gravity.TOP | Gravity.START);
        setPadding(dp(14), dp(12), dp(14), dp(12));
        setHorizontallyScrolling(true);
        setHorizontalScrollBarEnabled(true);
        setVerticalScrollBarEnabled(true);
        setSingleLine(false);
        setInputType(EditorInfo.TYPE_CLASS_TEXT
                | EditorInfo.TYPE_TEXT_FLAG_MULTI_LINE
                | EditorInfo.TYPE_TEXT_FLAG_NO_SUGGESTIONS);
        setImeOptions(EditorInfo.IME_FLAG_NO_EXTRACT_UI);
        addTextChangedListener(new TextWatcher() {
            @Override public void beforeTextChanged(CharSequence s, int start, int count, int after) {}
            @Override public void onTextChanged(CharSequence s, int start, int before, int count) {}
            @Override public void afterTextChanged(Editable s) {
                if (highlighting) return;
                removeCallbacks(highlightTask);
                postDelayed(highlightTask, 45);
            }
        });
    }

    public void highlightNow() {
        removeCallbacks(highlightTask);
        highlightTask.run();
    }

    private int dp(int value) {
        return Math.round(value * getResources().getDisplayMetrics().density);
    }
}
