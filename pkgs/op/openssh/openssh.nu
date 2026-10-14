use core.nu [act rewrite cue]

# the sandbox's passwd has no shell or $HOME for the build user
export def login-shell []: nothing -> nothing {
   let uid = (^id -u | str trim)
   let sh  = (which sh | first | get path)
   rewrite /etc/passwd {
      lines
      | each { split row ':' }
      | each {|user| if $user.2 == $uid { $user | update 5 $env.HOME | update 6 $sh } else { $user } }
      | each { str join ':' }
      | str join "\n"
      | $in + "\n"
   }
   mkdir $"($env.HOME)/.ssh"
   $"PATH=($env.PATH | str join ':')\n" | save -f $"($env.HOME)/.ssh/environment"

   let regress = $"((act).src)/regress"
   let bin     = (which --all ls | where type == external | first | get path | path dirname)
   rewrite $"($regress)/test-exec.sh" { str replace 'cat << EOF > $OBJ/sshd_config' "cat << EOF > $OBJ/sshd_config\n\tPermitUserEnvironment yes" }
   rewrite $"($regress)/sftp-cmds.sh" { str replace -a '/bin' $bin }
   rewrite $"($regress)/forwarding.sh" { str replace -a '/bin' $bin | str replace 'base=33' 'base=$((PORT / 100))' }
   rewrite $"($regress)/cfgmatch.sh" { str replace 'fwdport=3301' 'fwdport=$((PORT / 100))01' }
   rewrite $"($regress)/cfgmatchlisten.sh" { str replace 'fwdport=3301' 'fwdport=$((PORT / 100))01' }
   rewrite $"($regress)/multiplex.sh" { str replace 'P=3301' 'P=$((PORT / 100))01' }
   rewrite $"($regress)/forward-control.sh" { str replace 'LFWD_PORT=3320' 'LFWD_PORT=$((PORT / 100))20' | str replace 'RFWD_PORT=3321' 'RFWD_PORT=$((PORT / 100))21' }
}

# each t-exec script gets its own $OBJ and 100 ports. The forwarding scripts use xx00 to xx72 of
# them and connection-timeout uses $PORT to $PORT+4
export def test []: nothing -> nothing {
   let c       = (act)
   let regress = $"($c.build)/regress"
   cd $c.build
   cue make $"-j($c.njobs)" regress-binaries regress-unit-binaries

   let helpers = (ls $regress | get name)
   let scripts = ("print:\n\t@echo $(LTESTS)\n" | ^make -s --no-print-directory -C $"($c.src)/regress" -f Makefile -f - print | str trim | split row -r '\s+')
   let groups  = (
      $scripts
      | each {|script| {name: $script, targets: [t-exec], tests: [$script]} }
      | append {name: rest, targets: [file-tests interop-tests extra-tests unit], tests: []}
      | enumerate
   )

   let failed = ($groups | par-each --threads $c.njobs {|group|
      let g    = $group.item
      let obj  = if $g.tests == [] { $regress } else { $"($regress)-($group.index)" }
      let port = 5090 + $group.index * 100
      if $obj != $regress {
         mkdir $obj
         for helper in $helpers { ^ln -s $helper $obj }
      }
      let run = (with-env {TEST_SSH_PORT: ($port | into string)} {
         ^make ...$g.targets $"REGRESS_OBJ=($obj)" $"LTESTS=($g.tests | str join ' ')" | complete
      })
      if $run.exit_code == 0 {
         print -e $"ok ($g.name)"
         null
      } else {
         {name: $g.name, output: $"($run.stdout)($run.stderr)"}
      }
   } | compact)

   for failure in $failed { print -e $"--- ($failure.name)\n($failure.output)" }
   if $failed != [] { error make {msg: $"failed: ($failed | get name | str join ' ')"} }
}
