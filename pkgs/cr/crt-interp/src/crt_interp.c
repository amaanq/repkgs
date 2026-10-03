// Entry point for executables whose interpreter path is relative to the executable. mummer links
// it in front of crt1 and flips PT_INTERP to PT_NULL after the link, so the kernel starts us
// without one. We load that interpreter ourselves, put PT_INTERP back in memory, point AT_BASE
// and AT_ENTRY at it and at the program, and enter it on the kernel's untouched stack.
//
// With -DRELOC_STUB the same code becomes reloc_stub.bin, which formatelf implants into ELFs we
// did not link. The program's entry then comes from a header formatelf fills in.

typedef unsigned char u8;
typedef unsigned short u16;
typedef unsigned int u32;
typedef unsigned long u64;
typedef long i64;

enum { AUX_END = 0, AUX_PHDR = 3, AUX_PHNUM = 5, AUX_PAGESZ = 6, AUX_BASE = 7, AUX_ENTRY = 9 };
enum { AUX_EXECFN = 31 };
enum { SEG_NULL = 0, SEG_LOAD = 1, SEG_INTERP = 3, SEG_PHDR = 6 };
enum { READ = 1, WRITE = 2, EXEC = 4 };
enum { PRIVATE = 0x02, FIXED = 0x10, ANONYMOUS = 0x20 };
enum { CLOEXEC = 02000000, CWD = -100, PATH_LEN = 4096 };

struct elf_header {
   u8 ident[16];
   u16 type, machine;
   u32 version;
   u64 entry, phoff, shoff;
   u32 flags;
   u16 ehsize, phentsize, phnum, shentsize, shnum, shstrndx;
};

struct segment {
   u32 type, flags;
   u64 offset, vaddr, paddr, filesz, memsz, align;
};

#ifdef RELOC_STUB
// volatile so the load survives, formatelf writes `entry` after the compiler saw 0
__attribute__((section(".text.header"), used, visibility("hidden"))) const volatile struct {
   char magic[8];
   u64 entry;
} reloc_stub_header = {{'R', 'E', 'L', 'O', 'C', 'S', 'T', 'B'}, 0};
#else
// hidden so its address is pc-relative: a GOT load reads 0 this early, nothing is relocated yet
extern void _start(void) __attribute__((visibility("hidden")));
#endif

#if defined(__x86_64__)
enum { NR_WRITE = 1, NR_CLOSE = 3, NR_MMAP = 9, NR_MPROTECT = 10, NR_PREAD = 17, NR_EXIT = 60 };
enum { NR_OPENAT = 257, NR_READLINKAT = 267 };

static i64 syscall6(i64 nr, i64 a, i64 b, i64 c, i64 d, i64 e, i64 f) {
   register i64 r10 __asm__("r10") = d;
   register i64 r8 __asm__("r8") = e;
   register i64 r9 __asm__("r9") = f;
   i64 ret;
   __asm__ volatile("syscall"
                    : "=a"(ret)
                    : "a"(nr), "D"(a), "S"(b), "d"(c), "r"(r10), "r"(r8), "r"(r9)
                    : "rcx", "r11", "memory");
   return ret;
}
#elif defined(__powerpc64__)
enum { NR_EXIT = 1, NR_WRITE = 4, NR_CLOSE = 6, NR_MMAP = 90, NR_MPROTECT = 125 };
enum { NR_PREAD = 179, NR_OPENAT = 286, NR_READLINKAT = 296 };

static i64 syscall6(i64 nr, i64 a, i64 b, i64 c, i64 d, i64 e, i64 f) {
   register i64 r0 __asm__("r0") = nr;
   register i64 r3 __asm__("r3") = a;
   register i64 r4 __asm__("r4") = b;
   register i64 r5 __asm__("r5") = c;
   register i64 r6 __asm__("r6") = d;
   register i64 r7 __asm__("r7") = e;
   register i64 r8 __asm__("r8") = f;
   // the kernel reports failure in cr0.SO with a positive errno
   __asm__ volatile(
      "sc\n"
      "bns+ 1f\n"
      "neg %0, %0\n"
      "1:"
      : "+r"(r3), "+r"(r0), "+r"(r4), "+r"(r5), "+r"(r6), "+r"(r7), "+r"(r8)
      :
      : "r9", "r10", "r11", "r12", "cr0", "ctr", "memory");
   return r3;
}
#elif defined(__aarch64__)
enum { NR_OPENAT = 56, NR_CLOSE = 57, NR_WRITE = 64, NR_PREAD = 67, NR_READLINKAT = 78 };
enum { NR_EXIT = 93, NR_MMAP = 222, NR_MPROTECT = 226 };

