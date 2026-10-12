# shared by the subcommands: the tree, how nix is called, and where the mummerd socket is

export const SANDBOX_SOCKET = "/nix/var/nix/mummerd/socket"

export const FEATURES = [--extra-experimental-features "nix-command ca-derivations dynamic-derivations"]

const ROOT = path self ../..
export def root []: nothing -> string { $ROOT }

export def platform-arg [platform]: nothing -> list<string> {
   if $platform == null { [] } else { [--argstr platform $platform] }
}

export def peanut-path [peanut: any]: nothing -> any {
   if $peanut == null { return null }
   let path = ($peanut | path expand --strict)
   if not ($path | path join bin/mummer | path exists) { fail $"($path) has no bin/mummer" }
   $path
}

export def peanut-arg [peanut: any]: nothing -> list<string> {
   if $peanut == null { [] } else { [--argstr peanut (peanut-path $peanut)] }
}

export def rust-toolchain-path [toolchain: any]: nothing -> any {
   if $toolchain == null { return null }
   let path = ($toolchain | path expand --strict)
   if not ($path | path join bin/rustc | path exists) { fail $"($path) has no bin/rustc" }
   if not ($path | path join bin/cargo | path exists) { fail $"($path) has no bin/cargo" }
   $path
}

export def rust-toolchain-arg [toolchain: any]: nothing -> list<string> {
   if $toolchain == null { [] } else { [--argstr rustToolchain (rust-toolchain-path $toolchain)] }
}

export def set-expr [p, peanut: any, toolchain: any]: nothing -> string {
   let fields = ({platform: $p, peanut: (peanut-path $peanut), rustToolchain: (rust-toolchain-path $toolchain)} | transpose name value | where value != null
      | each {|field| $"($field.name) = ($field.value | to json -r | str replace -a '${' '\${');" })
   $"\(import (root) { ($fields | str join ' ') }\)"
}

export def fail [message: string]: nothing -> error { error make --unspanned {msg: $message} }

# an expression evaluated to JSON. When nix fails, its last `error:` line names the real problem
# (an unknown package or platform, a broken recipe) where the full trace runs through our query
export def eval [expr: string]: nothing -> any {
   # read-write, since a dynamic derivation has a path only once its .drv is written
   let result = (^nix-instantiate ...$FEATURES --eval --read-write-mode --strict --json --expr $expr | complete)
   if $result.exit_code == 0 { return ($result.stdout | from json) }
   let last = ($result.stderr | lines | where $it =~ '^\s*error:' | last | default $result.stderr)
   fail ($last | str replace -r '^\s*error:\s*' "")
}

# `s.${"7zip"}` parses where s.7zip does not, and a misspelt name lists the packages sharing its
# first two letters instead of a trace through this expression
export def pkg-expr [attr: string, p, peanut: any, toolchain: any]: nothing -> string {
   let quoted = ($attr | to json)
   $"\(let s = (set-expr $p $peanut $toolchain); n = ($quoted); in s.${n} or \(builtins.throw \"no package ${n}, near: ${toString \(builtins.filter \(a: builtins.substring 0 2 a == builtins.substring 0 2 n\) \(builtins.attrNames s\)\)}\"\)\)"
}

# a socket with a listener
export def is-socket [path: string]: nothing -> bool {
   let listening = (open --raw /proc/net/unix | str contains $" ($path | path expand)\n")
   ($path | path exists) and (ls -D $path | get 0.type) == "socket" and $listening
}

# /tmp rather than XDG_RUNTIME_DIR, because the nix build user reaches it through extra-sandbox-paths
export def mummerd-dir []: nothing -> string { $"/tmp/mummerd-(id -u)" }

export def mummerd-socket []: nothing -> any {
   [$env.MUMMER_SOCK? $SANDBOX_SOCKET $"(mummerd-dir)/socket"] | compact | where {|path| is-socket $path } | get 0?
}

# maps the socket into the sandbox, which takes a trusted user, and keeps the build here
export def cache-args []: nothing -> list<string> {
   let socket = (mummerd-socket)
   if $socket == null { fail "no mummerd socket, start the cache with `motley cache start`" }
   if $socket == $SANDBOX_SOCKET { return [--builders ""] }
   [--option extra-sandbox-paths $"($SANDBOX_SOCKET)=($socket)" --builders ""]
}

export def drv [attr: string, p, peanut: any, toolchain: any]: nothing -> string { eval $"(pkg-expr $attr $p $peanut $toolchain).drvPath" }
