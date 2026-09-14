# cargo build/test/install against a vendored registry snapshot, jig system phase cargo <verb>
export def --env setup []: nothing -> nothing {
  load-env (^jig system setup cargo | from json)
  hide-env -i RUSTFLAGS
}
export def workdir []: nothing -> string { ^jig system workdir cargo | str trim }
export def build []: nothing -> nothing { ^jig system phase cargo build }
export def test []: nothing -> nothing { ^jig system phase cargo test }
export def install []: nothing -> nothing { ^jig system phase cargo install }
