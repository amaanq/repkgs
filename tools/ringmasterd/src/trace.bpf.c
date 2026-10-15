/* The kernel half of ringmasterd. Every hook runs the same VFS code a ring's
 * request does, so io_uring is seen like a system call, and nothing here ever
 * makes a task wait. Struct layouts are declared by hand and relocated against
 * the running kernel's BTF when the object loads. */

typedef unsigned char __u8;
typedef unsigned short __u16;
typedef unsigned int __u32;
typedef unsigned long long __u64;
typedef int __s32;
typedef long long __s64;

#define SEC(name) __attribute__((section(name), used))
#define CORE __attribute__((preserve_access_index))
#define INLINE static __attribute__((always_inline)) inline
#define __uint(name, value) int (*name)[value]
#define __type(name, value) typeof(value)* name

#define AT_FDCWD -100
#define O_ACCMODE 3
#define O_TRUNC 01000
#define MAX_ERRNO 4095
#define LOOKUP_IN_ROOT (1U << 28)
#define BPF_NOEXIST 1

enum {
   BPF_MAP_TYPE_HASH = 1,
   BPF_MAP_TYPE_PERCPU_ARRAY = 6,
   BPF_MAP_TYPE_RINGBUF = 27,
};

static void* (*bpf_map_lookup_elem)(void* map, const void* key) = (void*)1;
static long (*bpf_map_update_elem)(void* map, const void* key, const void* value,
                                   __u64 flags) = (void*)2;
static long (*bpf_map_delete_elem)(void* map, const void* key) = (void*)3;
static __u64 (*bpf_get_current_pid_tgid)(void) = (void*)14;
static long (*bpf_probe_read_user)(void* dst, __u32 size, const void* src) = (void*)112;
static long (*bpf_probe_read_kernel)(void* dst, __u32 size, const void* src) = (void*)113;
static long (*bpf_probe_read_kernel_str)(void* dst, __u32 size, const void* src) = (void*)115;
static long (*bpf_ringbuf_output)(void* ringbuf, void* data, __u64 size, __u64 flags) = (void*)130;
static struct task_struct* (*bpf_get_current_task_btf)(void) = (void*)158;
static long (*bpf_loop)(__u32 count, void* callback, void* context, __u64 flags) = (void*)181;

struct qstr {
   __u32 len;
   const unsigned char* name;
} CORE;

struct dentry {
   struct dentry* d_parent;
   struct qstr d_name;
} CORE;

struct vfsmount {
   struct dentry* mnt_root;
} CORE;

struct mount {
   struct mount* mnt_parent;
   struct dentry* mnt_mountpoint;
   struct vfsmount mnt;
} CORE;

struct path {
   struct vfsmount* mnt;
   struct dentry* dentry;
} CORE;

struct file {
   struct path f_path;
} CORE;

struct fs_struct {
   struct path root;
   struct path pwd;
} CORE;

struct fdtable {
   unsigned int max_fds;
   struct file** fd;
} CORE;

struct files_struct {
   struct fdtable* fdt;
} CORE;

struct mm_struct {
   unsigned long arg_start;
   unsigned long arg_end;
} CORE;

struct task_struct {
   int pid;
   int tgid;
   struct mm_struct* mm;
   struct fs_struct* fs;
   struct files_struct* files;
} CORE;

struct open_flags {
   int open_flag;
   int lookup_flags;
} CORE;

struct linux_binprm {
   const char* filename;
} CORE;

#if defined(__TARGET_ARCH_x86)
struct pt_regs {
   unsigned long cs;
} CORE;
#define USER32_CS 0x23
#define X32_BIT 0x40000000
INLINE int foreign(struct pt_regs* regs, long id) {
   unsigned long cs = 0;
   bpf_probe_read_kernel(&cs, sizeof(cs), __builtin_preserve_access_index(&regs->cs));
   return cs == USER32_CS || (id & X32_BIT);
}
#elif defined(__TARGET_ARCH_arm64)
struct pt_regs {
   __u64 pstate;
} CORE;
#define PSR_MODE32_BIT 0x10
INLINE int foreign(struct pt_regs* regs, long id) {
   __u64 pstate = 0;
   bpf_probe_read_kernel(&pstate, sizeof(pstate), __builtin_preserve_access_index(&regs->pstate));
   return (pstate & PSR_MODE32_BIT) != 0;
}
#else
#error "ringmasterd knows x86 and arm64 compat calls only"
#endif

