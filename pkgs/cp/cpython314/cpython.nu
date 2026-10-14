use core.nu [ctx cue files note]
use msbuild.nu

# PCbuild/python.props computes these from patchlevel.h with MSBuild property functions
export def --env properties []: nothing -> nothing {
   let c       = (ctx)
   let version = $c.spec.version
   let parts   = ($version | parse -r '^(?<major>\d+)\.(?<minor>\d+)\.(?<micro>\d+)(?<stage>a|b|rc)?(?<serial>\d+)?$' | first)
   let major   = ($parts.major | into int)
   let minor   = ($parts.minor | into int)
   let micro   = ($parts.micro | into int)
   let level   = match $parts.stage { "a" => 10, "b" => 11, "rc" => 12, _ => 15 }
   let serial  = if ($parts.serial | is-empty) { 0 } else { $parts.serial | into int }
   let hex     = ($major * 0x1000000 + $minor * 0x10000 + $micro * 0x100 + $level * 0x10 + $serial)

   let ours = {
      Configuration: Release
      Platform: x64
      PlatformToolset: ClangCL
      MajorVersionNumber: $parts.major
      MinorVersionNumber: $parts.minor
      MicroVersionNumber: $parts.micro
      ReleaseLevelName: $"($parts.stage)($parts.serial)"
      _ReleaseLevel: ""
      ReleaseSerial: ($serial | into string)
      ReleaseLevelNumber: ($level | into string)
      PythonVersionHex: ($hex | into string)
      Field3Value: ($micro * 1000 + $level * 10 + $serial | into string)
      OverrideVersion: $version
      PySourcePath: $"($c.src)/"
      Py_OutDir: $"($c.build)/out"
      Py_IntDir: $"($c.build)/obj"
      IncludeSSL: "false"
      IncludeTkinter: "false"
      IncludeTests: "false"
      IncludeCTypes: "false"
      IncludeUwp: "false"
   }
   let given = $c.spec.vcxproj.properties
   $c | upsert spec.vcxproj.properties ($ours | merge $given) | to json | save -f ($env.NIX_BUILD_TOP | path join ctx.json)
}

# _freeze_module.vcxproj runs a _freeze_module.exe it builds for this machine. The build python's
# Programs/_freeze_module.py writes the same headers, as PCbuild itself does for ARM64
export def frozen-modules []: nothing -> nothing {
   let c       = (ctx)
   let pcbuild = $"($c.src)/PCbuild"
   let project = (msbuild evaluate $"($pcbuild)/_freeze_module.vcxproj" $c.spec.vcxproj.properties)
   let frozen  = ($project.items | where type in [None GetPath] and $it.meta.ModName? != null)
   note freeze $"($frozen | length) modules with the build python"
   for item in $frozen {
      let header = (msbuild win-path $item.meta.OutFile $pcbuild)
      mkdir ($header | path dirname)
      cue python3 $"($c.src)/Programs/_freeze_module.py" $item.meta.ModName $item.path $header
   }
}

# what zlib-ng.vcxproj's _EnsureZlibH target generates, with an empty symbol prefix
export def zlib-ng-headers []: nothing -> nothing {
   let c       = (ctx)
   let given   = $c.spec.vcxproj.properties
   let project = (msbuild evaluate $"($c.src)/PCbuild/zlib-ng.vcxproj" $given)
   let into    = (msbuild win-path $project.props.GeneratedZlibNgDir $"($c.src)/PCbuild")
   mkdir $into
   for header in [zlib zlib-ng] {
      open --raw $"($given.zlibNgDir)/($header).h.in" | str replace -a "@ZLIB_SYMBOL_PREFIX@" "" | save -f $"($into)/($header).h"
   }
}

# the tree PC/layout gives an installed python, import libraries in libs\ and the stdlib in Lib\
export def windows-layout []: nothing -> nothing {
   let c   = (ctx)
   let out = $c.out
   mkdir $"($out)/libs"
   for import in (files $"($out)/lib/*.lib") { mv $import $"($out)/libs/" }
   rm -rf $"($out)/lib"

   cue cp -r $"($c.src)/Lib" $"($out)/Lib"
   rm -rf $"($out)/Lib/test" ...(files --dirs $"($out)/Lib/**/__pycache__")

   mkdir $"($out)/include"
   cue cp -r ...(files --any $"($c.src)/Include/*") $"($c.src)/PC/pyconfig.h" $"($out)/include/"
}

# the PEP 739 file names the paths of the python that generated it, under cross the build
# machine's. These are where ours lands, relative like --relative-paths writes them
export def build-details []: nothing -> nothing {
   let out     = (ctx).out
   let file    = (files $"($out)/lib/python3.*/build-details.json" | first)
   let python  = ($file | path dirname | path basename)
   let details = (open $file)
   $details
   | reject libpython.static
   | merge deep {
      base_prefix: "../.."
      base_interpreter: $"./bin/($python)"
      libpython: {dynamic: $"./lib/lib($python).so", dynamic_stableabi: "./lib/libpython3.so"}
      c_api: {headers: $"./include/($python)", pkgconfig_path: "./lib/pkgconfig"}
   }
   | save -f $file
}
