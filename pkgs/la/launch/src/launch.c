// The one program behind every installed script and wrapped binary. On ELF platforms bin/foo is
// a symlink to this binary in its own store path, on Windows bin/foo.exe is a copy of it. Either
// way it reads bin/.foo.launch, which bigtop's launchers wrote, and runs what that names.
//
// The record is JSON, every string in it may use {root} (the package), {store} (its parent),
// {self} (bin/foo) and {argv0} (what we were invoked as):
//   {"env": {"PATH": {"prepend": ["{store}/…/bin"]}, "TZDIR": {"default": "…"}, "X": {"set": "…"}},
//    "program": "{root}/bin/.foo", "args": ["…"], "argv0": "{argv0}"}

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#define WINDOWS_PATHS 1
#else
#define _GNU_SOURCE
#include <limits.h>
#include <unistd.h>
#define WINDOWS_PATHS 0
#endif

#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

enum { LAUNCH_FAILED = 127, MAX_HOPS = 32 };

static void fail(const char* what, const char* detail) {
   fprintf(stderr, "launch: %s%s%s\n", what, detail ? ": " : "", detail ? detail : "");
   exit(LAUNCH_FAILED);
}

static void* allocate(size_t size) {
   void* block = malloc(size);
   if (!block) {
      fail("out of memory", NULL);
   }
   return block;
}

static char* concat(const char* left, const char* middle, const char* right) {
   size_t first = strlen(left);
   size_t second = strlen(middle);
   char* joined = allocate(first + second + strlen(right) + 1);
   memcpy(joined, left, first);
   memcpy(joined + first, middle, second);
   strcpy(joined + first + second, right);
   return joined;
}

static size_t base_offset(const char* text) {
   size_t offset = 0;
   for (size_t index = 0; text[index]; index++) {
      if (text[index] == '/' || (text[index] == '\\' && WINDOWS_PATHS)) {
         offset = index + 1;
      }
   }
   return offset;
}

static char* directory_of(const char* path) {
   size_t offset = base_offset(path);
   size_t length = offset > 1 ? offset - 1 : offset;
   char* directory = allocate(length + 1);
   memcpy(directory, path, length);
   directory[length] = 0;
   return directory;
}

struct context {
   const char* root;
   const char* store;
   const char* self;
   const char* argv0;
};

static char* expand(const char* text, const struct context* ctx) {
   const struct {
      const char* name;
      const char* value;
   } holes[] = {{"{root}", ctx->root},
                {"{store}", ctx->store},
                {"{self}", ctx->self},
                {"{argv0}", ctx->argv0}};
   size_t capacity = strlen(text) + 1;
   for (const char* cursor = text; *cursor; cursor++) {
      for (size_t hole = 0; hole < sizeof holes / sizeof *holes; hole++) {
         if (strncmp(cursor, holes[hole].name, strlen(holes[hole].name)) == 0) {
            capacity += strlen(holes[hole].value);
         }
      }
   }

   char* expanded = allocate(capacity);
   char* out = expanded;
   while (*text) {
      size_t hole = 0;
      while (hole < sizeof holes / sizeof *holes &&
             strncmp(text, holes[hole].name, strlen(holes[hole].name)) != 0) {
         hole++;
      }
      if (hole == sizeof holes / sizeof *holes) {
         *out++ = *text++;
         continue;
      }
      size_t length = strlen(holes[hole].value);
      memcpy(out, holes[hole].value, length);
      out += length;
      text += strlen(holes[hole].name);
   }
   *out = 0;
   return expanded;
}

// serde_json writes records with objects, arrays and strings only
struct reader {
   const char* cursor;
   const char* path;
};

static void malformed(const struct reader* in) {
   fail("bad record", in->path);
}

static void skip_space(struct reader* in) {
   while (*in->cursor == ' ' || *in->cursor == '\n' || *in->cursor == '\r' || *in->cursor == '\t') {
      in->cursor++;
   }
}

static void expect(struct reader* in, char wanted) {
   skip_space(in);
   if (*in->cursor != wanted) {
      malformed(in);
   }
   in->cursor++;
}

static int next_is(struct reader* in, char wanted) {
   skip_space(in);
   if (*in->cursor != wanted) {
      return 0;
   }
   in->cursor++;
   return 1;
}