#define READ(destination, source)                             \
   bpf_probe_read_kernel(&(destination), sizeof(destination), \
                         __builtin_preserve_access_index(&(source)))

enum kind {
   KIND_OPEN = 1,
   KIND_LOOKUP = 2,
   KIND_CLOBBER = 3,
   KIND_LISTING = 4,
   KIND_EXEC = 5,
   KIND_FORK = 6,
   KIND_ESCAPE = 7,
   KIND_DEEP = 8,
};

/* Escapes that are no system call. */
#define ESCAPE_FOREIGN 0xFFFF0002U
#define ESCAPE_UNRESOLVED 0xFFFF0003U

/* base_of's answer for a descriptor the task does not have, which the kernel
 * fails as EBADF before looking the name up. */
#define NO_DIRECTORY -2

/* An exec's detail when its arguments only fit from the back. */
#define EXEC_TAIL 1U

#define FIELD 4096
#define MASK (FIELD - 1)
#define DEPTH (FIELD / 2)
#define DEEPEST (1 << 16)
#define PREFIX 15
#define SLOTS 8

struct record {
   __u64 session;
   __u32 kind;
   __u32 tgid;
   __u32 detail;
   __u16 base;
   __u16 name;
   __u16 args;
   __u16 reserved;
};

struct scratch {
   struct record head;
   char data[3 * FIELD];
   char walk[3 * FIELD];
   struct dentry* deep_dentry;
   struct vfsmount* deep_mount;
   __u32 claimed;
};

struct {
   __uint(type, BPF_MAP_TYPE_HASH);
   __uint(max_entries, 1 << 16);
   __type(key, __u32);
   __type(value, __u64);
} traced SEC(".maps");

/* Mirrors `Losses` in main.rs, one counter per reason a record never reached the ring. */
struct losses {
   __u64 slots;
   __u64 ring;
   __u64 oversize;
   __u64 forks;
};

enum loss { LOSS_SLOTS, LOSS_RING, LOSS_OVERSIZE, LOSS_FORKS };

struct {
   __uint(type, BPF_MAP_TYPE_HASH);
   __uint(max_entries, 1 << 10);
   __type(key, __u64);
   __type(value, struct losses);
} lost SEC(".maps");

struct {
   __uint(type, BPF_MAP_TYPE_HASH);
   __uint(max_entries, 64);
   __type(key, __u32);
   __type(value, __u32);
} escapes SEC(".maps");

struct {
   __uint(type, BPF_MAP_TYPE_RINGBUF);
   __uint(max_entries, 1 << 26);
} events SEC(".maps");

/* Several per CPU, since a trampoline only pins a program to its CPU, so on a
 * preemptible kernel another task can start a record there before this one
 * finishes. Task storage would allocate for every new process and fail under
 * a burst of forks. */
struct {
   __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
   __uint(max_entries, SLOTS);
   __type(key, __u32);
   __type(value, struct scratch);
} scratches SEC(".maps");

INLINE __u64* session_of(__u32 tgid) {
   return bpf_map_lookup_elem(&traced, &tgid);
}

INLINE void count_lost(__u64 session, enum loss why) {
   __u64* counts = bpf_map_lookup_elem(&lost, &session);
   if (!counts) {
      struct losses zero = {};
      bpf_map_update_elem(&lost, &session, &zero, BPF_NOEXIST);
      counts = bpf_map_lookup_elem(&lost, &session);
   }
   if (counts) {
      __sync_fetch_and_add(&counts[why & 3], 1);
   }
}

INLINE struct scratch* begin(__u64 session, __u32 kind, __u32 tgid, __u32 detail) {
   struct scratch* scratch = 0;
   for (__u32 slot = 0; slot < SLOTS && !scratch; slot++) {
      struct scratch* candidate = bpf_map_lookup_elem(&scratches, &slot);
      if (candidate && !__sync_val_compare_and_swap(&candidate->claimed, 0, 1)) {
         scratch = candidate;
      }
   }
   if (!scratch) {
      count_lost(session, LOSS_SLOTS);
      return 0;
   }
   scratch->head.session = session;
   scratch->head.kind = kind;
   scratch->head.tgid = tgid;
   scratch->head.detail = detail;
   scratch->head.base = 0;
   scratch->head.name = 0;
   scratch->head.args = 0;
   scratch->head.reserved = 0;
   scratch->deep_dentry = 0;
   scratch->deep_mount = 0;
   return scratch;
}

