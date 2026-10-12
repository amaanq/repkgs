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
#define __uint(name, value) int(*name)[value]
#define __type(name, value) typeof(value) *name

#define AT_FDCWD -100
#define LOOKUP_IN_ROOT (1U << 28)

enum {
   BPF_MAP_TYPE_HASH = 1,
   BPF_MAP_TYPE_PERCPU_ARRAY = 6,
   BPF_MAP_TYPE_RINGBUF = 27,
};

static void *(*bpf_map_lookup_elem)(void *map, const void *key) = (void *)1;
static long (*bpf_map_update_elem)(void *map, const void *key, const void *value, __u64 flags) = (void *)2;
static long (*bpf_map_delete_elem)(void *map, const void *key) = (void *)3;
static __u64 (*bpf_get_current_pid_tgid)(void) = (void *)14;
static long (*bpf_probe_read_user)(void *dst, __u32 size, const void *src) = (void *)112;
static long (*bpf_probe_read_kernel)(void *dst, __u32 size, const void *src) = (void *)113;
static long (*bpf_probe_read_kernel_str)(void *dst, __u32 size, const void *src) = (void *)115;
static long (*bpf_ringbuf_output)(void *ringbuf, void *data, __u64 size, __u64 flags) = (void *)130;
static struct task_struct *(*bpf_get_current_task_btf)(void) = (void *)158;
static long (*bpf_loop)(__u32 count, void *callback, void *context, __u64 flags) = (void *)181;

struct qstr {
   __u32 len;
   const unsigned char *name;
} CORE;

struct dentry {
   struct dentry *d_parent;
   struct qstr d_name;
} CORE;

struct vfsmount {
   struct dentry *mnt_root;
} CORE;

struct mount {
   struct mount *mnt_parent;
   struct dentry *mnt_mountpoint;
   struct vfsmount mnt;
} CORE;

struct path {
   struct vfsmount *mnt;
   struct dentry *dentry;
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
   struct file **fd;
} CORE;

struct files_struct {
   struct fdtable *fdt;
} CORE;

struct mm_struct {
   unsigned long arg_start;
   unsigned long arg_end;
} CORE;

struct task_struct {
   int pid;
   int tgid;
   struct mm_struct *mm;
   struct fs_struct *fs;
   struct files_struct *files;
} CORE;

struct open_flags {
   int open_flag;
   int lookup_flags;
} CORE;

struct linux_binprm {
   const char *filename;
} CORE;

#if defined(__TARGET_ARCH_x86)
struct pt_regs {
   unsigned long cs;
} CORE;
#define USER32_CS 0x23
#define X32_BIT 0x40000000
INLINE int foreign(struct pt_regs *regs, long id) {
   unsigned long cs = 0;
   bpf_probe_read_kernel(&cs, sizeof(cs), __builtin_preserve_access_index(&regs->cs));
   return cs == USER32_CS || (id & X32_BIT);
}
#elif defined(__TARGET_ARCH_arm64)
struct pt_regs {
   __u64 pstate;
} CORE;
#define PSR_MODE32_BIT 0x10
INLINE int foreign(struct pt_regs *regs, long id) {
   __u64 pstate = 0;
   bpf_probe_read_kernel(&pstate, sizeof(pstate), __builtin_preserve_access_index(&regs->pstate));
   return (pstate & PSR_MODE32_BIT) != 0;
}
#else
#error "ringmasterd knows x86 and arm64 compat calls only"
#endif

#define READ(destination, source)                                                                  \
   bpf_probe_read_kernel(&(destination), sizeof(destination), __builtin_preserve_access_index(&(source)))

enum kind {
   KIND_OPEN = 1,
   KIND_LOOKUP = 2,
   KIND_CLOBBER = 3,
   KIND_LISTING = 4,
   KIND_EXEC = 5,
   KIND_FORK = 6,
   KIND_ESCAPE = 7,
};

/* Escapes that are no system call. */
#define ESCAPE_IN_ROOT 0xFFFF0001U
#define ESCAPE_FOREIGN 0xFFFF0002U
#define ESCAPE_UNRESOLVED 0xFFFF0003U

#define FIELD 4096
#define MASK (FIELD - 1)
#define DEPTH 128

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
};

struct {
   __uint(type, BPF_MAP_TYPE_HASH);
   __uint(max_entries, 1 << 16);
   __type(key, __u32);
   __type(value, __u64);
} traced SEC(".maps");