static unsigned hex4(struct reader* in) {
   unsigned value = 0;
   for (int digit = 0; digit < 4; digit++) {
      char chr = *in->cursor++;
      value <<= 4;
      if (chr >= '0' && chr <= '9') {
         value |= (unsigned)(chr - '0');
      } else if (chr >= 'a' && chr <= 'f') {
         value |= (unsigned)(chr - 'a' + 10);
      } else if (chr >= 'A' && chr <= 'F') {
         value |= (unsigned)(chr - 'A' + 10);
      } else {
         malformed(in);
      }
   }
   return value;
}

static char* put_utf8(char* out, unsigned point) {
   if (point < 0x80) {
      *out++ = (char)point;
   } else if (point < 0x800) {
      *out++ = (char)(0xC0 | point >> 6);
      *out++ = (char)(0x80 | (point & 0x3F));
   } else if (point < 0x10000) {
      *out++ = (char)(0xE0 | point >> 12);
      *out++ = (char)(0x80 | (point >> 6 & 0x3F));
      *out++ = (char)(0x80 | (point & 0x3F));
   } else {
      *out++ = (char)(0xF0 | point >> 18);
      *out++ = (char)(0x80 | (point >> 12 & 0x3F));
      *out++ = (char)(0x80 | (point >> 6 & 0x3F));
      *out++ = (char)(0x80 | (point & 0x3F));
   }
   return out;
}

// escapes never grow the text, so the raw length bounds the decoded one
static char* read_string(struct reader* in) {
   expect(in, '"');
   const char* end = in->cursor;
   while (*end && *end != '"') {
      end += *end == '\\' && end[1] ? 2 : 1;
   }
   if (!*end) {
      malformed(in);
   }

   char* text = allocate((size_t)(end - in->cursor) + 1);
   char* out = text;
   while (*in->cursor != '"') {
      char chr = *in->cursor++;
      if (chr != '\\') {
         *out++ = chr;
         continue;
      }
      chr = *in->cursor++;
      switch (chr) {
         case '"':
         case '\\':
         case '/':
            *out++ = chr;
            break;
         case 'b':
            *out++ = '\b';
            break;
         case 'f':
            *out++ = '\f';
            break;
         case 'n':
            *out++ = '\n';
            break;
         case 'r':
            *out++ = '\r';
            break;
         case 't':
            *out++ = '\t';
            break;
         case 'u': {
            unsigned point = hex4(in);
            if (point >= 0xD800 && point < 0xDC00 && in->cursor[0] == '\\' &&
                in->cursor[1] == 'u') {
               in->cursor += 2;
               point = 0x10000 + ((point - 0xD800) << 10) + (hex4(in) - 0xDC00);
            }
            out = put_utf8(out, point);
            break;
         }
         default:
            malformed(in);
      }
   }
   in->cursor++;
   *out = 0;
   return text;
}

struct list {
   char** items;
   size_t count;
};

static struct list read_strings(struct reader* in, const struct context* ctx) {
   struct list list = {0};
   size_t capacity = 0;
   expect(in, '[');
   if (next_is(in, ']')) {
      return list;
   }
   do {
      if (list.count == capacity) {
         capacity = capacity ? capacity * 2 : 4;
         list.items = realloc(list.items, capacity * sizeof *list.items);
         if (!list.items) {
            fail("out of memory", NULL);
         }
      }
      char* raw = read_string(in);
      list.items[list.count++] = expand(raw, ctx);
      free(raw);
   } while (next_is(in, ','));
   expect(in, ']');
   return list;
}

static char* read_expanded(struct reader* in, const struct context* ctx) {
   char* raw = read_string(in);
   char* expanded = expand(raw, ctx);
   free(raw);
   return expanded;
}

#ifdef _WIN32

static const char PATH_SEP[] = ";";

static wchar_t* wide(const char* text) {
   int size = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, text, -1, NULL, 0);
   if (size <= 0) {
      fail("not UTF-8", text);
   }
   wchar_t* converted = allocate((size_t)size * sizeof *converted);
   MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, text, -1, converted, size);
   return converted;
}