INLINE void drop(struct scratch* scratch) {
   scratch->claimed = 0;
}

INLINE void finish(struct scratch* scratch) {
   __u64 size =
      sizeof(struct record) + scratch->head.base + scratch->head.name + scratch->head.args;
   if (size > sizeof(struct record) + sizeof(scratch->data)) {
      count_lost(scratch->head.session, LOSS_OVERSIZE);
   } else if (bpf_ringbuf_output(&events, scratch, size, 0)) {
      count_lost(scratch->head.session, LOSS_RING);
   }
   drop(scratch);
}

INLINE void escape(__u64 session, __u32 tgid, __u32 why) {
   struct scratch* scratch = begin(session, KIND_ESCAPE, tgid, why);
   if (scratch) {
      finish(scratch);
   }
}

/* The mount a vfsmount is embedded in. */
INLINE struct mount* mount_of(struct vfsmount* vfsmount) {
   return (struct mount*)((char*)vfsmount -
                          __builtin_preserve_field_info(((struct mount*)0)->mnt, 0));
}

struct walking {
   struct scratch* scratch;
   struct dentry* dentry;
   struct vfsmount* vfsmount;
   struct dentry* root_dentry;
   struct vfsmount* root_mount;
   __u32 at;
   /* Reached the task's root or the top of its mount tree. */
   __u32 done;
   __u32 counting;
   __u32 count;
   __u32 skip;
};

/* One component of a walk, 1 to stop. A callback, so the verifier checks one
 * step rather than every unrolled one. */
static long step(__u32 index, struct walking* walking) {
   struct dentry* dentry = walking->dentry;
   struct vfsmount* vfsmount = walking->vfsmount;
   if (dentry == walking->root_dentry && vfsmount == walking->root_mount) {
      walking->done = 1;
      return 1;
   }
   struct dentry* mount_root = 0;
   struct dentry* parent = 0;
   READ(mount_root, vfsmount->mnt_root);
   READ(parent, dentry->d_parent);
   if (dentry == mount_root || dentry == parent) {
      struct mount* mount = mount_of(vfsmount);
      struct mount* above = 0;
      READ(above, mount->mnt_parent);
      if (dentry != mount_root || above == mount) {
         walking->done = 1;
         return 1;
      }
      READ(walking->dentry, mount->mnt_mountpoint);
      walking->vfsmount = (struct vfsmount*)((char*)above + __builtin_preserve_field_info(
                                                               ((struct mount*)0)->mnt, 0));
      return 0;
   }
   if (walking->counting) {
      walking->count++;
      walking->dentry = parent;
      return 0;
   }
   if (walking->skip) {
      walking->skip--;
      walking->dentry = parent;
      return 0;
   }
   __u32 length = 0;
   const unsigned char* name = 0;
   READ(length, dentry->d_name.len);
   READ(name, dentry->d_name.name);
   length &= 255;
   __u32 at = walking->at;
   if (2 * FIELD - at + length + 1 >= FIELD) {
      return 1;
   }
   at -= length;
   bpf_probe_read_kernel(&walking->scratch->walk[at & (2 * FIELD - 1)], length, name);
   at -= 1;
   walking->scratch->walk[at & (2 * FIELD - 1)] = '/';
   walking->at = at;
   walking->dentry = parent;
   return 0;
}

/* Writes `where` as the task sees it, from its root down, into the field the
 * record's base starts at, returning its length or a negative number when it
 * does not fit. Components are laid down backwards into `walk` first, since
 * a walk only learns them from the leaf up. */
INLINE long walk_from(struct scratch* scratch, struct dentry* dentry, struct vfsmount* vfsmount,
                      __u32 skip) {
   struct task_struct* task = bpf_get_current_task_btf();
   struct fs_struct* fs = 0;
   struct walking walking = {
      .scratch = scratch,
      .dentry = dentry,
      .vfsmount = vfsmount,
      .at = 2 * FIELD,
      .skip = skip,
   };
   READ(fs, task->fs);
   if (!fs) {
      return -1;
   }
   READ(walking.root_dentry, fs->root.dentry);
   READ(walking.root_mount, fs->root.mnt);
   if (bpf_loop(skip ? DEEPEST : DEPTH, step, &walking, 0) < 0 || !walking.done) {
      scratch->deep_dentry = dentry;
      scratch->deep_mount = vfsmount;
      return -1;
   }
   __u32 at = walking.at;
   if (at == 2 * FIELD) {
      at -= 1;
      scratch->walk[at & (2 * FIELD - 1)] = '/';
   }
   __u32 length = 2 * FIELD - at;
   bpf_probe_read_kernel(&scratch->data[0], length & MASK, &scratch->walk[at & (2 * FIELD - 1)]);
   return length & MASK;
}