static i64 syscall6(i64 nr, i64 a, i64 b, i64 c, i64 d, i64 e, i64 f) {
   register i64 x8 __asm__("x8") = nr;
   register i64 x0 __asm__("x0") = a;
   register i64 x1 __asm__("x1") = b;
   register i64 x2 __asm__("x2") = c;
   register i64 x3 __asm__("x3") = d;
   register i64 x4 __asm__("x4") = e;
   register i64 x5 __asm__("x5") = f;
   __asm__ volatile("svc 0"
                    : "+r"(x0)
                    : "r"(x8), "r"(x1), "r"(x2), "r"(x3), "r"(x4), "r"(x5)
                    : "memory");
   return x0;
}
#elif defined(__riscv) || defined(__loongarch64)
enum { NR_OPENAT = 56, NR_CLOSE = 57, NR_WRITE = 64, NR_PREAD = 67, NR_READLINKAT = 78 };
enum { NR_EXIT = 93, NR_MMAP = 222, NR_MPROTECT = 226 };

static i64 syscall6(i64 nr, i64 a, i64 b, i64 c, i64 d, i64 e, i64 f) {
   register i64 a7 __asm__("a7") = nr;
   register i64 a0 __asm__("a0") = a;
   register i64 a1 __asm__("a1") = b;
   register i64 a2 __asm__("a2") = c;
   register i64 a3 __asm__("a3") = d;
   register i64 a4 __asm__("a4") = e;
   register i64 a5 __asm__("a5") = f;
#if defined(__riscv)
   __asm__ volatile("ecall"
                    : "+r"(a0)
                    : "r"(a7), "r"(a1), "r"(a2), "r"(a3), "r"(a4), "r"(a5)
                    : "memory");
#else
   __asm__ volatile("syscall 0"
                    : "+r"(a0)
                    : "r"(a7), "r"(a1), "r"(a2), "r"(a3), "r"(a4), "r"(a5)
                    : "$t0", "$t1", "$t2", "$t3", "$t4", "$t5", "$t6", "$t7", "$t8", "memory");
#endif
   return a0;
}
#else
#error unsupported architecture
#endif

static i64 call(i64 nr, i64 a, i64 b, i64 c) {
   return syscall6(nr, a, b, c, 0, 0, 0);
}

static int failed(i64 ret) {
   return ret < 0 && ret > -4096;
}

static u64 length(const char* text) {
   const char* end = text;
   while (*end) {
      end++;
   }
   return (u64)(end - text);
}

static void say(const char* text) {
   call(NR_WRITE, 2, (i64)text, (i64)length(text));
}

__attribute__((noreturn)) static void fail(const char* why) {
   say("reloc-interp: ");
   say(why);
   say("\n");
   for (;;) {
      call(NR_EXIT, 127, 0, 0);
   }
}

static int protection(u32 flags) {
   return (flags & 4 ? READ : 0) | (flags & 2 ? WRITE : 0) | (flags & 1 ? EXEC : 0);
}

static u64 page_down(u64 value, u64 page) {
   return value & ~(page - 1);
}

static u64 page_up(u64 value, u64 page) {
   return page_down(value + page - 1, page);
}

struct process {
   u64* auxv;
   struct segment* segments;
   u64 count;
   u64 page;
   const char* execfn;
   u64 bias;
   int has_phdr;
   struct segment* disabled;
};

static void read_auxv(struct process* proc, u64* stack) {
   u64* cursor = stack + 1 + stack[0] + 1;
   while (*cursor) {
      cursor++;
   }
   proc->auxv = cursor + 1;
   proc->page = 4096;

   for (u64* entry = proc->auxv; entry[0] != AUX_END; entry += 2) {
      if (entry[0] == AUX_PHDR) {
         proc->segments = (struct segment*)entry[1];
      } else if (entry[0] == AUX_PHNUM) {
         proc->count = entry[1];
      } else if (entry[0] == AUX_PAGESZ) {
         proc->page = entry[1];
      } else if (entry[0] == AUX_EXECFN) {
         proc->execfn = (const char*)entry[1];
      }
   }
   if (!proc->segments) {
      fail("no AT_PHDR");
   }

   for (u64 index = 0; index < proc->count; index++) {
      struct segment* seg = &proc->segments[index];
      if (seg->type == SEG_PHDR) {
         proc->bias = (u64)proc->segments - seg->vaddr;
         proc->has_phdr = 1;
      } else if (seg->type == SEG_NULL && seg->filesz > 1) {
         proc->disabled = seg;
      }
   }
   if (!proc->disabled) {
      fail("no disabled PT_INTERP (PT_NULL with contents) found");
   }
}

