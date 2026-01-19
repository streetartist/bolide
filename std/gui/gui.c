// Bolide GUI Library - Modern HiDPI Win32 Implementation
// Complete rewrite with full control support

#define BOLIDE_GUI_EXPORTS
#define UNICODE
#define _UNICODE

#include "gui.h"

#ifdef _WIN32

#include <windows.h>
#include <commctrl.h>
#include <commdlg.h>
#include <shlobj.h>
#include <stdlib.h>
#include <string.h>
#include <stdio.h>

// ============================================================
// 内部状态
// ============================================================

static HINSTANCE g_hInstance = NULL;
static int g_control_id = 1000;
static HFONT g_hFont = NULL;
static HWND g_main_window = NULL;
static int g_base_dpi = 96;
static int g_initialized = 0;

// ============================================================
// 回调存储
// ============================================================

#define MAX_CALLBACKS 256

typedef enum {
    CB_CLICK,
    CB_CHANGE,
    CB_SELECT,
    CB_PAINT,
    CB_MOUSE_MOVE,
    CB_MOUSE_DOWN,
    CB_MOUSE_UP,
    CB_KEY_DOWN,
    CB_KEY_UP,
    CB_CLOSE,
    CB_RESIZE
} CallbackType;

typedef struct {
    HWND hwnd;
    CallbackType type;
    void* callback;
} CallbackEntry;

static CallbackEntry g_callbacks[MAX_CALLBACKS];
static int g_callback_count = 0;

// 定时器
#define MAX_TIMERS 32
typedef struct {
    UINT_PTR id;
    void (*callback)(void);
} TimerEntry;

static TimerEntry g_timers[MAX_TIMERS];
static int g_timer_count = 0;
static UINT_PTR g_timer_id_counter = 1;

// 文本缓冲区
static char g_text_buffer[8192];
static wchar_t g_wtext_buffer[4096];

// 画布数据
#define MAX_CANVAS 32
typedef struct {
    HWND hwnd;
    HDC memDC;
    HBITMAP memBitmap;
    HBITMAP oldBitmap;
    int width;
    int height;
} CanvasData;

static CanvasData g_canvas[MAX_CANVAS];
static int g_canvas_count = 0;

// ============================================================
// 工具函数
// ============================================================

static wchar_t* utf8_to_utf16(const char* utf8) {
    if (!utf8) return NULL;
    int len = MultiByteToWideChar(CP_UTF8, 0, utf8, -1, NULL, 0);
    wchar_t* utf16 = (wchar_t*)malloc(len * sizeof(wchar_t));
    if (utf16) {
        MultiByteToWideChar(CP_UTF8, 0, utf8, -1, utf16, len);
    }
    return utf16;
}

static char* utf16_to_utf8(const wchar_t* utf16) {
    if (!utf16) return NULL;
    int len = WideCharToMultiByte(CP_UTF8, 0, utf16, -1, NULL, 0, NULL, NULL);
    char* utf8 = (char*)malloc(len);
    if (utf8) {
        WideCharToMultiByte(CP_UTF8, 0, utf16, -1, utf8, len, NULL, NULL);
    }
    return utf8;
}

static void register_callback(HWND hwnd, CallbackType type, void* callback) {
    if (g_callback_count < MAX_CALLBACKS) {
        g_callbacks[g_callback_count].hwnd = hwnd;
        g_callbacks[g_callback_count].type = type;
        g_callbacks[g_callback_count].callback = callback;
        g_callback_count++;
    }
}

static void* find_callback(HWND hwnd, CallbackType type) {
    for (int i = 0; i < g_callback_count; i++) {
        if (g_callbacks[i].hwnd == hwnd && g_callbacks[i].type == type) {
            return g_callbacks[i].callback;
        }
    }
    return NULL;
}

static CanvasData* find_canvas(HWND hwnd) {
    for (int i = 0; i < g_canvas_count; i++) {
        if (g_canvas[i].hwnd == hwnd) {
            return &g_canvas[i];
        }
    }
    return NULL;
}

static HFONT create_scaled_font(int dpi) {
    int fontSize = MulDiv(14, dpi, 96);
    return CreateFontW(
        -fontSize, 0, 0, 0, FW_NORMAL, FALSE, FALSE, FALSE,
        DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
        CLEARTYPE_QUALITY, DEFAULT_PITCH | FF_DONTCARE, L"Segoe UI"
    );
}

// ============================================================
// 画布窗口过程
// ============================================================

