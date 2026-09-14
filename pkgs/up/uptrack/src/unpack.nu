#!/usr/bin/env nu
# Unpack an archive into `dest` the way sources are stored: if everything sits under one top-level
# directory (tarballs, github zips) that directory becomes `dest`, otherwise (flat zips such as
# deno's releases) the entries land in `dest` as they are. uptrack's prefetch runs this when it
# computes the hash, `jig unpack` (nix/sources.nix) does the same at build time so the hashes agree.
def main [archive: path, dest: path]: nothing -> nothing {
  let stage = $"($dest).unpack"
  mkdir $stage
  ^bsdtar -xf $archive -C $stage --no-same-owner --no-same-permissions
  # a .deb: the tree is the data.tar member
  let data = (ls $stage | where name =~ '/data\.tar\.[a-z0-9]+$')
  if ($data | length) == 1 and ($"($stage)/debian-binary" | path exists) {
    ^bsdtar -xf $data.0.name -C $stage --no-same-owner --no-same-permissions
    rm -f $data.0.name $"($stage)/debian-binary" ...(ls $stage | where name =~ '/control\.tar' | get name)
  }
  ^chmod -R u+w,a-st $stage
  let top = (ls -a $stage)
  if ($top | length) == 1 and $top.0.type == dir {
    mv $top.0.name $dest
    rm -rf $stage
  } else {
    mv $stage $dest
  }
}