static char* narrow(const wchar_t* text) {
   int size = WideCharToMultiByte(CP_UTF8, 0, text, -1, NULL, 0, NULL, NULL);
   if (size <= 0) {
      fail("WideCharToMultiByte failed", NULL);
   }
   char* converted = allocate((size_t)size);
   WideCharToMultiByte(CP_UTF8, 0, text, -1, converted, size, NULL, NULL);
   return converted;
}

// the environment is UTF-16, the narrow CRT would lose what the ANSI code page cannot hold
static char* get_env(const char* name) {
   wchar_t* key = wide(name);
   DWORD size = GetEnvironmentVariableW(key, NULL, 0);
   if (size == 0) {
      free(key);
      return NULL;
   }
   wchar_t* value = allocate(size * sizeof *value);
   if (GetEnvironmentVariableW(key, value, size) >= size) {
      fail("GetEnvironmentVariable failed", name);
   }
   char* converted = narrow(value);
   free(key);
   free(value);
   return converted;
}

static void set_env(const char* name, const char* value) {
   if (!SetEnvironmentVariableW(wide(name), wide(value))) {
      fail("SetEnvironmentVariable failed", name);
   }
}

static char* last_error(const char* call) {
   char* message = allocate(strlen(call) + 32);
   sprintf(message, "%s failed (%lu)", call, (unsigned long)GetLastError());
   return message;
}

// GetModuleFileName under Wine does not resolve unix symlinks, so the directory is made final
// here and runs from result links still find their record
static char* invoked_path(const char* argv0) {
   (void)argv0;
   wchar_t module[32768];
   DWORD length = GetModuleFileNameW(NULL, module, sizeof module / sizeof *module);
   if (length == 0 || length >= sizeof module / sizeof *module) {
      fail(last_error("GetModuleFileName"), NULL);
   }
   char* path = narrow(module);
   size_t offset = base_offset(path);
   char* directory = directory_of(path);

   HANDLE handle = CreateFileW(wide(directory), 0, FILE_SHARE_READ | FILE_SHARE_WRITE, NULL,
                               OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS, NULL);
   if (handle == INVALID_HANDLE_VALUE) {
      fail(last_error("CreateFile"), directory);
   }
   wchar_t final[32768];
   DWORD size = GetFinalPathNameByHandleW(handle, final, sizeof final / sizeof *final, 0);
   CloseHandle(handle);
   if (size == 0 || size >= sizeof final / sizeof *final) {
      fail(last_error("GetFinalPathNameByHandle"), directory);
   }
   const wchar_t* plain = wcsncmp(final, L"\\\\?\\", 4) == 0 ? final + 4 : final;
   return concat(narrow(plain), "\\", path + offset);
}

// CommandLineToArgvW in reverse
static void append_arg(wchar_t** line, size_t* used, const char* arg) {
   wchar_t* text = wide(arg);
   size_t room = *used + 4 + wcslen(text) * 2;
   *line = realloc(*line, (room + 1) * sizeof **line);
   if (!*line) {
      fail("out of memory", NULL);
   }
   wchar_t* out = *line + *used;
   *out++ = L' ';
   *out++ = L'"';
   size_t slashes = 0;
   for (const wchar_t* chr = text; *chr; chr++) {
      if (*chr == L'\\') {
         slashes++;
         continue;
      }
      size_t escapes = *chr == L'"' ? slashes * 2 + 1 : slashes;
      for (size_t index = 0; index < escapes; index++) {
         *out++ = L'\\';
      }
      slashes = 0;
      *out++ = *chr;
   }
   for (size_t index = 0; index < slashes * 2; index++) {
      *out++ = L'\\';
   }
   *out++ = L'"';
   *out = 0;
   *used = (size_t)(out - *line);
   free(text);
}

// the user's arguments go on as the unparsed tail of our own command line
static const wchar_t* user_args(void) {
   const wchar_t* line = GetCommandLineW();
   if (*line == L'"') {
      const wchar_t* close = wcschr(line + 1, L'"');
      return close ? close + 1 : line + wcslen(line);
   }
   while (*line && *line != L' ' && *line != L'\t') {
      line++;
   }
   return line;
}