INLINE long walk(struct scratch* scratch, struct dentry* dentry, struct vfsmount* vfsmount) {
   return walk_from(scratch, dentry, vfsmount, 0);
}

/* How many components lie between a dentry and the task's root, 0 when the
 * walk does not reach it. */
INLINE __u32 depth(struct scratch* scratch, struct dentry* dentry, struct vfsmount* vfsmount) {
   struct task_struct* task = bpf_get_current_task_btf();
   struct fs_struct* fs = 0;
   struct walking walking = {
      .scratch = scratch,
      .dentry = dentry,
      .vfsmount = vfsmount,
      .counting = 1,
   };
   READ(fs, task->fs);
   if (!fs) {
      return 0;
   }
   READ(walking.root_dentry, fs->root.dentry);
   READ(walking.root_mount, fs->root.mnt);
   if (bpf_loop(DEEPEST, step, &walking, 0) < 0 || !walking.done) {
      return 0;
   }
   return walking.count;
}

INLINE long walk_path(struct scratch* scratch, struct path* path) {
   struct dentry* dentry = 0;
   struct vfsmount* vfsmount = 0;
   READ(dentry, path->dentry);
   READ(vfsmount, path->mnt);
   if (!dentry || !vfsmount) {
      return -1;
   }
   return walk(scratch, dentry, vfsmount);
}

/* The directory `dfd` names for the current task, its working directory for
 * AT_FDCWD, into the record's base. */
INLINE long base_of(struct scratch* scratch, int dfd) {
   struct task_struct* task = bpf_get_current_task_btf();
   if (dfd == AT_FDCWD) {
      struct fs_struct* fs = 0;
      READ(fs, task->fs);
      if (!fs) {
         return -1;
      }
      return walk_path(scratch, &fs->pwd);
   }
   if (dfd < 0) {
      return NO_DIRECTORY;
   }
   struct files_struct* files = 0;
   struct fdtable* table = 0;
   struct file** fds = 0;
   struct file* file = 0;
   unsigned int max = 0;
   READ(files, task->files);
   if (!files) {
      return -1;
   }
   READ(table, files->fdt);
   READ(max, table->max_fds);
   if ((unsigned int)dfd >= max) {
      return NO_DIRECTORY;
   }
   READ(fds, table->fd);
   bpf_probe_read_kernel(&file, sizeof(file), &fds[dfd]);
   if (!file) {
      return NO_DIRECTORY;
   }
   return walk_path(scratch, &file->f_path);
}

/* A path too deep or long to spell out, sent as the directory its first
 * PREFIX components from the root name, so the client can tell whether the
 * run made it. */
INLINE void deep(__u64 session, __u32 tgid, struct scratch* scratch) {
   struct dentry* dentry = scratch->deep_dentry;
   struct vfsmount* vfsmount = scratch->deep_mount;
   __u32 count = dentry ? depth(scratch, dentry, vfsmount) : 0;
   long length = count > PREFIX ? walk_from(scratch, dentry, vfsmount, count - PREFIX) : -1;
   if (length < 0) {
      drop(scratch);
      escape(session, tgid, ESCAPE_UNRESOLVED);
      return;
   }
   scratch->head.kind = KIND_DEEP;
   scratch->head.detail = 0;
   scratch->head.base = 0;
   scratch->head.name = length;
   scratch->head.args = 0;
   finish(scratch);
}

/* A path as a call named it: `name` is the kernel's copy of it, whose string
 * pointer leads struct filename in every layout. A relative name brings the
 * directory it resolves against. */
