// reloc_self for Windows: the module that owns the address, with forward slashes. The ANSI API,
// because packages open the result with the narrow CRT.
#include <windows.h>
#ifndef PATH_MAX
#define PATH_MAX MAX_PATH
#endif

__attribute__((unused)) static int reloc_self(const void* address, char* path, size_t size) {
   HMODULE module = NULL;
   DWORD flags =
      GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
   if (!GetModuleHandleExA(flags, (LPCSTR)address, &module)) {
      return 0;
   }
   DWORD length = GetModuleFileNameA(module, path, (DWORD)size);
   if (length == 0 || length >= size) {
      return 0;
   }
   for (char* cursor = path; *cursor; cursor++) {
      if (*cursor == '\\') {
         *cursor = '/';
      }
   }
   return 1;
}
