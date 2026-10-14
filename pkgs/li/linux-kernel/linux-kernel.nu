use core.nu [act cue]

def kmake [...args: string]: nothing -> nothing {
   cue make LLVM=1 $"HOSTCC=($env.CC_FOR_BUILD)" $"HOSTCXX=($env.CXX_FOR_BUILD)" $"-j((act).njobs)" ...$args
}

export def configure []: nothing -> nothing { kmake defconfig }

export def build     []: nothing -> nothing { kmake all }

export def install   []: nothing -> nothing {
   let c       = (act)
   let release = (^make -s LLVM=1 kernelrelease | str trim)
   let image   = (^make -s LLVM=1 image_name | str trim)
   let modules = $"($c.out)/lib/modules/($release)"

   mkdir $"($c.out)/boot"

   cp $image $"($c.out)/boot/(($image | path basename))"
   cp System.map .config $"($c.out)/boot"

   kmake modules_install $"INSTALL_MOD_PATH=($c.out)" DEPMOD=true

   rm -f $"($modules)/build"

   let build = $"($c.dev)/lib/modules/($release)/build"
   mkdir $build

   let arch = (ls arch | where type == dir | get name | where {|dir| $"($dir)/include/generated" | path exists } | first)
   let skip = [ "*.o" "*.ko" "*.a" ".*.cmd" "/scripts/dtc/include-prefixes" "/drivers/**.h" "/tools/testing" ]
   let keep = [ "/.config" "/Module.symvers" "/include/***" "/scripts/***" "/tools/objtool/***" "/tools/include/***" "/tools/docs/***" $"/($arch)/include/***" "Makefile*" "Kbuild*" "Kconfig*" "*.h" "*.lds" "*/" ]

   ^rsync -am ...($skip | each { $"--exclude=($in)" }) ...($keep | each { $"--include=($in)" }) --exclude=* ./ $"($build)/"
}