INLINE void named(__u32 kind, __u32 detail, int dfd, const void* filename) {
   __u32 tgid = bpf_get_current_pid_tgid() >> 32;
   __u64* session = session_of(tgid);
   if (!session || !filename) {
      return;
   }
   const char* string = 0;
   char first = 0;
   bpf_probe_read_kernel(&string, sizeof(string), filename);
   if (!string) {
      return;
   }
   bpf_probe_read_kernel(&first, 1, string);
   struct scratch* scratch = begin(*session, kind, tgid, detail);
   if (!scratch) {
      return;
   }
   if (first != '/') {
      long base = base_of(scratch, dfd);
      if (base == NO_DIRECTORY) {
         drop(scratch);
         return;
      }
      if (base < 0) {
         deep(*session, tgid, scratch);
         return;
      }
      scratch->head.base = base;
   }
   __u32 offset = scratch->head.base & MASK;
   long copied = bpf_probe_read_kernel_str(&scratch->data[offset], FIELD, string);
   if (copied <= 0) {
      drop(scratch);
      return;
   }
   scratch->head.name = (copied - 1) & MASK;
   finish(scratch);
}

INLINE void resolved(__u32 kind, __u32 detail, struct path* path) {
   __u32 tgid = bpf_get_current_pid_tgid() >> 32;
   __u64* session = session_of(tgid);
   if (!session) {
      return;
   }
   struct scratch* scratch = begin(*session, kind, tgid, detail);
   if (!scratch) {
      return;
   }
   long length = walk_path(scratch, path);
   if (length < 0) {
      deep(*session, tgid, scratch);
      return;
   }
   scratch->head.name = length;
   finish(scratch);
}

/* A failed open resolved in a descriptor's root, which never leaves the
 * directory, so it is sent as reaching paths under it, as a deep one is. */
INLINE void rooted(int dfd) {
   __u32 tgid = bpf_get_current_pid_tgid() >> 32;
   __u64* session = session_of(tgid);
   if (!session) {
      return;
   }
   struct scratch* scratch = begin(*session, KIND_DEEP, tgid, 0);
   if (!scratch) {
      return;
   }
   long length = base_of(scratch, dfd);
   if (length == NO_DIRECTORY) {
      drop(scratch);
      return;
   }
   if (length < 0) {
      deep(*session, tgid, scratch);
      return;
   }
   scratch->head.name = length;
   finish(scratch);
}

/* The calls that can take a path away run as fexit programs, so one that
 * failed, as tests of error paths make them, counts only as the lookup it
 * was. The names stay valid until the system call that passed them returns. */
SEC("fexit/do_file_open")
int open(__u64* context) {
   const struct open_flags* flags = (void*)context[2];
   unsigned long file = context[3];
   int open_flag = 0;
   int lookup_flags = 0;
   READ(open_flag, flags->open_flag);
   READ(lookup_flags, flags->lookup_flags);
   if (lookup_flags & LOOKUP_IN_ROOT) {
      if (file < (unsigned long)-MAX_ERRNO) {
         resolved(KIND_OPEN, open_flag, &((struct file*)file)->f_path);
      } else {
         rooted((int)context[0]);
      }
      return 0;
   }
   if (file >= (unsigned long)-MAX_ERRNO) {
      open_flag &= ~(O_ACCMODE | O_TRUNC);
   }
   named(KIND_OPEN, open_flag, (int)context[0], (void*)context[1]);
   return 0;
}

SEC("fentry/filename_lookup")
int lookup(__u64* context) {
   named(KIND_LOOKUP, 0, (int)context[0], (void*)context[1]);
   return 0;
}

SEC("fexit/filename_unlinkat")
int unlink(__u64* context) {
   __u32 kind = (int)context[2] ? KIND_LOOKUP : KIND_CLOBBER;
   named(kind, 0, (int)context[0], (void*)context[1]);
   return 0;
}

SEC("fentry/filename_rmdir")
int rmdir(__u64* context) {
   named(KIND_LOOKUP, 0, (int)context[0], (void*)context[1]);
   return 0;
}

SEC("fentry/filename_mkdirat")
int mkdir(__u64* context) {
   named(KIND_LOOKUP, 0, (int)context[0], (void*)context[1]);
   return 0;
}

SEC("fentry/filename_mknodat")
int mknod(__u64* context) {
   named(KIND_LOOKUP, 0, (int)context[0], (void*)context[1]);
   return 0;
}

SEC("fentry/filename_symlinkat")
int symlink(__u64* context) {
   named(KIND_LOOKUP, 0, (int)context[1], (void*)context[2]);
   return 0;
}

SEC("fentry/filename_linkat")
int link(__u64* context) {
   named(KIND_LOOKUP, 0, (int)context[0], (void*)context[1]);
   named(KIND_LOOKUP, 0, (int)context[2], (void*)context[3]);
   return 0;
}