// the directory of our own executable joined with the interpreter path relative to it
static void interpreter_path(const struct process* proc, char* path) {
   const char* relative = (const char*)(proc->bias + proc->disabled->vaddr);
   i64 size = syscall6(NR_READLINKAT, CWD, (i64) "/proc/self/exe", (i64)path, PATH_LEN - 1, 0, 0);
   if (size <= 0) {
      if (!proc->execfn) {
         fail("neither /proc/self/exe nor AT_EXECFN");
      }
      size = (i64)length(proc->execfn);
      if (size >= PATH_LEN) {
         fail("path too long");
      }
      for (i64 index = 0; index < size; index++) {
         path[index] = proc->execfn[index];
      }
   }

   if (relative[0] == '/') {
      size = 0;
   }
   while (size > 0 && path[size - 1] != '/') {
      size--;
   }
   u64 tail = length(relative);
   if ((u64)size + tail + 1 > PATH_LEN) {
      fail("path too long");
   }
   for (u64 index = 0; index <= tail; index++) {
      path[size + index] = relative[index];
   }
}

static void map_segment(const struct segment* seg, u64 bias, u64 page, i64 file) {
   int prot = protection(seg->flags);
   u64 start = page_down(seg->vaddr, page);
   u64 file_end = seg->vaddr + seg->filesz;
   u64 file_pages = page_up(file_end, page);
   u64 mem_pages = page_up(seg->vaddr + seg->memsz, page);

   i64 offset = (i64)page_down(seg->offset, page);
   if (file_pages > start &&
       failed(syscall6(NR_MMAP, (i64)(bias + start), (i64)(file_pages - start), prot,
                       PRIVATE | FIXED, file, offset))) {
      fail("mmap segment failed");
   }
   if (seg->memsz <= seg->filesz) {
      return;
   }

   // ld.so uses the rest of its last bss page as heap and expects the zeros the kernel leaves
   if (prot & WRITE) {
      for (volatile u8* byte = (u8*)(bias + file_end); (u64)byte < bias + file_pages; byte++) {
         *byte = 0;
      }
   }
   if (mem_pages > file_pages &&
       failed(syscall6(NR_MMAP, (i64)(bias + file_pages), (i64)(mem_pages - file_pages), prot,
                       PRIVATE | FIXED | ANONYMOUS, -1, 0))) {
      fail("mmap bss failed");
   }
}

// maps the interpreter like the kernel would, returns its load bias and stores its entry
static u64 load(const char* path, u64 page, u64* entry) {
   i64 file = call(NR_OPENAT, CWD, (i64)path, CLOEXEC);
   if (failed(file)) {
      fail("cannot open interpreter (relative path)");
   }

   union {
      struct elf_header header;
      u8 bytes[4096];
   } head;
   if (syscall6(NR_PREAD, file, (i64)&head, sizeof head, 0, 0, 0) < (i64)sizeof head.header) {
      fail("short read");
   }
   const struct elf_header* header = &head.header;
   if (header->phoff + header->phnum * sizeof(struct segment) > sizeof head) {
      fail("phdrs out of first page");
   }
   const struct segment* segs = (const struct segment*)(head.bytes + header->phoff);

   u64 low = ~0UL;
   u64 high = 0;
   for (u16 index = 0; index < header->phnum; index++) {
      if (segs[index].type != SEG_LOAD) {
         continue;
      }
      if (segs[index].vaddr < low) {
         low = segs[index].vaddr;
      }
      if (segs[index].vaddr + segs[index].memsz > high) {
         high = segs[index].vaddr + segs[index].memsz;
      }
   }
   low = page_down(low, page);
   high = page_up(high, page);

   i64 span = syscall6(NR_MMAP, 0, (i64)(high - low), 0, PRIVATE | ANONYMOUS, -1, 0);
   if (failed(span)) {
      fail("mmap reserve failed");
   }
   u64 bias = (u64)span - low;
   for (u16 index = 0; index < header->phnum; index++) {
      if (segs[index].type == SEG_LOAD) {
         map_segment(&segs[index], bias, page, file);
      }
   }
   call(NR_CLOSE, file, 0, 0);

   *entry = bias + header->entry;
   return bias;
}

