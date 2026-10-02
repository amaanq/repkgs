# Nix layer

Every build system's options now come from the schema, with literal defaults, required
options, nullable options, lists, maps, enums and closed records. A required option has
no default of its own and Nix supplies it from `lock.deps`. Nix still binds tool packages
and lock fetchers. The broader contracts and symbolic defaults below remain proposed work.

## Ownership

Rust owns the recipe vocabulary and build behavior. Nix owns the lazy package graph,
dependency references, overrides and derivations. A schema describes the values crossing
that boundary. It does not turn Nix functions or package references into JSON expressions.

Today `nix/build-systems.nix` repeats the fields Rust deserializes, including defaults
that Rust often requires Nix to insert. Extend the hardening arrangement so `jib schema`
emits those declarations once and Nix reads the committed `nix/schema.json`.
Loading the schema runs no executable, builds nothing and performs no network access.
Bootstrap can read it before jib exists, which avoids a schema-generation cycle.

| File | Proposed responsibility |
| --- | --- |
| `nix/package.nix` | Thin entry point over schema validation and normalization, phase assembly and the final derivation constructor. Separate those responsibilities instead of one long expression. |
| `nix/set.nix` | Keep package discovery, the lazy package set, aliases, variants and build/target tool bindings. Read vocabularies from the schema instead of maintaining option tables. |
| `nix/platforms.nix` | Thin loader over emitted CPU, OS, ABI and toolchain facts. Bind the selected build and target platforms in Nix, with store paths supplied by bootstrap. |
| `nix/build-systems.nix` | Thin loader over system descriptors. Resolve symbolic tool and dependency defaults against the current sets and fetch adapters. |
| `nix/overrides.nix` | Keep edit operations and Nix function evaluation. Validate operands and edited recipes through the common schema validator. |
| `nix/fetch.nix` | Small adapters over producer descriptors owned by jig and emitted by jib. Keep `derivation`, `outputOf`, stage naming and store references in Nix. |
| `nix/sources.nix` | Keep local TOML loading, pin/hash overrides, URL expansion and fixed-output derivations. Share source field contracts with uptrack rather than duplicate them. |
| `nix/python.nix` | Keep project/environment construction and package resolution. Read helper and override option contracts from the schema. Preserve current local-lock and git-source behavior pending a separate decision. |
| `nix/features.nix` | Keep package-defined feature names and value resolution. Use the schema for declaration shapes and value checking. |
| `bootstrap/default.nix` | Keep stage ordering and dependency edges explicit. Load recipe input contracts and platform facts, then construct the stage derivations in Nix. |

The graph adapters remain real Nix code. Moving their dependency edges into a JSON
expression language would add another evaluator without removing the need for Nix.

## Schema contract

Each option record carries these fields, emitted deterministically from typed Rust data.

| Field | Meaning |
| --- | --- |
| `name` | Recipe spelling, including Serde renames, such as `cargo.noDefaultFeatures`. |
| `type` | A recursive descriptor for scalars, lists, maps, closed records, enums, unions and package/path references. Includes element types and nullability. |
| `default` | Tagged literal value, symbolic resolver or required-without-default. Explicit `null` differs from an omitted field. |
| `doc` | The option's meaning, rendered by `repkgs options` from the same descriptor. |
| `system` | Owning build system, or `null` for common recipe options. |

System descriptors also carry supported phases, their default order, tool roles, lock
producer bindings, platform restrictions and any bootstrap tool-stack ordering.
Platform descriptors contain concrete supported target facts, including triples and
file suffixes. They contain no realized store paths or host-environment observations.

One option declaration must generate both the Serde input contract and its schema
descriptor. A second hand-maintained table of Rust field-name strings would repeat the
current bug. Field types and names come from that declaration, while defaults, docs
and tool roles are explicit metadata attached to it. Common `root`, `tool` and `deps`
options belong to the composed system contract, even when a shared dispatcher consumes them.

Literal defaults serialize the same values used by normalization. Dynamic defaults
name a small typed resolver set, such as the selected build tool, target standard library
or a lock producer using the package source. Nix supplies those bindings lazily.
Arbitrary callbacks stay in Nix, outside the exported schema.

Keep recipe input and resolved builder input distinct. A recipe can hold a derivation
or contextual string where the builder receives a store path. Nix preserves references
through normalization and serializes only the resolved build plan and attributes.
Rust validates that resolved contract when reading it.

## Evaluation and errors

Resolve feature values in the current order, declared default, set-wide value, then
package override. Evaluate the recipe and apply variant/override edits before checking
the resulting recipe. Override operations still report invalid operands at their own path.
Validate supplied values, apply defaults only to absent fields, and validate the normalized
result before constructing the derivation. An explicit `null` never triggers a fetch default.

Closed records reject unknown keys at every level. Open maps explicitly describe their
value type, so `env` names and `cmake.defs` keys remain arbitrary while their values are checked.
Lists validate each element, enums reject unknown values, and unions describe allowed shapes.
Only declared build systems may have nonempty option records. Empty conditional system
records retain their current meaning when that system is absent from `uses`.

Errors identify the package and full field path, with the expected shape or allowed names.
For example, `ripgrep cargo.noDefaultFeatures expected boolean, got string` and
`jq autotools.makeFlag is unknown, expected makeFlags` fail during evaluation.
Validate a container before accessing its children so malformed input produces that error
instead of an unrelated Nix attribute-selection failure.

Package references are opaque leaves. Check their required interface and build/target role
without recursively walking the package set or forcing their output paths. Check path/string
inputs without losing Nix string context. Evaluating one package must not evaluate every recipe,
and querying `supported` must remain possible without constructing unsupported derivations.
Check keys and outer types when constructing the package. Validate each option's nested values
when that option is read, so querying support does not force interpolated dependency paths.

## Recipe spelling and rollout

The first pass proposes zero spelling changes and zero recipes requiring mechanical renames.
Keep `uses`, per-system records, `buildDependencies`, `dependencies`, `phases`, feature names
and override verbs. They already express the ownership split described in `design.md`.
This is a design recommendation, not a previously agreed requirement to freeze the DSL.
Stricter validation may expose invalid values, which must be counted separately from renames.
Any later rename needs an old/new spelling table and a tree-wide recipe count before adoption.
Replace the spelling outright when chosen, without aliases or a compatibility layer.

Start with common recipe fields and one system, then migrate the remaining system tables,
platform facts and fetch/bootstrap input contracts. Each slice removes its duplicate table.
Keep lock parsing, phase execution and relocation in their existing Rust owners.
Do not fold `ghc-bindist.nu` or target-side C rewrites into the schema work.

Done means `jib schema --check nix/schema.json` passes, valid existing recipes evaluate,
invalid keys and values fail at eval, and `repkgs options` shows the emitted defaults and docs.
Check lazy evaluation and store references alongside native and cross proof builds, with
evaluation time and memory compared against the same package selection before the rewrite.
The schema has one current format. No version negotiation or migration framework is needed.
