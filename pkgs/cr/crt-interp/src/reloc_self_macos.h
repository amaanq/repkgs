// reloc_self for macOS: dladdr names the image holding the address, the main executable included.
// Declared here under its symbol name so <dlfcn.h> and its feature macros stay out of the package.
#include <string.h>

struct reloc_image {
   const char* file;
   void* base;
   const char* symbol;
   void* symbol_address;
};

int reloc_dladdr(const void* address, struct reloc_image* image) __asm__("_dladdr");

__attribute__((unused)) static int reloc_self(const void* address, char* path, size_t size) {
   struct reloc_image image;
   if (!reloc_dladdr(address, &image) || !image.file || strlen(image.file) >= size) {
      return 0;
   }
   strcpy(path, image.file);
   return 1;
}