// glibc's ld.so looks for PT_INTERP among the program's headers
static void restore_interp(const struct process* proc) {
   u64 address = (u64)proc->disabled - proc->bias;
   int prot = READ;
   // the headers may sit in an r-x segment shared with .text (GNU ld's aarch64 default)
   for (u64 index = 0; index < proc->count; index++) {
      const struct segment* seg = &proc->segments[index];
      if (seg->type == SEG_LOAD && address - seg->vaddr < seg->memsz) {
         prot = protection(seg->flags);
      }
   }

   u64 start = page_down((u64)proc->disabled, proc->page);
   u64 size = page_up((u64)proc->disabled + sizeof(struct segment), proc->page) - start;
   if (failed(call(NR_MPROTECT, (i64)start, (i64)size, prot | WRITE))) {
      fail("mprotect phdr rw");
   }
   proc->disabled->type = SEG_INTERP;
   call(NR_MPROTECT, (i64)start, (i64)size, prot);
}

__attribute__((used)) static u64 reloc_main(u64* stack) {
   struct process proc = {0};
   read_auxv(&proc, stack);

#ifdef RELOC_STUB
   // formatelf always leaves a PT_PHDR, so the bias is exact for ET_EXEC too
   if (!proc.has_phdr) {
      fail("no PT_PHDR");
   }
   u64 program = proc.bias + reloc_stub_header.entry;
#else
   u64 program = (u64)&_start;
#endif

   char path[PATH_LEN];
   interpreter_path(&proc, path);
   u64 interp_entry = 0;
   u64 interp_base = load(path, proc.page, &interp_entry);
   restore_interp(&proc);

   int patched = 0;
   for (u64* entry = proc.auxv; entry[0] != AUX_END; entry += 2) {
      if (entry[0] == AUX_BASE) {
         entry[1] = interp_base;
         patched |= 1;
      } else if (entry[0] == AUX_ENTRY) {
         entry[1] = program;
         patched |= 2;
      }
   }
   if (patched != 3) {
      fail("auxv lacks AT_BASE/AT_ENTRY");
   }
   return interp_entry;
}

// Each entry keeps the kernel's stack pointer in a callee-saved register around reloc_main,
// then enters ld.so with the rtld_fini register zeroed as the kernel does.
#define ENTRY(body)                     \
   __asm__(                             \
      ".section .text.entry,\"ax\"\n"   \
      ".globl __reloc_start\n"          \
      ".type __reloc_start,%function\n" \
      "__reloc_start:\n" body)

#if defined(__x86_64__)
ENTRY(
   "mov %rsp, %rbx\n"
   "mov %rsp, %rdi\n"
   "and $-16, %rsp\n"
   "call reloc_main\n"
   "mov %rbx, %rsp\n"
   "xor %edx, %edx\n"
   "jmp *%rax\n");
#elif defined(__aarch64__)
ENTRY(
   "bti c\n"
   "mov x20, sp\n"
   "mov x0, sp\n"
   "and sp, x0, #-16\n"
   "bl reloc_main\n"
   "mov sp, x20\n"
   "mov x17, x0\n"
   "mov x0, xzr\n"
   "br x17\n");
#elif defined(__riscv)
ENTRY(
   "mv s2, sp\n"
   "mv a0, sp\n"
   "andi sp, sp, -16\n"
   "call reloc_main\n"
   "mv sp, s2\n"
   "mv t1, a0\n"
   "mv a0, zero\n"
   "jr t1\n");
#elif defined(__loongarch64)
ENTRY(
   "move $s1, $sp\n"
   "move $a0, $sp\n"
   "bstrins.d $sp, $zero, 3, 0\n"
   "bl reloc_main\n"
   "move $sp, $s1\n"
   "move $t1, $a0\n"
   "move $a0, $zero\n"
   "jr $t1\n");
#elif defined(__powerpc64__)
// ELFv2 enters with r12 at the entry address, the TOC is derived from it
ENTRY(
   "addis 2, 12, .TOC.-__reloc_start@ha\n"
   "addi 2, 2, .TOC.-__reloc_start@l\n"
   "mr 31, 1\n"
   "mr 3, 1\n"
   "clrrdi 1, 1, 4\n"
   "stdu 1, -32(1)\n"
   "bl reloc_main\n"
   "nop\n"
   "mr 1, 31\n"
   "mtctr 3\n"
   "mr 12, 3\n"
   "li 3, 0\n"
   "bctr\n");
#endif
