// Bolide GUI Library - Win32 Implementation
#define BOLIDE_GUI_EXPORTS
#define UNICODE
#define _UNICODE
#include "gui.h"

#ifdef _WIN32
#include <windows.h>
#include <stdlib.h>
#include <string.h>

static HINSTANCE g_hInstance = NULL;
static int g_control_id = 1000;
static HFONT g_hFont = NULL;

// UTF-8 转 UTF-16
static wchar_t* utf8_to_utf16(const char* utf8) {
    if (!utf8) return NULL;
    int len = MultiByteToWideChar(CP_UTF8, 0, utf8, -1, NULL, 0);
    wchar_t* utf16 = (wchar_t*)malloc(len * sizeof(wchar_t));
    MultiByteToWideChar(CP_UTF8, 0, utf8, -1, utf16, len);
    return utf16;
}

// 回调函数存储
#define MAX_CALLBACKS 100
static struct {
    HWND hwnd;
    void (*callback)(void);
} g_callbacks[MAX_CALLBACKS];
static int g_callback_count = 0;

// 定时器回调存储
#define MAX_TIMERS 10
static struct {
    UINT_PTR id;
    void (*callback)(void);
} g_timers[MAX_TIMERS];
static int g_timer_count = 0;
static HWND g_main_window = NULL;

// 窗口过程
LRESULT CALLBACK WndProc(HWND hwnd, UINT msg, WPARAM wParam, LPARAM lParam) {
    switch (msg) {
        case WM_COMMAND:
            if (HIWORD(wParam) == BN_CLICKED) {
                HWND btn = (HWND)lParam;
                for (int i = 0; i < g_callback_count; i++) {
                    if (g_callbacks[i].hwnd == btn && g_callbacks[i].callback) {
                        g_callbacks[i].callback();
                    }
                }
            }
            break;
        case WM_TIMER:
            for (int i = 0; i < g_timer_count; i++) {
                if (g_timers[i].id == wParam && g_timers[i].callback) {
                    g_timers[i].callback();
                }
            }
            break;
        case WM_DESTROY:
            PostQuitMessage(0);
            return 0;
    }
    return DefWindowProc(hwnd, msg, wParam, lParam);
}

GUI_API int gui_init(void) {
    g_hInstance = GetModuleHandle(NULL);

    // 创建默认字体
    g_hFont = CreateFontW(
        -14, 0, 0, 0, FW_NORMAL, FALSE, FALSE, FALSE,
        DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
        DEFAULT_QUALITY, DEFAULT_PITCH | FF_DONTCARE, L"Segoe UI"
    );
    if (!g_hFont) {
        g_hFont = (HFONT)GetStockObject(DEFAULT_GUI_FONT);
    }

    WNDCLASSW wc = {0};
    wc.lpfnWndProc = WndProc;
    wc.hInstance = g_hInstance;
    wc.lpszClassName = L"BolideWindow";
    wc.hbrBackground = (HBRUSH)(COLOR_WINDOW + 1);
    wc.hCursor = LoadCursor(NULL, IDC_ARROW);

    return RegisterClassW(&wc) != 0;
}

GUI_API void* gui_window(const char* title, int width, int height) {
    wchar_t* wtitle = utf8_to_utf16(title);
    HWND hwnd = CreateWindowW(
        L"BolideWindow", wtitle,
        WS_OVERLAPPEDWINDOW,
        CW_USEDEFAULT, CW_USEDEFAULT,
        width, height,
        NULL, NULL, g_hInstance, NULL
    );
    free(wtitle);
    if (g_main_window == NULL) {
        g_main_window = hwnd;
    }
    ShowWindow(hwnd, SW_SHOW);
    UpdateWindow(hwnd);
    return hwnd;
}

GUI_API void* gui_button(void* parent, const char* text, int x, int y, int w, int h) {
    wchar_t* wtext = utf8_to_utf16(text);
    HWND btn = CreateWindowW(
        L"BUTTON", wtext,
        WS_CHILD | WS_VISIBLE | BS_PUSHBUTTON,
        x, y, w, h,
        (HWND)parent, (HMENU)(intptr_t)(g_control_id++),
        g_hInstance, NULL
    );
    free(wtext);
    if (btn && g_hFont) {
        SendMessage(btn, WM_SETFONT, (WPARAM)g_hFont, TRUE);
    }
    return btn;
}

GUI_API void* gui_label(void* parent, const char* text, int x, int y, int w, int h) {
    wchar_t* wtext = utf8_to_utf16(text);
    HWND label = CreateWindowW(
        L"STATIC", wtext,
        WS_CHILD | WS_VISIBLE,
        x, y, w, h,
        (HWND)parent, (HMENU)(intptr_t)(g_control_id++),
        g_hInstance, NULL
    );
    free(wtext);
    if (label && g_hFont) {
        SendMessage(label, WM_SETFONT, (WPARAM)g_hFont, TRUE);
    }
    return label;
}

GUI_API void* gui_textbox(void* parent, int x, int y, int w, int h) {
    HWND edit = CreateWindowW(
        L"EDIT", L"",
        WS_CHILD | WS_VISIBLE | WS_BORDER | ES_AUTOHSCROLL,
        x, y, w, h,
        (HWND)parent, (HMENU)(intptr_t)(g_control_id++),
        g_hInstance, NULL
    );
    if (edit && g_hFont) {
        SendMessage(edit, WM_SETFONT, (WPARAM)g_hFont, TRUE);
    }
    return edit;
}

static char g_text_buffer[4096];
static wchar_t g_wtext_buffer[2048];

GUI_API const char* gui_get_text(void* handle) {
    GetWindowTextW((HWND)handle, g_wtext_buffer, 2048);
    WideCharToMultiByte(CP_UTF8, 0, g_wtext_buffer, -1, g_text_buffer, sizeof(g_text_buffer), NULL, NULL);
    return g_text_buffer;
}

GUI_API void gui_set_text(void* handle, const char* text) {
    wchar_t* wtext = utf8_to_utf16(text);
    SetWindowTextW((HWND)handle, wtext);
    free(wtext);
}

GUI_API void gui_on_click(void* button, void (*callback)(void)) {
    if (g_callback_count < MAX_CALLBACKS) {
        g_callbacks[g_callback_count].hwnd = (HWND)button;
        g_callbacks[g_callback_count].callback = callback;
        g_callback_count++;
    }
}

GUI_API void gui_run(void) {
    MSG msg;
    while (GetMessage(&msg, NULL, 0, 0)) {
        TranslateMessage(&msg);
        DispatchMessage(&msg);
    }
}

GUI_API void gui_close(void* window) {
    DestroyWindow((HWND)window);
}

static UINT_PTR g_timer_id_counter = 1;

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

#endif // _WIN32