static LRESULT CALLBACK CanvasWndProc(HWND hwnd, UINT msg, WPARAM wParam, LPARAM lParam) {
    switch (msg) {
        case WM_PAINT: {
            PAINTSTRUCT ps;
            HDC hdc = BeginPaint(hwnd, &ps);
            CanvasData* cd = find_canvas(hwnd);
            if (cd && cd->memDC) {
                BitBlt(hdc, 0, 0, cd->width, cd->height, cd->memDC, 0, 0, SRCCOPY);
            }
            EndPaint(hwnd, &ps);
            return 0;
        }
        case WM_ERASEBKGND:
            return 1;
        case WM_MOUSEMOVE: {
            void (*cb)(int, int) = (void (*)(int, int))find_callback(hwnd, CB_MOUSE_MOVE);
            if (cb) {
                cb(LOWORD(lParam), HIWORD(lParam));
            }
            break;
        }
        case WM_LBUTTONDOWN: {
            void (*cb)(int, int, int) = (void (*)(int, int, int))find_callback(hwnd, CB_MOUSE_DOWN);
            if (cb) {
                cb(LOWORD(lParam), HIWORD(lParam), 0);
            }
            break;
        }
        case WM_RBUTTONDOWN: {
            void (*cb)(int, int, int) = (void (*)(int, int, int))find_callback(hwnd, CB_MOUSE_DOWN);
            if (cb) {
                cb(LOWORD(lParam), HIWORD(lParam), 1);
            }
            break;
        }
        case WM_LBUTTONUP: {
            void (*cb)(int, int, int) = (void (*)(int, int, int))find_callback(hwnd, CB_MOUSE_UP);
            if (cb) {
                cb(LOWORD(lParam), HIWORD(lParam), 0);
            }
            break;
        }
        case WM_KEYDOWN: {
            void (*cb)(int) = (void (*)(int))find_callback(hwnd, CB_KEY_DOWN);
            if (cb) {
                cb((int)wParam);
            }
            break;
        }
        case WM_KEYUP: {
            void (*cb)(int) = (void (*)(int))find_callback(hwnd, CB_KEY_UP);
            if (cb) {
                cb((int)wParam);
            }
            break;
        }
    }
    return DefWindowProcW(hwnd, msg, wParam, lParam);
}

// ============================================================
// 主窗口过程
// ============================================================

static LRESULT CALLBACK WndProc(HWND hwnd, UINT msg, WPARAM wParam, LPARAM lParam) {
    switch (msg) {
        case WM_COMMAND: {
            HWND ctrl = (HWND)lParam;
            int code = HIWORD(wParam);
            
            // 按钮点击
            if (code == BN_CLICKED) {
                void (*cb)(void) = (void (*)(void))find_callback(ctrl, CB_CLICK);
                if (cb) cb();
            }
            // 文本变化
            else if (code == EN_CHANGE) {
                void (*cb)(void) = (void (*)(void))find_callback(ctrl, CB_CHANGE);
                if (cb) cb();
            }
            // 列表框选择
            else if (code == LBN_SELCHANGE) {
                void (*cb)(void) = (void (*)(void))find_callback(ctrl, CB_SELECT);
                if (cb) cb();
            }
            // 下拉框选择
            else if (code == CBN_SELCHANGE) {
                void (*cb)(void) = (void (*)(void))find_callback(ctrl, CB_CHANGE);
                if (cb) cb();
            }
            break;
        }
        case WM_HSCROLL:
        case WM_VSCROLL: {
            HWND ctrl = (HWND)lParam;
            if (ctrl) {
                void (*cb)(void) = (void (*)(void))find_callback(ctrl, CB_CHANGE);
                if (cb) cb();
            }
            break;
        }
        case WM_TIMER: {
            for (int i = 0; i < g_timer_count; i++) {
                if (g_timers[i].id == wParam && g_timers[i].callback) {
                    g_timers[i].callback();
                }
            }
            break;
        }
        case WM_SIZE: {
            void (*cb)(int, int) = (void (*)(int, int))find_callback(hwnd, CB_RESIZE);
            if (cb) {
                cb(LOWORD(lParam), HIWORD(lParam));
            }
            break;
        }
        case WM_CLOSE: {
            int (*cb)(void) = (int (*)(void))find_callback(hwnd, CB_CLOSE);
            if (cb) {
                if (cb() == 0) {
                    return 0; // 阻止关闭
                }
            }
            DestroyWindow(hwnd);
            return 0;
        }
        case WM_DESTROY: {
            if (hwnd == g_main_window) {
                PostQuitMessage(0);
            }
            return 0;
        }
        case WM_DPICHANGED: {
            // HiDPI: 处理 DPI 变化
            RECT* rect = (RECT*)lParam;
            SetWindowPos(hwnd, NULL, 
                rect->left, rect->top,
                rect->right - rect->left, 
                rect->bottom - rect->top,
                SWP_NOZORDER | SWP_NOACTIVATE);
            
            // 更新字体
            int newDpi = HIWORD(wParam);
            HFONT newFont = create_scaled_font(newDpi);
            if (newFont) {
                if (g_hFont) DeleteObject(g_hFont);
                g_hFont = newFont;
                // 更新所有子控件字体
                EnumChildWindows(hwnd, (WNDENUMPROC)SendMessageW, 
                    (LPARAM)MAKELPARAM(WM_SETFONT, TRUE));
            }
            return 0;
        }
    }
    return DefWindowProcW(hwnd, msg, wParam, lParam);
}

// ============================================================
// API 实现
// ============================================================

