use core.nu [act shebangs]

# Configure generates util/wrap.pl with an /usr/bin/env perl shebang
export def test-shebangs [] { shebangs $"((act).src)/util" }
