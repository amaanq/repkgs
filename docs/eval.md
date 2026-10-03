# Evaluation cost

`bench/eval.nu` instantiates the packages motley and the pinned nixpkgs both have and reads what
Nix's evaluator reports about itself through `NIX_SHOW_STATS`, the CPU time and the bytes the
garbage collector handed out.

```console
$ bench/eval.nu
$ bench/eval.nu --platform aarch64-linux --runs 9 --json
```

A package counts when motley supports it on the platform and nixpkgs has a derivation of the same
name. A few names differ between the sets, so `cpython314` pairs with `python314` and `llvm`,
`clang` and `lld` with their `llvmPackages_23` attributes. Each set is instantiated twice, once
with every paired package and once with only the first, and each run repeats `--runs` times with
the median kept. The first package pays for what every package shares, motley's bootstrap or
nixpkgs' stdenv, so the difference between the two runs divided by one less than the package count
is what one more package costs.

## 2026-10-02, x86_64-linux, 144 packages

| set     | total time   | total allocated | time per package | allocated per package |
| ------- | ------------ | --------------- | ---------------- | --------------------- |
| motley  | 1.2 to 1.6 s | 420 MB          | 2.4 to 3.7 ms    | 945 kB                |
| nixpkgs | 1.9 to 3.3 s | 498 MB          | 12 to 20 ms      | 2991 kB               |

A package costs motley about a third of the memory and a fifth to a quarter of the time it costs
nixpkgs. Allocation came out identical on every run. The times are spreads across runs on a
32-thread machine that was building at a load of about 35, so they want repeating on an idle one.