// No execve here. The child runs in a kill-on-close job so it cannot outlive us, Ctrl-C reaches it
// through the console, and its exit code becomes ours.
static void run(const char* program, const char* argv0, const struct list* pre, int argc,
                char** argv) {
   (void)argc;
   (void)argv;
   wchar_t* line = NULL;
   size_t used = 0;
   append_arg(&line, &used, argv0);
   for (size_t index = 0; index < pre->count; index++) {
      append_arg(&line, &used, pre->items[index]);
   }
   const wchar_t* tail = user_args();
   line = realloc(line, (used + wcslen(tail) + 1) * sizeof *line);
   if (!line) {
      fail("out of memory", NULL);
   }
   wcscpy(line + used, tail);

   HANDLE job = CreateJobObjectW(NULL, NULL);
   JOBOBJECT_EXTENDED_LIMIT_INFORMATION limit = {0};
   limit.BasicLimitInformation.LimitFlags =
      JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK;
   if (!job ||
       !SetInformationJobObject(job, JobObjectExtendedLimitInformation, &limit, sizeof limit)) {
      fail(last_error("CreateJobObject"), NULL);
   }

   STARTUPINFOW startup = {.cb = sizeof startup};
   PROCESS_INFORMATION child = {0};
   if (!CreateProcessW(wide(program), line + 1, NULL, NULL, TRUE, CREATE_SUSPENDED, NULL, NULL,
                       &startup, &child)) {
      fail(last_error("CreateProcess"), program);
   }
   // nested jobs are refused before Windows 8, the child then runs unsupervised
   AssignProcessToJobObject(job, child.hProcess);
   SetConsoleCtrlHandler(NULL, TRUE);
   if (ResumeThread(child.hThread) == (DWORD)-1) {
      fail(last_error("ResumeThread"), program);
   }
   CloseHandle(child.hThread);

   DWORD code = 0;
   if (WaitForSingleObject(child.hProcess, INFINITE) != WAIT_OBJECT_0 ||
       !GetExitCodeProcess(child.hProcess, &code)) {
      fail(last_error("WaitForSingleObject"), program);
   }
   exit((int)code);
}

#else

static const char PATH_SEP[] = ":";

static char* get_env(const char* name) {
   const char* value = getenv(name);
   return value ? strdup(value) : NULL;
}

static void set_env(const char* name, const char* value) {
   if (setenv(name, value, 1) != 0) {
      fail(strerror(errno), name);
   }
}

static char* absolute(const char* path) {
   if (path[0] == '/') {
      return strdup(path);
   }
   char cwd[PATH_MAX];
   if (!getcwd(cwd, sizeof cwd)) {
      fail(strerror(errno), "getcwd");
   }
   return concat(cwd, "/", path);
}

// argv[0] as the shell resolved it: as given with a slash, otherwise the first hit on PATH
static char* argv0_path(const char* argv0) {
   if (strchr(argv0, '/')) {
      return absolute(argv0);
   }
   const char* search = getenv("PATH");
   if (!search) {
      fail("argv[0] has no '/' and PATH is unset", argv0);
   }
   for (;;) {
      size_t length = strcspn(search, ":");
      char* directory = strndup(search, length);
      char* candidate = concat(directory, "/", argv0);
      free(directory);
      if (access(candidate, X_OK) == 0) {
         return absolute(candidate);
      }
      free(candidate);
      if (!search[length]) {
         fail("not found on PATH", argv0);
      }
      search += length + 1;
   }
}

// Profiles and result links may stand in front of bin/foo, so the chain is followed hop by hop
// to the link that points at a file named launch. That link's name picks the record.
static char* invoked_path(const char* argv0) {
   char* current = argv0_path(argv0);
   for (int hop = 0; hop < MAX_HOPS; hop++) {
      char target[PATH_MAX];
      ssize_t length = readlink(current, target, sizeof target - 1);
      if (length < 0) {
         fail("not a launcher symlink", current);
      }
      target[length] = 0;

      char* directory = directory_of(current);
      if (strcmp(target + base_offset(target), "launch") == 0) {
         char resolved[PATH_MAX];
         if (!realpath(directory, resolved)) {
            fail(strerror(errno), directory);
         }
         return concat(resolved, "/", current + base_offset(current));
      }
      char* next = target[0] == '/' ? strdup(target) : concat(directory, "/", target);
      free(directory);
      free(current);
      current = next;
   }
   fail("symlink loop", argv0);
   return NULL;
}

