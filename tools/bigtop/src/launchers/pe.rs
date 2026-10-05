use std::{
   collections::BTreeMap,
   fs::{
      canonicalize,
      copy,
      read_dir,
      rename,
   },
   io,
   path::{
      Path,
      PathBuf,
   },
   process::Command,
};

use mummer::{
   files::{
      glob,
      utf8,
      write_file,
   },
   tree::note,
};

use crate::{
   attrs::nix_store,
   exports::{
      closure,
      exports_of,
      record_dll_dirs,
      relative,
   },
   launchers::entries::{
      Record,
      Variable,
      runtime_env,
   },
};

fn dll_directories(pes: &[PathBuf], sysroot: &Path, deps: &[String]) -> io::Result<Vec<String>> {
   let mut directories = pes
      .iter()
      .filter_map(|path| path.parent().map(Path::to_path_buf))
      .collect::<Vec<PathBuf>>();

   for dep in deps {
      directories.extend([Path::new(dep).join("bin"), Path::new(dep).join("lib")]);
   }

   directories.push(sysroot.join("bin"));
   let mut providers = BTreeMap::<String, PathBuf>::new();

   for directory in directories.iter().filter(|directory| directory.is_dir()) {
      for found in read_dir(directory)? {
         let entry = found?;
         let Some(name) = entry.file_name().to_str().map(str::to_lowercase) else {
            continue;
         };

         providers.entry(name).or_insert_with(|| entry.path());
      }
   }

   let mut needed = Vec::<String>::new();

   for binary in pes {
      for import in imports(binary)? {
         match providers.get(&import) {
            Some(provider) => {
               let resolved = canonicalize(provider)?;
               let directory = resolved
                  .parent()
                  .ok_or_else(|| io::Error::other("DLL has no parent"))?;
               let name = utf8(directory)?.to_owned();

               if !needed.contains(&name) {
                  needed.push(name);
               }
            },
            None if system_dll(sysroot, &import)? => {},
            None => {
               return Err(io::Error::other(format!(
                  "{import} is imported but neither the package, a dependency nor the OS provides \
                   it"
               )));
            },
         }
      }
   }

   for dep in deps {
      for directory in exports_of(Path::new(dep))?.dll_dirs {
         if !needed.contains(&directory) {
            needed.push(directory);
         }
      }
   }

   Ok(needed)
}

pub fn run(output: &Path, launch: &Path, sysroot: &Path, deps: &[String]) -> io::Result<()> {
   let out = utf8(output)?;
   let store_dir = nix_store();
   let package_relative = |path: &str| relative(path, out, &store_dir);
   let pes = glob(&format!("{out}/**/*.{{exe,dll,pyd}}"))?
      .into_iter()
      .filter(|path| path.is_file())
      .collect::<Vec<PathBuf>>();
   // a DLL can arrive transitively (libssh2 through libgit2's pkg-config
   // Requires), and unlike ELF there is no RUNPATH to find it, so every
   // package in the closure is a provider
   let roots = closure(deps)?
      .into_iter()
      .map(|entry| entry.root)
      .collect::<Vec<_>>();
   let needed = dll_directories(&pes, sysroot, &roots)?;
   let (mut environment, _) = runtime_env(deps, out, &store_dir)?;
   let inject = !environment.is_empty();
   let prepended = match environment.remove("PATH") {
      Some(Variable::Prepend(directories)) => directories,
      None | Some(Variable::Default(_) | Variable::Set(_)) => Vec::new(),
   };

   for binary in pes
      .iter()
      .filter(|path| path.extension().is_some_and(|extension| extension == "exe"))
   {
      let directory = binary
         .parent()
         .ok_or_else(|| io::Error::other("PE executable has no parent"))?;
      let file_name = binary
         .file_name()
         .ok_or_else(|| io::Error::other("PE executable has no name"))?;
      let name = utf8(Path::new(file_name))?;
      let path = needed
         .iter()
         .filter(|needed_dir| Path::new(needed_dir) != directory)
         .map(|needed_dir| package_relative(needed_dir))
         .chain(prepended.iter().cloned())
         .collect::<Vec<String>>();

      if path.is_empty() && !inject {
         continue;
      }

      let hidden = directory.join(format!(".{name}"));
      let rel = hidden.strip_prefix(output).map_err(io::Error::other)?;
      rename(binary, &hidden)?;
      environment.insert("PATH".to_owned(), Variable::Prepend(path));

      let record = Record {
         env:     &environment,
         program: format!("{{root}}/{}", rel.display()),
         args:    None,
         argv0:   None,
      };
      let mut rendered = serde_json::to_vec(&record)?;
      rendered.push(b'\n');
      write_file(&directory.join(format!(".{name}.launch")), &rendered, None)?;
      copy(launch, binary)?;
      environment.remove("PATH");
      note(
         "launcher",
         &format!(
            "{} with DLL search paths",
            binary
               .strip_prefix(output)
               .map_err(io::Error::other)?
               .display()
         ),
      );
   }

   let foreign = needed
      .into_iter()
      .filter(|directory| !Path::new(directory).starts_with(output))
      .collect::<Vec<String>>();

   if !foreign.is_empty() {
      record_dll_dirs(output, foreign)?;
   }

   Ok(())
}

fn imports(path: &Path) -> io::Result<Vec<String>> {
   let result = Command::new("llvm-readobj")
      .arg("--coff-imports")
      .arg(path)
      .output()?;

   if !result.status.success() {
      return Err(io::Error::other(format!(
         "llvm-readobj failed for {}",
         path.display()
      )));
   }

   let mut imported = Vec::new();
   let mut regular = false;

   for line in String::from_utf8_lossy(&result.stdout).lines() {
      let text = line.trim();

      if regular {
         if let Some(name) = text
            .strip_prefix("Name: ")
            .and_then(|tail| tail.split_whitespace().next())
         {
            imported.push(name.to_lowercase());
         }

         regular = false;
      }

      if text == "Import {" {
         regular = true;
      }
   }

   Ok(imported)
}

fn system_dll(sysroot: &Path, name: &str) -> io::Result<bool> {
   let stem = name.strip_suffix(".dll").unwrap_or(name);
   Ok(name.starts_with("api-ms-win-")
      || name.starts_with("ext-ms-")
      || sysroot.join("lib").join(format!("lib{stem}.a")).exists()
      || !glob(&format!(
         "{}/sdk/lib/*/{{um,ucrt}}/*/{stem}.lib",
         sysroot.display()
      ))?
      .is_empty())
}
