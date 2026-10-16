use core.nu [act cue]

# The asset sources are vendored crate paths that only exist inside the build.
export def bundle-assets []: nothing -> nothing {
   let out = (act).out
   cue $"($out)/bin/circus-server" --bundle-assets $"($out)/share/circus-server/assets"
}