static void run(const char* program, const char* argv0, const struct list* pre, int argc,
                char** argv) {
   char** args = allocate((pre->count + (size_t)argc + 1) * sizeof *args);
   size_t count = 0;
   args[count++] = (char*)argv0;
   for (size_t index = 0; index < pre->count; index++) {
      args[count++] = pre->items[index];
   }
   for (int index = 1; index < argc; index++) {
      args[count++] = argv[index];
   }
   args[count] = NULL;
   execv(program, args);
   fail(strerror(errno), program);
}

#endif

static char* join(const struct list* parts, const char* tail) {
   size_t size = 1 + (tail ? strlen(tail) + 1 : 0);
   for (size_t index = 0; index < parts->count; index++) {
      size += strlen(parts->items[index]) + 1;
   }
   char* joined = allocate(size);
   joined[0] = 0;
   for (size_t index = 0; index < parts->count; index++) {
      if (index) {
         strcat(joined, PATH_SEP);
      }
      strcat(joined, parts->items[index]);
   }
   if (tail) {
      if (parts->count) {
         strcat(joined, PATH_SEP);
      }
      strcat(joined, tail);
   }
   return joined;
}

static void apply_variable(struct reader* in, const char* name, const struct context* ctx) {
   expect(in, '{');
   char* verb = read_string(in);
   expect(in, ':');
   char* current = get_env(name);
   if (strcmp(verb, "prepend") == 0) {
      struct list parts = read_strings(in, ctx);
      set_env(name, join(&parts, current && *current ? current : NULL));
   } else if (strcmp(verb, "default") == 0) {
      char* value = read_expanded(in, ctx);
      if (!current) {
         set_env(name, value);
      }
   } else if (strcmp(verb, "set") == 0) {
      set_env(name, read_expanded(in, ctx));
   } else {
      fail("unknown env verb", verb);
   }
   expect(in, '}');
   free(verb);
   free(current);
}

static char* read_file(const char* path) {
#ifdef _WIN32
   FILE* file = _wfopen(wide(path), L"rb");
#else
   FILE* file = fopen(path, "rb");
#endif
   if (!file) {
      fail("cannot read record", path);
   }
   size_t capacity = 4096;
   size_t used = 0;
   char* text = allocate(capacity);
   for (;;) {
      used += fread(text + used, 1, capacity - used - 1, file);
      if (used < capacity - 1) {
         break;
      }
      capacity *= 2;
      text = realloc(text, capacity);
      if (!text) {
         fail("out of memory", NULL);
      }
   }
   if (ferror(file)) {
      fail("cannot read record", path);
   }
   fclose(file);
   text[used] = 0;
   return text;
}

int main(int argc, char** argv) {
   if (argc < 1 || !argv[0]) {
      fail("no argv[0]", NULL);
   }
   char* self = invoked_path(argv[0]);
   char* bin = directory_of(self);
   char* root = directory_of(bin);
   struct context ctx = {root, directory_of(root), self, argv[0]};
   char* record = concat(bin, WINDOWS_PATHS ? "\\." : "/.", self + base_offset(self));
   record = concat(record, ".launch", "");

   struct reader in = {read_file(record), record};
   char* program = NULL;
   char* argv0 = NULL;
   struct list pre = {0};
   expect(&in, '{');
   if (!next_is(&in, '}')) {
      do {
         char* key = read_string(&in);
         expect(&in, ':');
         if (strcmp(key, "program") == 0) {
            program = read_expanded(&in, &ctx);
         } else if (strcmp(key, "argv0") == 0) {
            argv0 = read_expanded(&in, &ctx);
         } else if (strcmp(key, "args") == 0) {
            pre = read_strings(&in, &ctx);
         } else if (strcmp(key, "env") == 0) {
            expect(&in, '{');
            if (!next_is(&in, '}')) {
               do {
                  char* name = read_string(&in);
                  expect(&in, ':');
                  apply_variable(&in, name, &ctx);
                  free(name);
               } while (next_is(&in, ','));
               expect(&in, '}');
            }
         } else {
            fail("unknown record field", key);
         }
         free(key);
      } while (next_is(&in, ','));
      expect(&in, '}');
   }
   if (!program) {
      fail("record lacks program", record);
   }

   run(program, argv0 ? argv0 : program, &pre, argc, argv);
   return LAUNCH_FAILED;
}
