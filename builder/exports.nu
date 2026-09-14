# exports.json: what a package offers dependents (include, lib, pkgconfig, aclocal dirs relative
# to its root, libs, env, propagate), read with defaults from the tree and written by finish.
# jig env turns the closure of these into search paths

# a package's exports with defaults filled in. Used for dependencies and for writing our own
export def exports-of [p: path]: nothing -> record<name: string, includeDirs: list<string>, libDirs: list<string>, libs: list<string>, pkgconfigDirs: list<string>, aclocalDirs: list<string>, env: record, propagate: list<string>> {
  ^jig exports $p | from json | first | reject root
}

# dependencies plus everything they `propagate`, breadth first, each once
export def dep-closure [roots: list<string>]: nothing -> list<record> {
  ^jig deps ...$roots | from json
}

# absolute directories of one exports field (libDirs, includeDirs, …) across dependencies
export def dep-dirs [deps: list<record<name: string, root: string>>, field: string]: nothing -> list<string> {
  $deps | each {|d| $d | get $field | each {|rel| $"($d.root)/($rel)" } } | flatten
}

# exports.json: spec.exports over the defaults, exports = false: nothing to link against. Dependencies
# our .pc files (Requires) and cmake configs (find_dependency) name are propagated
export def write-exports [out: string, spec: record, deps: list<record>]: nothing -> nothing {
  {name: $spec.name, exports: $spec.exports?, deps: $deps.root} | to json -r | ^jig write-exports $out
}
