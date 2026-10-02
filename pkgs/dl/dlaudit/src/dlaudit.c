// rtld-audit(7) module writing each soname a dlopen searched for and never opened to
// $DLAUDIT_OUT. The loader runs it under its lock before libc is set up, so it allocates nothing.
#define _GNU_SOURCE
#include <dlfcn.h>
#include <fcntl.h>
#include <link.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

enum { LONGEST = 255 };

static int report = -1;
static char pending[LONGEST + 1];
static size_t length;

static const char* const prefixes[] = {"libnss_", "libnsl", "libidn2", "libgcc_s.so"};

static int starts_with(const char* text, const char* prefix) {
   return strncmp(text, prefix, strlen(prefix)) == 0;
}

// glibc loads these by soname on its own and copes when they are missing
static int optional(const char* name) {
   for (size_t index = 0; index < sizeof prefixes / sizeof *prefixes; index++) {
      if (starts_with(name, prefixes[index])) {
         return 1;
      }
   }

   return 0;
}

static void settle(void) {
   if (length > 0 && report >= 0) {
      pending[length] = '\n';
      (void)write(report, pending, length + 1);
   }

   length = 0;
}

unsigned la_version(unsigned version) {
   const char* path = getenv("DLAUDIT_OUT");
   if (path) {
      report = open(path, O_WRONLY | O_APPEND | O_CREAT | O_CLOEXEC, 0644);
   }
   return version;
}

// the other flags are the loader trying directories for the same name
char* la_objsearch(const char* name, uintptr_t* cookie, unsigned flag) {
   (void)cookie;
   if (flag != LA_SER_ORIG) {
      return (char*)name;
   }

   settle();
   size_t size = strlen(name);
   if (size <= LONGEST && !strchr(name, '/') && !optional(name)) {
      memcpy(pending, name, size);
      length = size;
   }
   return (char*)name;
}

// a versioned alias answers too, libfoo.so -> libfoo.so.1
unsigned la_objopen(struct link_map* map, Lmid_t lmid, uintptr_t* cookie) {
   (void)lmid;
   (void)cookie;
   const char* slash = strrchr(map->l_name, '/');
   const char* file = slash ? slash + 1 : map->l_name;
   if (length > 0 && strncmp(file, pending, length) == 0) {
      length = 0;
   }
   return 0;
}

void la_preinit(uintptr_t* cookie) {
   (void)cookie;
   settle();
}

__attribute__((destructor)) static void finish(void) {
   settle();
}
