// reloc_self for Linux: the file behind the mapping that holds the address. ISO C only, since the
// package's own feature-test macros are in effect here.
#include <stdio.h>
#include <string.h>

__attribute__((unused)) static int reloc_self(const void* address, char* path, size_t size) {
   FILE* maps = fopen("/proc/self/maps", "re");
   if (!maps) {
      return 0;
   }
   unsigned long wanted = (unsigned long)address;
   unsigned long start = 0;
   unsigned long end = 0;
   char line[PATH_MAX + 128];
   int found = 0;
   while (!found && fgets(line, sizeof line, maps)) {
      char* file = strchr(line, '/');
      if (!file || sscanf(line, "%lx-%lx", &start, &end) != 2 || wanted < start || wanted >= end) {
         continue;
      }
      file[strcspn(file, "\n")] = 0;
      if (strlen(file) < size) {
         strcpy(path, file);
         found = 1;
      }
   }
   fclose(maps);
   return found;
}
