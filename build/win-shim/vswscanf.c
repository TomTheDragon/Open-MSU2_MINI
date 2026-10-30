/* Zig's MinGW C library has no vswscanf, but wxWidgets needs it.
   Microsoft's UCRT implements it as a wrapper around __stdio_common_vswscanf;
   this does the same. Types are spelled out to avoid clashing with header inlines. */
#include <stdarg.h>
#include <stddef.h>
typedef unsigned short wchar16;
int __cdecl __stdio_common_vswscanf(unsigned long long options, const wchar16 *buffer, size_t count,
                                    const wchar16 *format, void *locale, va_list args);
int __cdecl vswscanf(const wchar16 *buffer, const wchar16 *format, va_list args) {
    return __stdio_common_vswscanf(0, buffer, (size_t)-1, format, NULL, args);
}
