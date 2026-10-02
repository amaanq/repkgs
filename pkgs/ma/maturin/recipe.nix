{ source, ... }:
[
  { install."lib/python3/site-packages/" = "${toString (source "default")}/maturin"; }
]