struct {
   __uint(type, BPF_MAP_TYPE_HASH);
   __uint(max_entries, 1 << 10);
   __type(key, __u64);
   __type(value, __u64);
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

struct {
   __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
   __uint(max_entries, 1);
   __type(key, __u32);
   __type(value, struct scratch);
} scratches SEC(".maps");

INLINE __u64 *session_of(__u32 tgid) { return bpf_map_lookup_elem(&traced, &tgid); }

INLINE struct scratch *begin(__u64 session, __u32 kind, __u32 tgid, __u32 detail) {
   __u32 zero = 0;
   struct scratch *scratch = bpf_map_lookup_elem(&scratches, &zero);
   if (!scratch)
      return 0;
   scratch->head.session = session;
   scratch->head.kind = kind;
   scratch->head.tgid = tgid;
   scratch->head.detail = detail;
   scratch->head.base = 0;
   scratch->head.name = 0;
   scratch->head.args = 0;
   scratch->head.reserved = 0;
   return scratch;
}

INLINE void count_lost(__u64 session) {
   __u64 one = 1;
   __u64 *count = bpf_map_lookup_elem(&lost, &session);
   if (count)
      __sync_fetch_and_add(count, 1);
   else
      bpf_map_update_elem(&lost, &session, &one, 0);
}

INLINE void finish(struct scratch *scratch) {
   __u64 size = sizeof(struct record) + scratch->head.base + scratch->head.name + scratch->head.args;
   if (size > sizeof(struct record) + sizeof(scratch->data)) {
      count_lost(scratch->head.session);
      return;
   }
   if (bpf_ringbuf_output(&events, scratch, size, 0))
      count_lost(scratch->head.session);
}

INLINE void escape(__u64 session, __u32 tgid, __u32 why) {
   struct scratch *scratch = begin(session, KIND_ESCAPE, tgid, why);
   if (scratch)
      finish(scratch);
}

/* The mount a vfsmount is embedded in. */
INLINE struct mount *mount_of(struct vfsmount *vfsmount) {
   return (struct mount *)((char *)vfsmount -
                           __builtin_preserve_field_info(((struct mount *)0)->mnt, 0));
}

struct walking {
   struct scratch *scratch;
   struct dentry *dentry;
   struct vfsmount *vfsmount;
   struct dentry *root_dentry;
   struct vfsmount *root_mount;
   __u32 at;
   /* Reached the task's root or the top of its mount tree. */
   __u32 done;
};

/* One component of a walk, 1 to stop. A callback, so the verifier checks one
 * step rather than every unrolled one. */
static long step(__u32 index, struct walking *walking) {
   struct dentry *dentry = walking->dentry;
   struct vfsmount *vfsmount = walking->vfsmount;
   if (dentry == walking->root_dentry && vfsmount == walking->root_mount) {
      walking->done = 1;
      return 1;
   }
   struct dentry *mount_root = 0;
   struct dentry *parent = 0;
   READ(mount_root, vfsmount->mnt_root);
   READ(parent, dentry->d_parent);
   if (dentry == mount_root || dentry == parent) {
      struct mount *mount = mount_of(vfsmount);
      struct mount *above = 0;
      READ(above, mount->mnt_parent);
      if (dentry != mount_root || above == mount) {
         walking->done = 1;
         return 1;
      }
      READ(walking->dentry, mount->mnt_mountpoint);
      walking->vfsmount = (struct vfsmount *)((char *)above +
                                              __builtin_preserve_field_info(((struct mount *)0)->mnt, 0));
      return 0;
   }
   __u32 length = 0;
   const unsigned char *name = 0;
   READ(length, dentry->d_name.len);
   READ(name, dentry->d_name.name);
   length &= 255;
   __u32 at = walking->at;
   if (2 * FIELD - at + length + 1 >= FIELD)
      return 1;
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
INLINE long walk(struct scratch *scratch, struct dentry *dentry, struct vfsmount *vfsmount) {
   struct task_struct *task = bpf_get_current_task_btf();
   struct fs_struct *fs = 0;
   struct walking walking = {
      .scratch = scratch,
      .dentry = dentry,
      .vfsmount = vfsmount,
      .at = 2 * FIELD,
   };
   READ(fs, task->fs);
   if (!fs)
      return -1;
   READ(walking.root_dentry, fs->root.dentry);
   READ(walking.root_mount, fs->root.mnt);
   if (bpf_loop(DEPTH, step, &walking, 0) < 0 || !walking.done)
      return -1;
   __u32 at = walking.at;
   if (at == 2 * FIELD) {
      at -= 1;
      scratch->walk[at & (2 * FIELD - 1)] = '/';
   }
   __u32 length = 2 * FIELD - at;
   bpf_probe_read_kernel(&scratch->data[0], length & MASK, &scratch->walk[at & (2 * FIELD - 1)]);
   return length & MASK;
}

INLINE long walk_path(struct scratch *scratch, struct path *path) {
   struct dentry *dentry = 0;
   struct vfsmount *vfsmount = 0;
   READ(dentry, path->dentry);
   READ(vfsmount, path->mnt);
   if (!dentry || !vfsmount)
      return -1;
   return walk(scratch, dentry, vfsmount);
}

/* The directory `dfd` names for the current task, its working directory for
 * AT_FDCWD, into the record's base. */
INLINE long base_of(struct scratch *scratch, int dfd) {
   struct task_struct *task = bpf_get_current_task_btf();
   if (dfd == AT_FDCWD) {
      struct fs_struct *fs = 0;
      READ(fs, task->fs);
      if (!fs)
         return -1;
      return walk_path(scratch, &fs->pwd);
   }
   if (dfd < 0)
      return -1;
   struct files_struct *files = 0;
   struct fdtable *table = 0;
   struct file **fds = 0;
   struct file *file = 0;
   unsigned int max = 0;
   READ(files, task->files);
   if (!files)
      return -1;
   READ(table, files->fdt);
   READ(max, table->max_fds);
   if ((unsigned int)dfd >= max)
      return -1;
   READ(fds, table->fd);
   bpf_probe_read_kernel(&file, sizeof(file), &fds[dfd]);
   if (!file)
      return -1;
   return walk_path(scratch, &file->f_path);
}

/* A path as a call named it: `name` is the kernel's copy of it, whose string
 * pointer leads struct filename in every layout. A relative name brings the
 * directory it resolves against. */
INLINE void named(__u32 kind, __u32 detail, int dfd, const void *filename) {
   __u32 tgid = bpf_get_current_pid_tgid() >> 32;
   __u64 *session = session_of(tgid);
   if (!session || !filename)
      return;
   const char *string = 0;
   char first = 0;
   bpf_probe_read_kernel(&string, sizeof(string), filename);
   if (!string)
      return;
   bpf_probe_read_kernel(&first, 1, string);
   struct scratch *scratch = begin(*session, kind, tgid, detail);
   if (!scratch)
      return;
   if (first != '/') {
      long base = base_of(scratch, dfd);
      if (base < 0) {
         escape(*session, tgid, ESCAPE_UNRESOLVED);
         return;
      }
      scratch->head.base = base;
   }
   __u32 offset = scratch->head.base & MASK;
   long copied = bpf_probe_read_kernel_str(&scratch->data[offset], FIELD, string);
   if (copied <= 0)
      return;
   scratch->head.name = (copied - 1) & MASK;
   finish(scratch);
}

INLINE void resolved(__u32 kind, struct path *path) {
   __u32 tgid = bpf_get_current_pid_tgid() >> 32;
   __u64 *session = session_of(tgid);
   if (!session)
      return;
   struct scratch *scratch = begin(*session, kind, tgid, 0);
   if (!scratch)
      return;
   long length = walk_path(scratch, path);
   if (length < 0) {
      escape(*session, tgid, ESCAPE_UNRESOLVED);
      return;
   }
   scratch->head.name = length;
   finish(scratch);
}

SEC("fentry/do_file_open")
int open(__u64 *context) {
   const struct open_flags *flags = (void *)context[2];
   int open_flag = 0;
   int lookup_flags = 0;
   READ(open_flag, flags->open_flag);
   READ(lookup_flags, flags->lookup_flags);
   if (lookup_flags & LOOKUP_IN_ROOT) {
      __u32 tgid = bpf_get_current_pid_tgid() >> 32;
      __u64 *session = session_of(tgid);
      if (session)
         escape(*session, tgid, ESCAPE_IN_ROOT);
      return 0;
   }
   named(KIND_OPEN, open_flag, (int)context[0], (void *)context[1]);
   return 0;
}

SEC("fentry/filename_lookup")
int lookup(__u64 *context) {
   named(KIND_LOOKUP, 0, (int)context[0], (void *)context[1]);
   return 0;
}

SEC("fentry/filename_unlinkat")
int unlink(__u64 *context) {
   named(KIND_CLOBBER, 0, (int)context[0], (void *)context[1]);
   return 0;
}

SEC("fentry/filename_rmdir")
int rmdir(__u64 *context) {
   named(KIND_LOOKUP, 0, (int)context[0], (void *)context[1]);
   return 0;
}

SEC("fentry/filename_mkdirat")
int mkdir(__u64 *context) {
   named(KIND_LOOKUP, 0, (int)context[0], (void *)context[1]);
   return 0;
}

SEC("fentry/filename_mknodat")
int mknod(__u64 *context) {
   named(KIND_LOOKUP, 0, (int)context[0], (void *)context[1]);
   return 0;
}

SEC("fentry/filename_symlinkat")
int symlink(__u64 *context) {
   named(KIND_LOOKUP, 0, (int)context[1], (void *)context[2]);
   return 0;
}

SEC("fentry/filename_linkat")
int link(__u64 *context) {
   named(KIND_LOOKUP, 0, (int)context[0], (void *)context[1]);
   named(KIND_LOOKUP, 0, (int)context[2], (void *)context[3]);
   return 0;
}

SEC("fentry/filename_renameat2")
int rename(__u64 *context) {
   named(KIND_CLOBBER, 0, (int)context[0], (void *)context[1]);
   named(KIND_CLOBBER, 0, (int)context[2], (void *)context[3]);
   return 0;
}

SEC("fentry/vfs_truncate")
int truncate(__u64 *context) {
   resolved(KIND_CLOBBER, (struct path *)context[0]);
   return 0;
}

SEC("fentry/iterate_dir")
int listing(__u64 *context) {
   struct file *file = (void *)context[0];
   resolved(KIND_LISTING, &file->f_path);
   return 0;
}

SEC("tp_btf/sched_process_fork")
int fork(__u64 *context) {
   struct task_struct *parent = (void *)context[0];
   struct task_struct *child = (void *)context[1];
   int parent_tgid = 0;
   int child_pid = 0;
   int child_tgid = 0;
   READ(parent_tgid, parent->tgid);
   READ(child_pid, child->pid);
   READ(child_tgid, child->tgid);
   __u32 key = parent_tgid;
   __u64 *session = session_of(key);
   if (!session || child_pid != child_tgid)
      return 0;
   __u64 value = *session;
   __u32 child_key = child_tgid;
   if (bpf_map_update_elem(&traced, &child_key, &value, 0)) {
      count_lost(value);
      return 0;
   }
   struct scratch *scratch = begin(value, KIND_FORK, key, child_key);
   if (scratch)
      finish(scratch);
   return 0;
}

SEC("tp_btf/sched_process_exec")
int exec(__u64 *context) {
   struct task_struct *task = (void *)context[0];
   struct linux_binprm *program = (void *)context[2];
   int tgid = 0;
   READ(tgid, task->tgid);
   __u32 key = tgid;
   __u64 *session = session_of(key);
   if (!session)
      return 0;
   const char *filename = 0;
   READ(filename, program->filename);
   struct scratch *scratch = begin(*session, KIND_EXEC, key, 0);
   if (!scratch || !filename)
      return 0;
   struct fs_struct *fs = 0;
   READ(fs, task->fs);
   long base = fs ? walk_path(scratch, &fs->pwd) : -1;
   if (base < 0) {
      escape(*session, key, ESCAPE_UNRESOLVED);
      return 0;
   }
   scratch->head.base = base;
   __u32 offset = base & MASK;
   long copied = bpf_probe_read_kernel_str(&scratch->data[offset], FIELD, filename);
   if (copied <= 0)
      return 0;
   scratch->head.name = (copied - 1) & MASK;
   struct mm_struct *mm = 0;
   unsigned long start = 0;
   unsigned long stop = 0;
   READ(mm, task->mm);
   if (mm) {
      READ(start, mm->arg_start);
      READ(stop, mm->arg_end);
   }
   if (stop > start) {
      __u32 length = stop - start;
      if (length >= FIELD) {
         escape(*session, key, ESCAPE_UNRESOLVED);
         return 0;
      }
      __u32 at = (scratch->head.base + scratch->head.name) & (2 * FIELD - 1);
      if (!bpf_probe_read_user(&scratch->data[at], length & MASK, (void *)start))
         scratch->head.args = length & MASK;
   }
   finish(scratch);
   return 0;
}

SEC("tp_btf/sched_process_free")
int reaped(__u64 *context) {
   struct task_struct *task = (void *)context[0];
   int pid = 0;
   int tgid = 0;
   READ(pid, task->pid);
   READ(tgid, task->tgid);
   if (pid != tgid)
      return 0;
   __u32 key = tgid;
   bpf_map_delete_elem(&traced, &key);
   return 0;
}

SEC("tp_btf/sys_enter")
int syscall(__u64 *context) {
   __u32 tgid = bpf_get_current_pid_tgid() >> 32;
   __u64 *session = session_of(tgid);
   if (!session)
      return 0;
   struct pt_regs *regs = (void *)context[0];
   long id = (long)context[1];
   if (foreign(regs, id)) {
      escape(*session, tgid, ESCAPE_FOREIGN);
      return 0;
   }
   __u32 number = id;
   if (bpf_map_lookup_elem(&escapes, &number))
      escape(*session, tgid, number);
   return 0;
}

char LICENSE[] SEC("license") = "GPL";
