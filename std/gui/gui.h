// Bolide GUI Library Header
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

#endif // BOLIDE_GUI_H
