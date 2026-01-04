// Bolide GUI Library - Win32 API Wrapper
#ifndef BOLIDE_GUI_H
#define BOLIDE_GUI_H

#ifdef _WIN32
  #ifdef BOLIDE_GUI_EXPORTS
    #define GUI_API __declspec(dllexport)
  #else
    #define GUI_API __declspec(dllimport)
  #endif
#else
  #define GUI_API
#endif

#include <stdint.h>

// 初始化 GUI 系统
GUI_API int gui_init(void);

// 创建窗口
GUI_API void* gui_window(const char* title, int width, int height);

// 创建按钮
GUI_API void* gui_button(void* parent, const char* text, int x, int y, int w, int h);

// 创建标签
GUI_API void* gui_label(void* parent, const char* text, int x, int y, int w, int h);

// 创建文本框
GUI_API void* gui_textbox(void* parent, int x, int y, int w, int h);

// 获取文本框内容
GUI_API const char* gui_get_text(void* handle);

// 设置文本
GUI_API void gui_set_text(void* handle, const char* text);

// 设置按钮点击回调
GUI_API void gui_on_click(void* button, void (*callback)(void));

// 运行消息循环
GUI_API void gui_run(void);

// 关闭窗口
GUI_API void gui_close(void* window);

// 设置定时器
GUI_API int gui_set_timer(int interval_ms, void (*callback)(void));

// 取消定时器
GUI_API void gui_kill_timer(int timer_id);

#endif
