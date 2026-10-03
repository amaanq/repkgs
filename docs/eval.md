# Evaluation cost

`bench/eval.nu` instantiates the packages motley and the pinned nixpkgs both have and reads what
Nix itself counts (`NIX_SHOW_STATS`): CPU seconds and bytes allocated. Each set gets one
`nix-instantiate` over all of its paired packages and one over the first of them alone. The
difference, divided by the remaining packages, is the cost a package adds once the set's fixed
cost (the bootstrap here, stdenv there) is paid.

```console
$ bench/eval.nu                      # x86_64-linux, median of 5 runs
$ bench/eval.nu --platform aarch64-linux --runs 9 --json
```

A package pairs with the nixpkgs attribute of the same name, or the one the script's `renames`
names (`cpython314` and `python314`, llvm's tools under `llvmPackages_23`).

## 2026-10-02, x86_64-linux, 144 packages

| | total | per package |
|---|---|---|
| motley | 420 MB, 1.2 to 1.6 s | 945 KB, 2.4 to 3.7 ms |
| nixpkgs | 498 MB, 1.9 to 3.3 s | 2991 KB, 12 to 20 ms |

Allocation is deterministic and the same on every run. The times were taken on a 32-thread machine
running builds at a load of 35, so they are ranges over runs and want repeating on an idle one.
