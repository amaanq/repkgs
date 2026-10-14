use core.nu [ctx fix-shebangs]

# Configure generates util/wrap.pl with an /usr/bin/env perl shebang
export def test-shebangs [] { fix-shebangs $"((ctx).src)/util" }
