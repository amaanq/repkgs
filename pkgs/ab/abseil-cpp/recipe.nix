{ source, ... }:
[
  { env.TZDIR = "${toString (source "default")}/absl/time/internal/cctz/testdata/zoneinfo"; }
]
