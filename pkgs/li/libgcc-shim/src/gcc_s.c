// The libgcc_s.so.1 that upstream binaries such as rustc and node expect, on a runtime without gcc.
// libunwind.a, linked whole and versioned by gcc_s.ver, provides the _Unwind_* ABI. compiler-rt
// hides its builtins, so the few libgcc integer routines those binaries import live here.

int __popcountdi2(long long value) {
   return __builtin_popcountll((unsigned long long)value);
}

int __popcountti2(__int128 value) {
   return __builtin_popcountll((unsigned long long)value) +
          __builtin_popcountll((unsigned long long)(value >> 64));
}

int __clzdi2(long long value) {
   return __builtin_clzll((unsigned long long)value);
}

int __ctzdi2(long long value) {
   return __builtin_ctzll((unsigned long long)value);
}
