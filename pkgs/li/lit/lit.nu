use core.nu [act build-dep cue glob-files]

# lit.cfg runs its tests with PYTHONPATH replaced and user site off, so psutil reaches the
# timeout tests only from the interpreter's own site-packages, here a venv's
export def test []: nothing -> nothing {
   let c      = (act)
   let lit    = ($c.src | path join llvm utils lit)
   let venv   = ($c.build | path join lit-venv)
   let psutil = (glob-files --dirs $"(build-dep python-psutil)/lib/python3*/site-packages" | first)
   cue python3 -m venv --without-pip $venv
   let site = (glob-files --dirs $"($venv)/lib/python3*/site-packages" | first)
   $psutil | save ($site | path join psutil.pth)
   cue ($venv | path join bin python3) ($lit | path join lit.py) -sv $"-j($c.njobs)" ($lit | path join tests)
}
