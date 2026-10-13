use core.nu [ctx edit x]

# the sandbox's passwd has no shell or $HOME for the build user
export def login-shell []: nothing -> nothing {
   let uid = (^id -u | str trim)
   let sh  = (which sh | first | get path)
   edit /etc/passwd {
      lines
      | each { split row ':' }
      | each {|user| if $user.2 == $uid { $user | update 5 $env.HOME | update 6 $sh } else { $user } }
      | each { str join ':' }
      | str join "\n"
      | $in + "\n"
   }
   mkdir $"($env.HOME)/.ssh"
   $"PATH=($env.PATH | str join ':')\n" | save -f $"($env.HOME)/.ssh/environment"

   let regress = $"((ctx).src)/regress"
   let bin     = (which --all ls | where type == external | first | get path | path dirname)
   edit $"($regress)/test-exec.sh" { str replace 'cat << EOF > $OBJ/sshd_config' "cat << EOF > $OBJ/sshd_config\n\tPermitUserEnvironment yes" }
   edit $"($regress)/sftp-cmds.sh" { str replace -a '/bin' $bin }
   edit $"($regress)/forwarding.sh" { str replace -a '/bin' $bin }
}

# every t-exec script with its own $OBJ and port, the ones on fixed forwarding ports in one group
export def test []: nothing -> nothing {
   let c       = (ctx)
   let regress = $"($c.build)/regress"
   let fixed   = [ cfgmatch cfgmatchlisten forward-control forwarding multiplex ]
   cd $c.build
   x make $"-j($c.njobs)" regress-binaries regress-unit-binaries

   let helpers = (ls $regress | get name)
   let scripts = ("print:\n\t@echo $(LTESTS)\n" | ^make -s --no-print-directory -C $"($c.src)/regress" -f Makefile -f - print | str trim | split row -r '\s+')
   let groups  = (
      $scripts
      | where $it not-in $fixed
      | each {|script| {name: $script, targets: [t-exec], tests: [$script]} }
      | append {name: fixed-ports, targets: [t-exec], tests: $fixed}
      | append {name: rest, targets: [file-tests interop-tests extra-tests unit], tests: []}
      | enumerate
   )

   let failed = ($groups | par-each --threads $c.njobs {|group|
      let g    = $group.item
      let obj  = if $g.tests == [] { $regress } else { $"($regress)-($group.index)" }
      let port = 4300 + $group.index * 4
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