GUI_API int gui_init(void) {
    if (g_initialized) return 1;
    
    g_hInstance = GetModuleHandle(NULL);
    
    // 启用 Per-Monitor DPI Awareness v2
    typedef BOOL (WINAPI *SetProcessDpiAwarenessContextProc)(DPI_AWARENESS_CONTEXT);
    HMODULE user32 = GetModuleHandleW(L"user32.dll");
    if (user32) {
        SetProcessDpiAwarenessContextProc setDpiContext = 
            (SetProcessDpiAwarenessContextProc)GetProcAddress(user32, "SetProcessDpiAwarenessContext");
        if (setDpiContext) {
            setDpiContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
    }
    
    // 初始化 Common Controls
    INITCOMMONCONTROLSEX icc = {0};
    icc.dwSize = sizeof(icc);
    icc.dwICC = ICC_WIN95_CLASSES | ICC_BAR_CLASSES | ICC_PROGRESS_CLASS;
    InitCommonControlsEx(&icc);
    
    // 创建默认字体
    g_hFont = create_scaled_font(96);
    if (!g_hFont) {
        g_hFont = (HFONT)GetStockObject(DEFAULT_GUI_FONT);
    }
    
    // 注册主窗口类
    WNDCLASSW wc = {0};
    wc.lpfnWndProc = WndProc;
    wc.hInstance = g_hInstance;
    wc.lpszClassName = L"BolideWindow";
    wc.hbrBackground = (HBRUSH)(COLOR_WINDOW + 1);
    wc.hCursor = LoadCursor(NULL, IDC_ARROW);
    wc.style = CS_HREDRAW | CS_VREDRAW;
    RegisterClassW(&wc);
    
    // 注册画布窗口类
    WNDCLASSW canvasClass = {0};
    canvasClass.lpfnWndProc = CanvasWndProc;
    canvasClass.hInstance = g_hInstance;
    canvasClass.lpszClassName = L"BolideCanvas";
    canvasClass.hbrBackground = (HBRUSH)(COLOR_WINDOW + 1);
    canvasClass.hCursor = LoadCursor(NULL, IDC_CROSS);
    canvasClass.style = CS_HREDRAW | CS_VREDRAW;
    RegisterClassW(&canvasClass);
    
    g_initialized = 1;
    return 1;
}

GUI_API int gui_get_dpi(void* hwnd) {
    typedef UINT (WINAPI *GetDpiForWindowProc)(HWND);
    static GetDpiForWindowProc getDpiForWindow = NULL;
    static int tried = 0;
    
    if (!tried) {
        HMODULE user32 = GetModuleHandleW(L"user32.dll");
        if (user32) {
            getDpiForWindow = (GetDpiForWindowProc)GetProcAddress(user32, "GetDpiForWindow");
        }
        tried = 1;
    }
    
    if (getDpiForWindow && hwnd) {
        return getDpiForWindow((HWND)hwnd);
    }
    return 96;
}

GUI_API int gui_scale(int value, void* hwnd) {
    int dpi = gui_get_dpi(hwnd);
    return MulDiv(value, dpi, 96);
}

GUI_API void gui_run(void) {
    MSG msg;
    while (GetMessage(&msg, NULL, 0, 0)) {
        TranslateMessage(&msg);
        DispatchMessage(&msg);
    }
}

GUI_API void gui_quit(void) {
    PostQuitMessage(0);
}

// ============================================================
// 窗口
// ============================================================

GUI_API void* gui_window(const char* title, int width, int height) {
    wchar_t* wtitle = utf8_to_utf16(title);
    
    // 获取系统 DPI 进行缩放
    HDC hdc = GetDC(NULL);
    int dpi = GetDeviceCaps(hdc, LOGPIXELSX);
    ReleaseDC(NULL, hdc);
    
    int scaledWidth = MulDiv(width, dpi, 96);
    int scaledHeight = MulDiv(height, dpi, 96);
    
    HWND hwnd = CreateWindowExW(
        0,
        L"BolideWindow", wtitle,
        WS_OVERLAPPEDWINDOW,
        CW_USEDEFAULT, CW_USEDEFAULT,
        scaledWidth, scaledHeight,
        NULL, NULL, g_hInstance, NULL
    );
    
    free(wtitle);
    
    if (g_main_window == NULL) {
        g_main_window = hwnd;
    }
    
    // 更新字体为当前 DPI
    if (g_hFont) DeleteObject(g_hFont);
    g_hFont = create_scaled_font(dpi);
    
    ShowWindow(hwnd, SW_SHOW);
    UpdateWindow(hwnd);
    
    return hwnd;
}

GUI_API void gui_close(void* hwnd) {
    DestroyWindow((HWND)hwnd);
}

GUI_API void gui_set_title(void* hwnd, const char* title) {
    wchar_t* wtitle = utf8_to_utf16(title);
    SetWindowTextW((HWND)hwnd, wtitle);
    free(wtitle);
}

GUI_API const char* gui_get_title(void* hwnd) {
    GetWindowTextW((HWND)hwnd, g_wtext_buffer, 4096);
    WideCharToMultiByte(CP_UTF8, 0, g_wtext_buffer, -1, g_text_buffer, sizeof(g_text_buffer), NULL, NULL);
    return g_text_buffer;
}

GUI_API void gui_set_position(void* hwnd, int x, int y) {
    SetWindowPos((HWND)hwnd, NULL, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER);
}

GUI_API void gui_set_size(void* hwnd, int width, int height) {
    int dpi = gui_get_dpi(hwnd);
    int scaledWidth = MulDiv(width, dpi, 96);
    int scaledHeight = MulDiv(height, dpi, 96);
    SetWindowPos((HWND)hwnd, NULL, 0, 0, scaledWidth, scaledHeight, SWP_NOMOVE | SWP_NOZORDER);
}

GUI_API void gui_show(void* hwnd, int show) {
    ShowWindow((HWND)hwnd, show ? SW_SHOW : SW_HIDE);
}

GUI_API void gui_center(void* hwnd) {
    RECT rc;
    GetWindowRect((HWND)hwnd, &rc);
    int width = rc.right - rc.left;
    int height = rc.bottom - rc.top;
    int x = (GetSystemMetrics(SM_CXSCREEN) - width) / 2;
    int y = (GetSystemMetrics(SM_CYSCREEN) - height) / 2;
    SetWindowPos((HWND)hwnd, NULL, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER);
}

// ============================================================
// 基础控件
// ============================================================

static HWND create_control(const wchar_t* className, const wchar_t* text, DWORD style,
                           HWND parent, int x, int y, int w, int h) {
    int dpi = gui_get_dpi(parent);
    int sx = MulDiv(x, dpi, 96);
    int sy = MulDiv(y, dpi, 96);
    int sw = MulDiv(w, dpi, 96);
    int sh = MulDiv(h, dpi, 96);
    
    HWND hwnd = CreateWindowExW(
        0, className, text,
        WS_CHILD | WS_VISIBLE | style,
        sx, sy, sw, sh,
        parent, (HMENU)(intptr_t)(g_control_id++),
        g_hInstance, NULL
    );
    
    if (hwnd && g_hFont) {
        SendMessage(hwnd, WM_SETFONT, (WPARAM)g_hFont, TRUE);
    }
    
    return hwnd;
}

GUI_API void* gui_button(void* parent, const char* text, int x, int y, int w, int h) {
    wchar_t* wtext = utf8_to_utf16(text);
    HWND btn = create_control(L"BUTTON", wtext, BS_PUSHBUTTON, (HWND)parent, x, y, w, h);
    free(wtext);
    return btn;
}

GUI_API void* gui_label(void* parent, const char* text, int x, int y, int w, int h) {
    wchar_t* wtext = utf8_to_utf16(text);
    HWND label = create_control(L"STATIC", wtext, SS_LEFT, (HWND)parent, x, y, w, h);
    free(wtext);
    return label;
}

GUI_API void* gui_textbox(void* parent, int x, int y, int w, int h) {
    return create_control(L"EDIT", L"", 
        ES_AUTOHSCROLL | WS_BORDER, (HWND)parent, x, y, w, h);
}

GUI_API void* gui_textarea(void* parent, int x, int y, int w, int h) {
    return create_control(L"EDIT", L"", 
        ES_MULTILINE | ES_AUTOVSCROLL | ES_WANTRETURN | WS_BORDER | WS_VSCROLL,
        (HWND)parent, x, y, w, h);
}

GUI_API void* gui_password(void* parent, int x, int y, int w, int h) {
    return create_control(L"EDIT", L"", 
        ES_PASSWORD | ES_AUTOHSCROLL | WS_BORDER, (HWND)parent, x, y, w, h);
}

// ============================================================
// 选择控件
// ============================================================

GUI_API void* gui_checkbox(void* parent, const char* text, int x, int y, int w, int h) {
    wchar_t* wtext = utf8_to_utf16(text);
    HWND chk = create_control(L"BUTTON", wtext, BS_AUTOCHECKBOX, (HWND)parent, x, y, w, h);
    free(wtext);
    return chk;
}

GUI_API int gui_checkbox_get(void* handle) {
    return (int)SendMessage((HWND)handle, BM_GETCHECK, 0, 0);
}

GUI_API void gui_checkbox_set(void* handle, int checked) {
    SendMessage((HWND)handle, BM_SETCHECK, checked ? BST_CHECKED : BST_UNCHECKED, 0);
}

GUI_API void* gui_radio(void* parent, const char* text, int x, int y, int w, int h, int group_start) {
    wchar_t* wtext = utf8_to_utf16(text);
    DWORD style = BS_AUTORADIOBUTTON;
    if (group_start) style |= WS_GROUP;
    HWND radio = create_control(L"BUTTON", wtext, style, (HWND)parent, x, y, w, h);
    free(wtext);
    return radio;
}

GUI_API int gui_radio_get(void* handle) {
    return (int)SendMessage((HWND)handle, BM_GETCHECK, 0, 0);
}

GUI_API void gui_radio_set(void* handle, int checked) {
    SendMessage((HWND)handle, BM_SETCHECK, checked ? BST_CHECKED : BST_UNCHECKED, 0);
}

// ============================================================
// 滑块与进度条
// ============================================================

GUI_API void* gui_slider(void* parent, int min_val, int max_val, int x, int y, int w, int h) {
    HWND slider = create_control(TRACKBAR_CLASSW, L"", TBS_HORZ | TBS_AUTOTICKS,
        (HWND)parent, x, y, w, h);
    SendMessage(slider, TBM_SETRANGE, TRUE, MAKELPARAM(min_val, max_val));
    return slider;
}

GUI_API int gui_slider_get(void* handle) {
    return (int)SendMessage((HWND)handle, TBM_GETPOS, 0, 0);
}

GUI_API void gui_slider_set(void* handle, int value) {
    SendMessage((HWND)handle, TBM_SETPOS, TRUE, value);
}

GUI_API void* gui_progress(void* parent, int x, int y, int w, int h) {
    HWND prog = create_control(PROGRESS_CLASSW, L"", PBS_SMOOTH,
        (HWND)parent, x, y, w, h);
    SendMessage(prog, PBM_SETRANGE, 0, MAKELPARAM(0, 100));
    return prog;
}

GUI_API void gui_progress_set(void* handle, int value) {
    SendMessage((HWND)handle, PBM_SETPOS, value, 0);
}

GUI_API void gui_progress_set_range(void* handle, int min_val, int max_val) {
    SendMessage((HWND)handle, PBM_SETRANGE, 0, MAKELPARAM(min_val, max_val));
}

// ============================================================
// 列表控件
// ============================================================

GUI_API void* gui_listbox(void* parent, int x, int y, int w, int h) {
    return create_control(L"LISTBOX", L"",
        LBS_NOTIFY | LBS_HASSTRINGS | WS_VSCROLL | WS_BORDER,
        (HWND)parent, x, y, w, h);
}

GUI_API void gui_listbox_add(void* handle, const char* text) {
    wchar_t* wtext = utf8_to_utf16(text);
    SendMessageW((HWND)handle, LB_ADDSTRING, 0, (LPARAM)wtext);
    free(wtext);
}

GUI_API void gui_listbox_insert(void* handle, int index, const char* text) {
    wchar_t* wtext = utf8_to_utf16(text);
    SendMessageW((HWND)handle, LB_INSERTSTRING, index, (LPARAM)wtext);
    free(wtext);
}

GUI_API void gui_listbox_remove(void* handle, int index) {
    SendMessage((HWND)handle, LB_DELETESTRING, index, 0);
}

GUI_API void gui_listbox_clear(void* handle) {
    SendMessage((HWND)handle, LB_RESETCONTENT, 0, 0);
}

GUI_API int gui_listbox_get_selected(void* handle) {
    return (int)SendMessage((HWND)handle, LB_GETCURSEL, 0, 0);
}

GUI_API void gui_listbox_set_selected(void* handle, int index) {
    SendMessage((HWND)handle, LB_SETCURSEL, index, 0);
}

GUI_API int gui_listbox_count(void* handle) {
    return (int)SendMessage((HWND)handle, LB_GETCOUNT, 0, 0);
}

GUI_API const char* gui_listbox_get_text(void* handle, int index) {
    // 检查 index 是否有效
    int count = (int)SendMessage((HWND)handle, LB_GETCOUNT, 0, 0);
    if (index < 0 || index >= count) {
        g_text_buffer[0] = '\0';
        return g_text_buffer;
    }

    // 获取文本长度（不包含 null terminator）
    int len = (int)SendMessageW((HWND)handle, LB_GETTEXTLEN, index, 0);
    if (len == LB_ERR || len < 0) {
        g_text_buffer[0] = '\0';
        return g_text_buffer;
    }

    // 如果长度为 0，返回空字符串
    if (len == 0) {
        g_text_buffer[0] = '\0';
        return g_text_buffer;
    }

    // 确保缓冲区足够大（需要 len + 1 个字符来存储 null terminator）
    if (len >= 4095) {
        g_text_buffer[0] = '\0';
        return g_text_buffer;
    }

    // 分配临时缓冲区
    wchar_t* temp_buffer = (wchar_t*)malloc((len + 1) * sizeof(wchar_t));
    if (!temp_buffer) {
        g_text_buffer[0] = '\0';
        return g_text_buffer;
    }

    // 获取文本
    int result = (int)SendMessageW((HWND)handle, LB_GETTEXT, index, (LPARAM)temp_buffer);
    if (result == LB_ERR || result != len) {
        free(temp_buffer);
        g_text_buffer[0] = '\0';
        return g_text_buffer;
    }

    // 确保以 null 结尾
    temp_buffer[len] = L'\0';

    // 转换为 UTF-8
    int utf8_len = WideCharToMultiByte(CP_UTF8, 0, temp_buffer, -1, NULL, 0, NULL, NULL);
    if (utf8_len > 0 && utf8_len <= sizeof(g_text_buffer)) {
        WideCharToMultiByte(CP_UTF8, 0, temp_buffer, -1, g_text_buffer, sizeof(g_text_buffer), NULL, NULL);
    } else {
        g_text_buffer[0] = '\0';
    }

    free(temp_buffer);
    return g_text_buffer;
}

GUI_API void* gui_combobox(void* parent, int x, int y, int w, int h) {
    // ComboBox 高度包括下拉列表
    return create_control(L"COMBOBOX", L"",
        CBS_DROPDOWNLIST | CBS_HASSTRINGS | WS_VSCROLL,
        (HWND)parent, x, y, w, h * 6);
}

GUI_API void gui_combobox_add(void* handle, const char* text) {
    wchar_t* wtext = utf8_to_utf16(text);
    SendMessageW((HWND)handle, CB_ADDSTRING, 0, (LPARAM)wtext);
    free(wtext);
}

GUI_API void gui_combobox_clear(void* handle) {
    SendMessage((HWND)handle, CB_RESETCONTENT, 0, 0);
}

GUI_API int gui_combobox_get_selected(void* handle) {
    return (int)SendMessage((HWND)handle, CB_GETCURSEL, 0, 0);
}

GUI_API void gui_combobox_set_selected(void* handle, int index) {
    SendMessage((HWND)handle, CB_SETCURSEL, index, 0);
}

GUI_API int gui_combobox_count(void* handle) {
    return (int)SendMessage((HWND)handle, CB_GETCOUNT, 0, 0);
}

// ============================================================
// 通用控件操作
// ============================================================

GUI_API const char* gui_get_text(void* handle) {
    GetWindowTextW((HWND)handle, g_wtext_buffer, 4096);
    WideCharToMultiByte(CP_UTF8, 0, g_wtext_buffer, -1, g_text_buffer, sizeof(g_text_buffer), NULL, NULL);
    return g_text_buffer;
}

GUI_API void gui_set_text(void* handle, const char* text) {
    wchar_t* wtext = utf8_to_utf16(text);
    SetWindowTextW((HWND)handle, wtext);
    free(wtext);
}

GUI_API void gui_enable(void* handle, int enabled) {
    EnableWindow((HWND)handle, enabled);
}

GUI_API void gui_visible(void* handle, int visible) {
    ShowWindow((HWND)handle, visible ? SW_SHOW : SW_HIDE);
}

GUI_API void gui_focus(void* handle) {
    SetFocus((HWND)handle);
}

// ============================================================
// 画布
// ============================================================

GUI_API void* gui_canvas(void* parent, int x, int y, int w, int h) {
    if (g_canvas_count >= MAX_CANVAS) return NULL;
    
    int dpi = gui_get_dpi(parent);
    int sx = MulDiv(x, dpi, 96);
    int sy = MulDiv(y, dpi, 96);
    int sw = MulDiv(w, dpi, 96);
    int sh = MulDiv(h, dpi, 96);
    
    HWND hwnd = CreateWindowExW(
        0, L"BolideCanvas", L"",
        WS_CHILD | WS_VISIBLE | WS_BORDER,
        sx, sy, sw, sh,
        (HWND)parent, (HMENU)(intptr_t)(g_control_id++),
        g_hInstance, NULL
    );
    
    if (!hwnd) return NULL;
    
    // 创建内存 DC 和位图
    HDC screenDC = GetDC(hwnd);
    HDC memDC = CreateCompatibleDC(screenDC);
    HBITMAP memBitmap = CreateCompatibleBitmap(screenDC, sw, sh);
    HBITMAP oldBitmap = (HBITMAP)SelectObject(memDC, memBitmap);
    ReleaseDC(hwnd, screenDC);
    
    // 初始化为白色
    RECT rc = {0, 0, sw, sh};
    HBRUSH whiteBrush = CreateSolidBrush(RGB(255, 255, 255));
    FillRect(memDC, &rc, whiteBrush);
    DeleteObject(whiteBrush);
    
    // 保存画布数据
    CanvasData* cd = &g_canvas[g_canvas_count++];
    cd->hwnd = hwnd;
    cd->memDC = memDC;
    cd->memBitmap = memBitmap;
    cd->oldBitmap = oldBitmap;
    cd->width = sw;
    cd->height = sh;
    
    return hwnd;
}

static COLORREF rgb_from_int(int color) {
    // 从 0xRRGGBB 转换为 COLORREF (0x00BBGGRR)
    int r = (color >> 16) & 0xFF;
    int g = (color >> 8) & 0xFF;
    int b = color & 0xFF;
    return RGB(r, g, b);
}

GUI_API void gui_canvas_rect(void* handle, int x, int y, int w, int h, int color) {
    CanvasData* cd = find_canvas((HWND)handle);
    if (!cd) return;
    
    HPEN pen = CreatePen(PS_SOLID, 1, rgb_from_int(color));
    HPEN oldPen = (HPEN)SelectObject(cd->memDC, pen);
    HBRUSH oldBrush = (HBRUSH)SelectObject(cd->memDC, GetStockObject(NULL_BRUSH));
    
    Rectangle(cd->memDC, x, y, x + w, y + h);
    
    SelectObject(cd->memDC, oldPen);
    SelectObject(cd->memDC, oldBrush);
    DeleteObject(pen);
}

GUI_API void gui_canvas_fill_rect(void* handle, int x, int y, int w, int h, int color) {
    CanvasData* cd = find_canvas((HWND)handle);
    if (!cd) return;
    
    RECT rc = {x, y, x + w, y + h};
    HBRUSH brush = CreateSolidBrush(rgb_from_int(color));
    FillRect(cd->memDC, &rc, brush);
    DeleteObject(brush);
}

GUI_API void gui_canvas_line(void* handle, int x1, int y1, int x2, int y2, int color) {
    CanvasData* cd = find_canvas((HWND)handle);
    if (!cd) return;
    
    HPEN pen = CreatePen(PS_SOLID, 1, rgb_from_int(color));
    HPEN oldPen = (HPEN)SelectObject(cd->memDC, pen);
    
    MoveToEx(cd->memDC, x1, y1, NULL);
    LineTo(cd->memDC, x2, y2);
    
    SelectObject(cd->memDC, oldPen);
    DeleteObject(pen);
}

GUI_API void gui_canvas_circle(void* handle, int cx, int cy, int r, int color) {
    CanvasData* cd = find_canvas((HWND)handle);
    if (!cd) return;
    
    HPEN pen = CreatePen(PS_SOLID, 1, rgb_from_int(color));
    HPEN oldPen = (HPEN)SelectObject(cd->memDC, pen);
    HBRUSH oldBrush = (HBRUSH)SelectObject(cd->memDC, GetStockObject(NULL_BRUSH));
    
    Ellipse(cd->memDC, cx - r, cy - r, cx + r, cy + r);
    
    SelectObject(cd->memDC, oldPen);
    SelectObject(cd->memDC, oldBrush);
    DeleteObject(pen);
}

GUI_API void gui_canvas_fill_circle(void* handle, int cx, int cy, int r, int color) {
    CanvasData* cd = find_canvas((HWND)handle);
    if (!cd) return;
    
    HBRUSH brush = CreateSolidBrush(rgb_from_int(color));
    HBRUSH oldBrush = (HBRUSH)SelectObject(cd->memDC, brush);
    HPEN oldPen = (HPEN)SelectObject(cd->memDC, GetStockObject(NULL_PEN));
    
    Ellipse(cd->memDC, cx - r, cy - r, cx + r, cy + r);
    
    SelectObject(cd->memDC, oldPen);
    SelectObject(cd->memDC, oldBrush);
    DeleteObject(brush);
}

GUI_API void gui_canvas_text(void* handle, const char* text, int x, int y, int color) {
    CanvasData* cd = find_canvas((HWND)handle);
    if (!cd) return;
    
    wchar_t* wtext = utf8_to_utf16(text);
    SetTextColor(cd->memDC, rgb_from_int(color));
    SetBkMode(cd->memDC, TRANSPARENT);
    
    if (g_hFont) SelectObject(cd->memDC, g_hFont);
    TextOutW(cd->memDC, x, y, wtext, (int)wcslen(wtext));
    
    free(wtext);
}

GUI_API void gui_canvas_clear(void* handle, int color) {
    CanvasData* cd = find_canvas((HWND)handle);
    if (!cd) return;
    
    RECT rc = {0, 0, cd->width, cd->height};
    HBRUSH brush = CreateSolidBrush(rgb_from_int(color));
    FillRect(cd->memDC, &rc, brush);
    DeleteObject(brush);
}

GUI_API void gui_canvas_refresh(void* handle) {
    InvalidateRect((HWND)handle, NULL, FALSE);
    UpdateWindow((HWND)handle);
}

// ============================================================
// 对话框
// ============================================================

GUI_API int gui_msgbox(void* parent, const char* title, const char* message, int flags) {
    wchar_t* wtitle = utf8_to_utf16(title);
    wchar_t* wmessage = utf8_to_utf16(message);
    int result = MessageBoxW((HWND)parent, wmessage, wtitle, flags);
    free(wtitle);
    free(wmessage);
    return result;
}

static char g_file_buffer[MAX_PATH * 2];

GUI_API const char* gui_open_file(void* parent, const char* filter, const char* title) {
    wchar_t wfilename[MAX_PATH] = {0};
    wchar_t* wfilter = utf8_to_utf16(filter ? filter : "All Files\0*.*\0");
    wchar_t* wtitle = utf8_to_utf16(title ? title : "Open File");
    
    OPENFILENAMEW ofn = {0};
    ofn.lStructSize = sizeof(ofn);
    ofn.hwndOwner = (HWND)parent;
    ofn.lpstrFilter = wfilter;
    ofn.lpstrFile = wfilename;
    ofn.nMaxFile = MAX_PATH;
    ofn.lpstrTitle = wtitle;
    ofn.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST;
    
    if (GetOpenFileNameW(&ofn)) {
        WideCharToMultiByte(CP_UTF8, 0, wfilename, -1, g_file_buffer, sizeof(g_file_buffer), NULL, NULL);
    } else {
        g_file_buffer[0] = '\0';
    }
    
    free(wfilter);
    free(wtitle);
    return g_file_buffer;
}

GUI_API const char* gui_save_file(void* parent, const char* filter, const char* title) {
    wchar_t wfilename[MAX_PATH] = {0};
    wchar_t* wfilter = utf8_to_utf16(filter ? filter : "All Files\0*.*\0");
    wchar_t* wtitle = utf8_to_utf16(title ? title : "Save File");
    
    OPENFILENAMEW ofn = {0};
    ofn.lStructSize = sizeof(ofn);
    ofn.hwndOwner = (HWND)parent;
    ofn.lpstrFilter = wfilter;
    ofn.lpstrFile = wfilename;
    ofn.nMaxFile = MAX_PATH;
    ofn.lpstrTitle = wtitle;
    ofn.Flags = OFN_OVERWRITEPROMPT;
    
    if (GetSaveFileNameW(&ofn)) {
        WideCharToMultiByte(CP_UTF8, 0, wfilename, -1, g_file_buffer, sizeof(g_file_buffer), NULL, NULL);
    } else {
        g_file_buffer[0] = '\0';
    }
    
    free(wfilter);
    free(wtitle);
    return g_file_buffer;
}

static int CALLBACK BrowseFolderCallback(HWND hwnd, UINT uMsg, LPARAM lParam, LPARAM lpData) {
    (void)lParam;
    if (uMsg == BFFM_INITIALIZED && lpData) {
        SendMessageW(hwnd, BFFM_SETSELECTIONW, TRUE, lpData);
    }
    return 0;
}

GUI_API const char* gui_select_folder(void* parent, const char* title) {
    wchar_t wpath[MAX_PATH] = {0};
    wchar_t* wtitle = utf8_to_utf16(title ? title : "Select Folder");
    
    BROWSEINFOW bi = {0};
    bi.hwndOwner = (HWND)parent;
    bi.lpszTitle = wtitle;
    bi.ulFlags = BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE;
    bi.lpfn = BrowseFolderCallback;
    
    LPITEMIDLIST pidl = SHBrowseForFolderW(&bi);
    if (pidl && SHGetPathFromIDListW(pidl, wpath)) {
        WideCharToMultiByte(CP_UTF8, 0, wpath, -1, g_file_buffer, sizeof(g_file_buffer), NULL, NULL);
        CoTaskMemFree(pidl);
    } else {
        g_file_buffer[0] = '\0';
    }
    
    free(wtitle);
    return g_file_buffer;
}

GUI_API int gui_color_picker(void* parent, int initial_color) {
    static COLORREF customColors[16] = {0};
    
    CHOOSECOLORW cc = {0};
    cc.lStructSize = sizeof(cc);
    cc.hwndOwner = (HWND)parent;
    cc.rgbResult = rgb_from_int(initial_color);
    cc.lpCustColors = customColors;
    cc.Flags = CC_FULLOPEN | CC_RGBINIT;
    
    if (ChooseColorW(&cc)) {
        // 转换回 0xRRGGBB 格式
        int r = GetRValue(cc.rgbResult);
        int g = GetGValue(cc.rgbResult);
        int b = GetBValue(cc.rgbResult);
        return (r << 16) | (g << 8) | b;
    }
    return initial_color;
}

// ============================================================
// 菜单
// ============================================================

static int g_menu_id = 10000;

typedef struct {
    int id;
    void (*callback)(void);
} MenuCallback;

#define MAX_MENU_CALLBACKS 128
static MenuCallback g_menu_callbacks[MAX_MENU_CALLBACKS];
static int g_menu_callback_count = 0;

GUI_API void* gui_menubar(void* window) {
    HMENU menubar = CreateMenu();
    SetMenu((HWND)window, menubar);
    return menubar;
}

GUI_API void* gui_menu(void* menubar, const char* text) {
    wchar_t* wtext = utf8_to_utf16(text);
    HMENU menu = CreatePopupMenu();
    AppendMenuW((HMENU)menubar, MF_POPUP, (UINT_PTR)menu, wtext);
    DrawMenuBar(GetParent((HWND)menubar));
    free(wtext);
    return menu;
}

GUI_API void* gui_menu_item(void* menu, const char* text, void (*callback)(void)) {
    wchar_t* wtext = utf8_to_utf16(text);
    int id = g_menu_id++;
    AppendMenuW((HMENU)menu, MF_STRING, id, wtext);
    
    if (g_menu_callback_count < MAX_MENU_CALLBACKS) {
        g_menu_callbacks[g_menu_callback_count].id = id;
        g_menu_callbacks[g_menu_callback_count].callback = callback;
        g_menu_callback_count++;
    }
    
    free(wtext);
    return (void*)(intptr_t)id;
}

GUI_API void gui_menu_separator(void* menu) {
    AppendMenuW((HMENU)menu, MF_SEPARATOR, 0, NULL);
}

// ============================================================
// 定时器
// ============================================================

GUI_API int gui_set_timer(int interval_ms, void (*callback)(void)) {
    if (g_main_window == NULL || g_timer_count >= MAX_TIMERS) {
        return 0;
    }
    UINT_PTR id = g_timer_id_counter++;
    SetTimer(g_main_window, id, interval_ms, NULL);
    g_timers[g_timer_count].id = id;
    g_timers[g_timer_count].callback = callback;
    g_timer_count++;
    return (int)id;
}

GUI_API void gui_kill_timer(int timer_id) {
    if (g_main_window == NULL) return;
    KillTimer(g_main_window, (UINT_PTR)timer_id);
    for (int i = 0; i < g_timer_count; i++) {
        if (g_timers[i].id == (UINT_PTR)timer_id) {
            for (int j = i; j < g_timer_count - 1; j++) {
                g_timers[j] = g_timers[j + 1];
            }
            g_timer_count--;
            break;
        }
    }
}

// ============================================================
// 事件回调
// ============================================================

GUI_API void gui_on_click(void* handle, void (*callback)(void)) {
    register_callback((HWND)handle, CB_CLICK, (void*)callback);
}

GUI_API void gui_on_change(void* handle, void (*callback)(void)) {
    register_callback((HWND)handle, CB_CHANGE, (void*)callback);
}

GUI_API void gui_on_select(void* handle, void (*callback)(void)) {
    register_callback((HWND)handle, CB_SELECT, (void*)callback);
}

GUI_API void gui_on_paint(void* handle, void (*callback)(void)) {
    register_callback((HWND)handle, CB_PAINT, (void*)callback);
}

GUI_API void gui_on_mouse_move(void* handle, void (*callback)(int, int)) {
    register_callback((HWND)handle, CB_MOUSE_MOVE, (void*)callback);
}

GUI_API void gui_on_mouse_down(void* handle, void (*callback)(int, int, int)) {
    register_callback((HWND)handle, CB_MOUSE_DOWN, (void*)callback);
}

GUI_API void gui_on_mouse_up(void* handle, void (*callback)(int, int, int)) {
    register_callback((HWND)handle, CB_MOUSE_UP, (void*)callback);
}

GUI_API void gui_on_key_down(void* handle, void (*callback)(int)) {
    register_callback((HWND)handle, CB_KEY_DOWN, (void*)callback);
}

GUI_API void gui_on_key_up(void* handle, void (*callback)(int)) {
    register_callback((HWND)handle, CB_KEY_UP, (void*)callback);
}

GUI_API void gui_on_close(void* handle, int (*callback)(void)) {
    register_callback((HWND)handle, CB_CLOSE, (void*)callback);
}

GUI_API void gui_on_resize(void* handle, void (*callback)(int, int)) {
    register_callback((HWND)handle, CB_RESIZE, (void*)callback);
}

#endif // _WIN32
