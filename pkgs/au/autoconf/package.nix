{
  package,
  pkgs,
  buildPkgs,
}:
package {
  name = "autoconf";
  patches = [ ./relocatable.patch ];
  uses = [ "autotools" ];
  phases.before."autotools.configure" = [
    {
      # keeps make from regenerating them with help2man
      name = "man-uptodate";
      run = ''
        files $"($c.src)/man/*.1" | each {|page| ^touch $page } | ignore
      '';
    }
  ];
  phases.after."autotools.install" = [
    {
      # the installed scripts name m4, perl and bash by the build machine's paths
      name = "retarget";
      run = ''
        let swaps = ([m4 perl bash] | each {|name|
          {built: (tool $name), runs: $"(dep-root $name 'the scripts run it')/bin/($name)"}
        })
        for script in (glob $"($c.out)/bin/*") {
          edit $script {|| let text = $in; $swaps | reduce --fold $text {|swap, edited| $edited | str replace --all $swap.built $swap.runs } }
        }
      '';
    }
  ];
  buildDependencies = [
    buildPkgs.m4
    buildPkgs.perl
  ];
  dependencies = [
    pkgs.m4
    pkgs.perl
    pkgs.bash
  ];
  tests.run = false;
}
