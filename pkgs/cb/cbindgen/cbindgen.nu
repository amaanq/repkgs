# the expand and bitfield tests run rustc -Zunpretty=expanded, which is a nightly flag
export def --env unstable []: nothing -> nothing { $env.RUSTC_BOOTSTRAP = "1" }
