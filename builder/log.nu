# One line per event in Nix's structured-log form ("@nix {json}", libutil/logging.cc), so
# `nix build` and rom show the current phase and `nix log` keeps the text. A `phase` event sets the
# derivation's phase, anything else is an info-level message. The builder and bootstrap share it
export def note [kind: string, msg: string = ""]: nothing -> nothing {
  let event = if $kind == "phase" {
    {action: setPhase, phase: $msg}
  } else {
    {action: msg, level: 3, msg: $"($kind): ($msg)"}
  }
  print -e $"@nix ($event | to json -r)"
}