SEC("fexit/filename_renameat2")
int rename(__u64* context) {
   __u32 kind = (int)context[5] ? KIND_LOOKUP : KIND_CLOBBER;
   named(kind, 0, (int)context[0], (void*)context[1]);
   named(kind, 0, (int)context[2], (void*)context[3]);
   return 0;
}

SEC("fexit/vfs_truncate")
int truncate(__u64* context) {
   resolved((long)context[2] ? KIND_LOOKUP : KIND_CLOBBER, 0, (struct path*)context[0]);
   return 0;
}

SEC("fentry/iterate_dir")
int listing(__u64* context) {
   struct file* file = (void*)context[0];
   resolved(KIND_LISTING, 0, &file->f_path);
   return 0;
}

SEC("tp_btf/sched_process_fork")
int fork(__u64* context) {
   struct task_struct* parent = (void*)context[0];
   struct task_struct* child = (void*)context[1];
   int parent_tgid = 0;
   int child_pid = 0;
   int child_tgid = 0;
   READ(parent_tgid, parent->tgid);
   READ(child_pid, child->pid);
   READ(child_tgid, child->tgid);
   __u32 key = parent_tgid;
   __u64* session = session_of(key);
   if (!session || child_pid != child_tgid) {
      return 0;
   }
   __u64 value = *session;
   __u32 child_key = child_tgid;
   if (bpf_map_update_elem(&traced, &child_key, &value, 0)) {
      count_lost(value, LOSS_FORKS);
      return 0;
   }
   struct scratch* scratch = begin(value, KIND_FORK, key, child_key);
   if (scratch) {
      finish(scratch);
   }
   return 0;
}

SEC("tp_btf/sched_process_exec")
int exec(__u64* context) {
   struct task_struct* task = (void*)context[0];
   struct linux_binprm* program = (void*)context[2];
   int tgid = 0;
   READ(tgid, task->tgid);
   __u32 key = tgid;
   __u64* session = session_of(key);
   if (!session) {
      return 0;
   }
   const char* filename = 0;
   READ(filename, program->filename);
   if (!filename) {
      return 0;
   }
   struct scratch* scratch = begin(*session, KIND_EXEC, key, 0);
   if (!scratch) {
      return 0;
   }
   struct fs_struct* fs = 0;
   READ(fs, task->fs);
   long base = fs ? walk_path(scratch, &fs->pwd) : -1;
   if (base < 0) {
      deep(*session, key, scratch);
      return 0;
   }
   scratch->head.base = base;
   __u32 offset = base & MASK;
   long copied = bpf_probe_read_kernel_str(&scratch->data[offset], FIELD, filename);
   if (copied <= 0) {
      drop(scratch);
      return 0;
   }
   scratch->head.name = (copied - 1) & MASK;
   struct mm_struct* mm = 0;
   unsigned long start = 0;
   unsigned long stop = 0;
   READ(mm, task->mm);
   if (mm) {
      READ(start, mm->arg_start);
      READ(stop, mm->arg_end);
   }
   if (stop > start) {
      if (stop - start >= FIELD) {
         start = stop - MASK;
         scratch->head.detail = EXEC_TAIL;
      }
      __u32 length = stop - start;
      __u32 at = (scratch->head.base + scratch->head.name) & (2 * FIELD - 1);
      if (!bpf_probe_read_user(&scratch->data[at], length & MASK, (void*)start)) {
         scratch->head.args = length & MASK;
      }
   }
   finish(scratch);
   return 0;
}

SEC("tp_btf/sched_process_free")
int reaped(__u64* context) {
   struct task_struct* task = (void*)context[0];
   int pid = 0;
   int tgid = 0;
   READ(pid, task->pid);
   READ(tgid, task->tgid);
   if (pid != tgid) {
      return 0;
   }
   __u32 key = tgid;
   bpf_map_delete_elem(&traced, &key);
   return 0;
}

SEC("tp_btf/sys_enter")
int syscall(__u64* context) {
   __u32 tgid = bpf_get_current_pid_tgid() >> 32;
   __u64* session = session_of(tgid);
   if (!session) {
      return 0;
   }
   struct pt_regs* regs = (void*)context[0];
   long id = (long)context[1];
   if (foreign(regs, id)) {
      escape(*session, tgid, ESCAPE_FOREIGN);
      return 0;
   }
   __u32 number = id;
   if (bpf_map_lookup_elem(&escapes, &number)) {
      escape(*session, tgid, number);
   }
   return 0;
}

char LICENSE[] SEC("license") = "GPL";
