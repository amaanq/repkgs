// RELOC("../share/foo") is that path taken relative to the directory of the executable or library
// it was compiled into, resolved when first asked for. Patches use it where a program would bake
// in its install prefix, so a package keeps working wherever its closure is copied. cc installs
// the reloc_self.h for its target next to this header.
#ifndef RELOC_H
#define RELOC_H

#include <limits.h>
#include <stdlib.h>
#include <string.h>

// strict ISO modes hide it on glibc
#ifndef PATH_MAX
#define PATH_MAX 4096
#endif

#include "reloc_self.h"

#ifdef __cplusplus
extern "C" {
#endif

struct reloc_entry {
   struct reloc_entry* next;
   const char* relative;
   char* absolute;
};

// static, so each executable and library has its own copy and finds the file it is linked into
__attribute__((unused)) static const char* reloc_directory(void) {
   static char directory[PATH_MAX];
   static int resolved;
   if (!resolved) {
      resolved = 1;
      if (reloc_self((const void*)&reloc_directory, directory, sizeof directory)) {
         char* slash = strrchr(directory, '/');
         if (slash) {
            slash[1] = 0;
         }
      } else {
         directory[0] = 0;
      }
   }
   return directory;
}

// answers stay valid for the life of the process, callers keep the pointers
__attribute__((unused)) static const char* reloc_path(const char* relative) {
   static struct reloc_entry* entries;
   for (struct reloc_entry* entry = entries; entry; entry = entry->next) {
      if (entry->relative == relative || strcmp(entry->relative, relative) == 0) {
         return entry->absolute;
      }
   }

   const char* directory = reloc_directory();
   size_t prefix = strlen(directory);
   size_t length = strlen(relative);
   struct reloc_entry* entry = (struct reloc_entry*)malloc(sizeof *entry + prefix + length + 1);
   if (!entry) {
      return relative;
   }
   entry->absolute = (char*)(entry + 1);
   memcpy(entry->absolute, directory, prefix);
   memcpy(entry->absolute + prefix, relative, length + 1);
   entry->relative = relative;
   entry->next = entries;
   entries = entry;
   return entry->absolute;
}

#define RELOC(relative) reloc_path(relative)

#ifdef __cplusplus
}
#endif

#endif
