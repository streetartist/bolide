package dev.bolide.ide;

import android.text.Editable;
import android.text.Spannable;
import android.text.SpannableStringBuilder;
import android.text.style.ForegroundColorSpan;

import java.util.regex.Matcher;
import java.util.regex.Pattern;

/** Lightweight Bolide lexer shared by the file editor and the REPL editor. */
public final class BolideHighlighter {
    public static final int COLOR_TEXT = 0xFFE8ECF5;
    private static final int COLOR_KEYWORD = 0xFFC792EA;
    private static final int COLOR_TYPE = 0xFF82AAFF;
    private static final int COLOR_STRING = 0xFFC3E88D;
    private static final int COLOR_COMMENT = 0xFF65708A;
    private static final int COLOR_NUMBER = 0xFFF78C6C;
    private static final int COLOR_FUNCTION = 0xFF61D6C4;
    private static final int COLOR_LITERAL = 0xFFFFCB6B;
    private static final int COLOR_OPERATOR = 0xFF89DDFF;
    private static final int COLOR_ATTRIBUTE = 0xFFFF5370;

    private static final Pattern KEYWORDS = Pattern.compile(
            "\\b(?:if|elif|else|while|for|in|break|continue|return|import|as|from|throw|try|catch|finally|match|select|timeout|default|fn|let|var|class|value|trait|impl|extern|struct|type|export|enum|union|async|await|spawn|thread|pool|scope|comptime|macro|attr|quote|yield|with|throws|inline|owned|ref|weak|unowned|and|or|not|self|super)\\b");
    private static final Pattern TYPES = Pattern.compile(
            "\\b(?:int|float|bool|str|bytes|bigint|decimal|dynamic|dyn|ptr|Future|Task|void|list|dict|channel|func|c_void|c_char|c_int|c_uint|c_long|c_ulong|c_float|c_double|c_bool|i8|u8|i16|u16|i32|u32|i64|u64|f32|f64|string)\\b");
    private static final Pattern LITERALS = Pattern.compile("\\b(?:true|false)\\b");
    private static final Pattern NUMBERS = Pattern.compile(
            "\\b(?:0x[0-9A-Fa-f][0-9A-Fa-f_]*|0b[01][01_]*|[0-9][0-9_]*(?:\\.[0-9][0-9_]*)?[BbDd]?)\\b");
    private static final Pattern FUNCTIONS = Pattern.compile(
            "\\b[A-Za-z_][A-Za-z0-9_]*\\s*(?=\\()");
    private static final Pattern ATTRIBUTES = Pattern.compile("@[A-Za-z_][A-Za-z0-9_]*");
    private static final Pattern OPERATORS = Pattern.compile(
            "=>|->|\\?\\?|\\+=|-=|\\*=|/=|%=|==|!=|<=|>=|<<|>>|[+\\-*/%=<>!&|?:]");

    private BolideHighlighter() {}

    private static final class SyntaxSpan extends ForegroundColorSpan {
        SyntaxSpan(int color) { super(color); }
    }

    public static SpannableStringBuilder highlight(CharSequence source) {
        SpannableStringBuilder result = new SpannableStringBuilder(source);
        apply(result);
        return result;
    }

    public static void apply(Editable text) {
        for (SyntaxSpan span : text.getSpans(0, text.length(), SyntaxSpan.class)) {
            text.removeSpan(span);
        }
        String source = text.toString();
        boolean[] protectedChars = new boolean[source.length()];
        lexCommentsAndStrings(text, source, protectedChars);
        applyPattern(text, source, protectedChars, KEYWORDS, COLOR_KEYWORD);
        applyPattern(text, source, protectedChars, TYPES, COLOR_TYPE);
        applyPattern(text, source, protectedChars, LITERALS, COLOR_LITERAL);
        applyPattern(text, source, protectedChars, NUMBERS, COLOR_NUMBER);
        applyPattern(text, source, protectedChars, FUNCTIONS, COLOR_FUNCTION);
        applyPattern(text, source, protectedChars, ATTRIBUTES, COLOR_ATTRIBUTE);
        applyPattern(text, source, protectedChars, OPERATORS, COLOR_OPERATOR);
    }

    private static void lexCommentsAndStrings(
            Editable text, String source, boolean[] protectedChars) {
        int length = source.length();
        int i = 0;
        while (i < length) {
            char c = source.charAt(i);
            if (c == '/' && i + 1 < length && source.charAt(i + 1) == '/') {
                int end = source.indexOf('\n', i + 2);
                if (end < 0) end = length;
                protect(text, protectedChars, i, end, COLOR_COMMENT);
                i = end;
            } else if (c == '/' && i + 1 < length && source.charAt(i + 1) == '*') {
                int close = source.indexOf("*/", i + 2);
                int end = close < 0 ? length : close + 2;
                protect(text, protectedChars, i, end, COLOR_COMMENT);
                i = end;
            } else if (c == '"') {
                int start = i;
                i++;
                boolean escaped = false;
                while (i < length) {
                    char current = source.charAt(i++);
                    if (escaped) {
                        escaped = false;
                    } else if (current == '\\') {
                        escaped = true;
                    } else if (current == '"') {
                        break;
                    }
                }
                protect(text, protectedChars, start, i, COLOR_STRING);
            } else {
                i++;
            }
        }
    }

    private static void protect(
            Editable text, boolean[] protectedChars, int start, int end, int color) {
        for (int i = start; i < end && i < protectedChars.length; i++) {
            protectedChars[i] = true;
        }
        if (start < end) {
            text.setSpan(new SyntaxSpan(color), start, end, Spannable.SPAN_EXCLUSIVE_EXCLUSIVE);
        }
    }

    private static void applyPattern(
            Editable text, String source, boolean[] protectedChars, Pattern pattern, int color) {
        Matcher matcher = pattern.matcher(source);
        while (matcher.find()) {
            boolean blocked = false;
            for (int i = matcher.start(); i < matcher.end(); i++) {
                if (protectedChars[i]) {
                    blocked = true;
                    break;
                }
            }
            if (!blocked) {
                text.setSpan(new SyntaxSpan(color), matcher.start(), matcher.end(),
                        Spannable.SPAN_EXCLUSIVE_EXCLUSIVE);
            }
        }
    }
}
