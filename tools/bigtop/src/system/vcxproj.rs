use std::{
   collections::{
      BTreeMap,
      BTreeSet,
   },
   fmt::{
      self,
      Display,
      Formatter,
   },
   fs::{
      copy,
      create_dir_all,
      read,
   },
   io,
   path::{
      Path,
      PathBuf,
   },
   str::FromStr,
};

use mummer::{
   files::{
      utf8,
      write_file,
   },
   tree::note,
};
use serde::{
   Deserialize,
   Serialize,
};

use crate::{
   attrs::{
      Attrs,
      Ctx,
   },
   schema::{
      Condition,
      Field,
      OptionType,
      Type,
      Unsupported,
   },
   system::{
      command::{
         run,
         unknown,
      },
      dispatch::System,
      msbuild::{
         Item,
         evaluate,
         split_list,
         win_path,
      },
   },
};

options! {
   Options {
      _root ("root"): String = ".".to_owned()
         => "directory below the source the project lives in (monorepos, build/cmake)",
      projects ("projects"): Vec<String> = Vec::new()
         => "vcxproj files or traversal projects relative to the source root",
      exclude ("exclude"): Vec<String> = Vec::new()
         => "project file stems excluded from the build",
      properties ("properties"): BTreeMap<String, String> = BTreeMap::new()
         => "MSBuild global properties",
      install ("install"): BTreeMap<ConfigurationType, String> = BTreeMap::new()
         => "installation directory per configuration type",
   }
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
enum ConfigurationType {
   Application,
   DynamicLibrary,
   StaticLibrary,
}

impl OptionType for ConfigurationType {
   fn kind() -> Type {
      Type::Enum {
         values: [Self::Application, Self::DynamicLibrary, Self::StaticLibrary]
            .map(|kind| kind.to_string())
            .to_vec(),
      }
   }
}

impl ConfigurationType {
   const fn extension(self) -> &'static str {
      match self {
         Self::Application => ".exe",
         Self::DynamicLibrary => ".dll",
         Self::StaticLibrary => ".lib",
      }
   }

   const fn install_dir(self) -> &'static str {
      match self {
         Self::Application | Self::DynamicLibrary => "bin",
         Self::StaticLibrary => "lib",
      }
   }
}

impl FromStr for ConfigurationType {
   type Err = io::Error;

   fn from_str(value: &str) -> io::Result<Self> {
      match value {
         "Application" => Ok(Self::Application),
         "DynamicLibrary" => Ok(Self::DynamicLibrary),
         "StaticLibrary" => Ok(Self::StaticLibrary),
         _ => {
            Err(io::Error::other(format!(
               "unsupported ConfigurationType {value}"
            )))
         },
      }
   }
}

impl Display for ConfigurationType {
   #[expect(
      clippy::renamed_function_params,
      reason = "names must contain at least two characters"
   )]
   fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
      formatter.write_str(match *self {
         Self::Application => "Application",
         Self::DynamicLibrary => "DynamicLibrary",
         Self::StaticLibrary => "StaticLibrary",
      })
   }
}

#[derive(Deserialize, Serialize)]
struct Project {
   name:   String,
   kind:   ConfigurationType,
   output: String,
   implib: Option<String>,
   file:   String,
   #[serde(skip)]
   refs:   Vec<String>,
   #[serde(skip)]
   inputs: Vec<String>,
   #[serde(skip)]
   link:   Vec<String>,
   #[serde(skip)]
   edges:  Vec<String>,
}

pub struct Vcxproj;

impl System for Vcxproj {
   fn module(&self) -> &'static str {
      module_path!()
   }

   fn name(&self) -> &'static str {
      "vcxproj"
   }

   fn options(&self) -> io::Result<Vec<Field>> {
      Options::schema(self.name())
   }

   fn phases(&self) -> &'static [&'static str] {
      &["configure", "build", "install"]
   }

   fn unsupported(&self) -> Option<Unsupported> {
      Some(Unsupported::new(
         Condition::LibcOtherThan("msvc".to_owned()),
         "Visual Studio projects describe an MSVC-ABI build",
      ))
   }

   fn workdir(&self, ctx: &Ctx) -> io::Result<PathBuf> {
      Ok(ctx.build.clone())
   }

   fn phase(&self, ctx: &Ctx, _attrs: &Attrs, verb: &str, _socket: &Path) -> io::Result<()> {
      let options = ctx.options::<Options>("vcxproj")?;
      match verb {
         "configure" => configure(ctx, &options),
         "build" => {
            run(
               "ninja",
               &["-C", utf8(&ctx.build)?, &format!("-j{}", ctx.njobs)],
               &[],
            )
         },
         "install" => install(ctx, &options),
         _ => Err(unknown("vcxproj", verb)),
      }
   }
}

fn list(value: &str) -> Vec<&str> {
   let mut seen = BTreeSet::new();
   split_list(value)
      .filter(|part| seen.insert(*part))
      .collect()
}

fn value<'meta>(meta: &'meta BTreeMap<String, String>, key: &str) -> &'meta str {
   meta.get(key).map_or("", String::as_str)
}

fn defines(source: &str) -> Vec<String> {
   list(source)
      .into_iter()
      .map(|definition| format!("-D{}", definition.replace("\\\"", "\"")))
      .collect()
}

fn cl_flags(meta: &BTreeMap<String, String>, dir: &Path) -> io::Result<Vec<String>> {
   let optimization = match value(meta, "Optimization") {
      "Disabled" => "-O0",
      "MinSpace" => "-Os",
      "Full" => "-O3",
      _ => "-O2",
   };
   let runtime = match value(meta, "RuntimeLibrary") {
      "MultiThreaded" => "-fms-runtime-lib=static",
      "MultiThreadedDebug" => "-fms-runtime-lib=static_dbg",
      "MultiThreadedDebugDLL" => "-fms-runtime-lib=dll_dbg",
      _ => "-fms-runtime-lib=dll",
   };
   let mut flags = vec![optimization.to_owned(), runtime.to_owned()];
   match value(meta, "ExceptionHandling") {
      "Async" => flags.push("-fasync-exceptions".to_owned()),
      "false" => flags.push("-fno-exceptions".to_owned()),
      _ => {},
   }
   let arch: &[&str] = match value(meta, "EnableEnhancedInstructionSet") {
      "StreamingSIMDExtensions" => &["-msse"],
      "StreamingSIMDExtensions2" => &["-msse2"],
      "AdvancedVectorExtensions" => &["-mavx"],
      "AdvancedVectorExtensions2" => &["-mavx2"],
      "AdvancedVectorExtensions512" => {
         &[
            "-mavx512f",
            "-mavx512bw",
            "-mavx512cd",
            "-mavx512dq",
            "-mavx512vl",
         ]
      },
      _ => &[],
   };
   flags.extend(arch.iter().map(|flag| (*flag).to_owned()));
   for key in ["LanguageStandard", "LanguageStandard_C"] {
      if let Some(standard) = value(meta, key).strip_prefix("std") {
         flags.push(format!("-std={}", standard.replacen("cpp", "c++", 1)));
      }
   }
   flags.extend(defines(value(meta, "PreprocessorDefinitions")));
   for directory in list(value(meta, "AdditionalIncludeDirectories")) {
      flags.push(format!("-I{}", win_path(directory, dir)?));
   }
   match value(meta, "CompileAs") {
      "CompileAsCpp" => flags.extend(["-x".to_owned(), "c++".to_owned()]),
      "CompileAsC" => flags.extend(["-x".to_owned(), "c".to_owned()]),
      _ => {},
   }
   flags.extend(
      list(&value(meta, "AdditionalOptions").replace(' ', ";"))
         .into_iter()
         .filter_map(cl_option),
   );
   Ok(flags)
}

fn cl_option(option: &str) -> Option<String> {
   let normalized = option
      .strip_prefix('-')
      .map_or_else(|| option.to_owned(), |rest| format!("/{rest}"));
   if matches!(
      normalized.as_str(),
      "/utf-8" | "/MP" | "/bigobj" | "/FS" | "/EHsc"
   ) || normalized.starts_with("/flto")
      || normalized.starts_with("/GL")
      || normalized.starts_with("/d2")
   {
      return None;
   }
   if normalized.strip_prefix("/Zm").is_some_and(|suffix| {
      !suffix.is_empty() && suffix.chars().all(|character| character.is_ascii_digit())
   }) {
      return None;
   }
   if let Some(warning) = normalized.strip_prefix("/w") {
      let mut chars = warning.chars();
      if chars
         .next()
         .is_some_and(|character| matches!(character, 'd' | 'e' | '1'..='4'))
         && !chars.as_str().is_empty()
         && chars.all(|character| character.is_ascii_digit())
      {
         return None;
      }
   }
   if let Some(architecture) = normalized.strip_prefix("/arch:") {
      return Some(format!("-m{}", architecture.to_lowercase()));
   }
   if let Some(standard) = normalized.strip_prefix("/std:") {
      return Some(format!(
         "-std={}",
         standard.replacen("c++latest", "c++2c", 1)
      ));
   }
   if normalized == "/EHa" {
      return Some("-fasync-exceptions".to_owned());
   }
   Some(option.to_owned())
}

type LinkSetting = fn(&str, &Path) -> io::Result<Option<String>>;

fn link_flags(
   meta: &BTreeMap<String, String>,
   kind: ConfigurationType,
   dir: &Path,
) -> io::Result<Vec<String>> {
   let mut flags = Vec::new();
   for library in list(value(meta, "AdditionalDependencies")) {
      flags.push(if library.contains(['\\', '/']) {
         win_path(library, dir)?
      } else {
         format!("-l{}", library.strip_suffix(".lib").unwrap_or(library))
      });
   }
   for directory in list(value(meta, "AdditionalLibraryDirectories")) {
      flags.push(format!("-L{}", win_path(directory, dir)?));
   }
   let settings: [(&str, LinkSetting); 6] = [
      ("SubSystem", |setting, _| {
         Ok(Some(format!("/subsystem:{}", setting.to_lowercase())))
      }),
      ("StackReserveSize", |setting, _| {
         Ok(Some(format!("/stack:{setting}")))
      }),
      ("ModuleDefinitionFile", |setting, base| {
         Ok(Some(format!("/def:{}", win_path(setting, base)?)))
      }),
      ("BaseAddress", |setting, _| {
         Ok(Some(format!("/base:{setting}")))
      }),
      ("EntryPointSymbol", |setting, _| {
         Ok(Some(format!("/entry:{setting}")))
      }),
      ("NoEntryPoint", |setting, _| {
         Ok((setting == "true").then(|| "/noentry".to_owned()))
      }),
   ];
   for (key, translate) in settings {
      let setting = value(meta, key);
      if setting.is_empty() {
         continue;
      }
      if let Some(option) = translate(setting, dir)? {
         flags.push(format!("-Wl,{option}"));
      }
   }
   flags.extend(
      list(value(meta, "DelayLoadDLLs"))
         .into_iter()
         .map(|library| format!("-Wl,/delayload:{library}")),
   );
   for option in list(&value(meta, "AdditionalOptions").replace(' ', ";")) {
      let normalized = option.strip_prefix(['/', '-']).unwrap_or(option);
      if option.starts_with(['/', '-'])
         && ["LTCG", "GENPROFILE", "USEPROFILE", "OPT:"]
            .iter()
            .any(|prefix| normalized.starts_with(prefix))
      {
         continue;
      }
      flags.push(format!("-Wl,{option}"));
   }
   if kind == ConfigurationType::DynamicLibrary {
      flags.push("-shared".to_owned());
   }
   Ok(flags)
}

fn rc_flags(meta: &BTreeMap<String, String>, dir: &Path) -> io::Result<Vec<String>> {
   let mut flags = defines(value(meta, "PreprocessorDefinitions"));
   for directory in list(value(meta, "AdditionalIncludeDirectories")) {
      flags.push(format!("-I{}", win_path(directory, dir)?));
   }
   Ok(flags)
}

fn ninja_quote(value: &str) -> String {
   value
      .replace('$', "$$")
      .replace(' ', "$ ")
      .replace(':', "$:")
}

fn shell_quote(value: &str) -> String {
   if !value.is_empty()
      && value
         .chars()
         .all(|character| character.is_alphanumeric() || "_@%+=:,./-".contains(character))
   {
      value.to_owned()
   } else {
      format!("'{}'", value.replace('\'', "'\\''"))
   }
}

fn quoted_flags(flags: &[String]) -> String {
   flags
      .iter()
      .map(|flag| shell_quote(flag))
      .collect::<Vec<_>>()
      .join(" ")
}

fn quoted_paths(paths: &[String]) -> String {
   paths
      .iter()
      .map(|path| ninja_quote(path))
      .collect::<Vec<_>>()
      .join(" ")
}

fn project(ctx: &Ctx, file: String, props: &BTreeMap<String, String>) -> io::Result<Project> {
   let path = format!("{}/{file}", ctx.src.display());
   let dir = Path::new(&path).parent().unwrap_or_else(|| Path::new("."));
   let toolset = BTreeMap::from([(
      "Link".to_owned(),
      BTreeMap::from([(
         "AdditionalDependencies".to_owned(),
         "kernel32.lib;user32.lib;gdi32.lib;winspool.lib;comdlg32.lib;advapi32.lib;shell32.lib;\
          ole32.lib;oleaut32.lib;uuid.lib;odbc32.lib;odbccp32.lib"
            .to_owned(),
      )]),
   )]);
   let evaluated = evaluate(Path::new(&path), props, toolset)?;
   let kind = evaluated
      .props
      .get("ConfigurationType")
      .map_or(Ok(ConfigurationType::Application), |kind| kind.parse())?;
   let ext = evaluated
      .props
      .get("TargetExt")
      .filter(|ext| !ext.is_empty())
      .map_or_else(|| kind.extension(), String::as_str);
   let stem = utf8(Path::new(Path::new(&file).file_stem().unwrap_or_default()))?;
   let target = evaluated
      .props
      .get("TargetName")
      .filter(|target| !target.is_empty())
      .map_or(stem, String::as_str);
   let name = format!("{target}{ext}");
   let object_dir = format!("{}/obj/{stem}", ctx.build.display());
   let refs = evaluated
      .items
      .iter()
      .filter(|item| item.kind == "ProjectReference")
      .map(|item| {
         Path::new(&item.path).strip_prefix(&ctx.src).map_or_else(
            |_| item.path.clone(),
            |relative| {
               relative
                  .to_str()
                  .map_or_else(|| item.path.clone(), str::to_owned)
            },
         )
      })
      .collect();
   let default_outdir = format!("{}/out/", ctx.build.display());
   let outdir = win_path(
      evaluated
         .props
         .get("OutDir")
         .map_or(default_outdir.as_str(), String::as_str),
      dir,
   )?;
   let output = format!("{}/{name}", outdir.trim_end_matches('/'));
   let implib = if kind == ConfigurationType::Application {
      None
   } else {
      Some(format!(
         "{}/{}.lib",
         outdir.trim_end_matches('/'),
         utf8(Path::new(Path::new(&name).file_stem().unwrap_or_default()))?
      ))
   };
   let link = link_flags(
      evaluated.itemdefs.get("Link").unwrap_or(&BTreeMap::new()),
      kind,
      dir,
   )?;
   let mut project = Project {
      name,
      kind,
      output,
      implib,
      file,
      refs,
      inputs: Vec::new(),
      link,
      edges: Vec::new(),
   };
   project.compile_edges(&evaluated.items, &object_dir, dir)?;
   Ok(project)
}

impl Project {
   fn compile_edges(&mut self, items: &[Item], object_dir: &str, dir: &Path) -> io::Result<()> {
      let object_name = |path: &str, extension: &str| {
         format!(
            "{object_dir}/{}.{extension}",
            path.trim_start_matches('/').replace('/', "__")
         )
      };
      let live = |kind: &'static str| {
         items.iter().filter(move |item| {
            item.kind == kind && value(&item.meta, "ExcludedFromBuild") != "true"
         })
      };
      for item in live("ClCompile") {
         let output = object_name(&item.path, "obj");
         let rule = if Path::new(&item.path)
            .extension()
            .is_some_and(|ext| matches!(ext.to_str(), Some("cc" | "cpp" | "cxx")))
         {
            "cxx"
         } else {
            "cc"
         };
         self.edges.push(format!(
            "build {}: {rule} {}\n  flags = {}\n",
            ninja_quote(&output),
            ninja_quote(&item.path),
            quoted_flags(&cl_flags(&item.meta, dir)?)
         ));
         self.inputs.push(output);
      }
      for item in live("ResourceCompile") {
         let output = object_name(&item.path, "res");
         let mut directories = vec![
            utf8(
               Path::new(&item.path)
                  .parent()
                  .unwrap_or_else(|| Path::new(".")),
            )?
            .to_owned(),
         ];
         for directory in list(value(&item.meta, "AdditionalIncludeDirectories")) {
            directories.push(win_path(directory, dir)?);
         }
         let rcdirs = directories
            .iter()
            .map(|directory| format!("/I {}", shell_quote(directory)))
            .collect::<Vec<_>>()
            .join(" ");
         self.edges.push(format!(
            "build {}: rc {}\n  flags = {}\n  rcflags = {rcdirs}\n",
            ninja_quote(&output),
            ninja_quote(&item.path),
            quoted_flags(&rc_flags(&item.meta, dir)?)
         ));
         self.inputs.push(output);
      }
      for item in live("CustomBuild") {
         let mut outputs = Vec::new();
         for output in list(value(&item.meta, "Outputs")) {
            outputs.push(win_path(output, dir)?);
         }
         let command = item
            .meta
            .get("Command")
            .ok_or_else(|| io::Error::other("CustomBuild has no Command"))?
            .replace('\\', "/");
         let translated = ["ml64 ", "ml "]
            .iter()
            .find_map(|prefix| {
               command
                  .strip_prefix(prefix)
                  .map(|rest| format!("llvm-{prefix}{rest}"))
            })
            .unwrap_or(command);
         self.edges.push(format!(
            "build {}: custom {}\n  cmd = {translated}\n",
            quoted_paths(&outputs),
            ninja_quote(&item.path)
         ));
         self.inputs.extend(outputs.into_iter().filter(|output| {
            Path::new(output)
               .extension()
               .is_some_and(|extension| extension == "obj")
         }));
      }
      Ok(())
   }
}

const RULES: &str = r"rule cc
  command = cc -MD -MF $out.d $flags -c $in -o $out
  deps = gcc
  depfile = $out.d
  description = cc $out
rule cxx
  command = c++ -MD -MF $out.d $flags -c $in -o $out
  deps = gcc
  depfile = $out.d
  description = c++ $out
rule rc
  command = cc -E -xc -DRC_INVOKED $flags $in -o $out.i && sed -i -E '/^ *[0-9A-Za-z_]+ +(ICON|BITMAP|CURSOR|FONT|HTML|RT_MANIFEST|24|RCDATA|MESSAGETABLE) /s,[\],/,g' $out.i && llvm-rc /no-preprocess /C 1252 $rcflags /FO $out $out.i
  description = rc $out
rule link
  command = cc $in -o $main $flags
  description = link $out
rule lib
  command = llvm-lib /nologo /out:$out $in
  description = lib $out
rule custom
  command = $cmd
  description = $cmd
";

fn ninja_file(projects: &[Project]) -> io::Result<String> {
   let by_file = projects
      .iter()
      .map(|project| (project.file.as_str(), project))
      .collect::<BTreeMap<&str, &Project>>();
   let mut sections = vec![RULES.to_owned()];
   sections.extend(
      projects
         .iter()
         .flat_map(|project| project.edges.iter().cloned()),
   );
   for project in projects {
      if project.kind == ConfigurationType::StaticLibrary {
         let implib = project
            .implib
            .as_deref()
            .ok_or_else(|| io::Error::other("static library has no output"))?;
         sections.push(format!(
            "build {}: lib {}\n",
            ninja_quote(implib),
            quoted_paths(&project.inputs)
         ));
         continue;
      }
      let dependencies = project
         .refs
         .iter()
         .filter_map(|file| by_file.get(file.as_str()))
         .filter(|dependency| dependency.kind != ConfigurationType::Application)
         .filter_map(|dependency| dependency.implib.as_deref())
         .map(ninja_quote)
         .collect::<Vec<String>>();
      let also = project
         .implib
         .as_ref()
         .map_or_else(String::new, |implib| format!(" | {}", ninja_quote(implib)));
      let mut flags = quoted_flags(&project.link);
      if let Some(implib) = project.implib.as_deref() {
         if !flags.is_empty() {
            flags.push(' ');
         }
         flags.push_str(&shell_quote(&format!("-Wl,/implib:{implib}")));
      }
      sections.push(format!(
         "build {}{also}: link {} {}\n  main = {}\n  flags = {flags}\n",
         ninja_quote(&project.output),
         quoted_paths(&project.inputs),
         dependencies.join(" "),
         ninja_quote(&project.output)
      ));
   }
   Ok(sections.join("\n"))
}

fn projects(ctx: &Ctx, options: &Options) -> io::Result<Vec<Project>> {
   if options.projects.is_empty() {
      return Err(io::Error::other("vcxproj.projects is empty"));
   }
   let mut files = Vec::new();
   let mut seen = BTreeSet::new();
   for file in &options.projects {
      if Path::new(file)
         .extension()
         .is_none_or(|extension| extension != "proj")
      {
         if seen.insert(file.clone()) {
            files.push(file.clone());
         }
         continue;
      }
      let path = format!("{}/{file}", ctx.src.display());
      let dir = Path::new(&path).parent().unwrap_or_else(|| Path::new("."));
      let evaluated = evaluate(Path::new(&path), &options.properties, BTreeMap::new())?;
      for expanded in evaluated.sub_projects("Build", dir)? {
         let inside = Path::new(&expanded)
            .strip_prefix(&ctx.src)
            .map_err(io::Error::other)?;
         let relative = utf8(inside)?.to_owned();
         if seen.insert(relative.clone()) {
            files.push(relative);
         }
      }
   }
   files
      .into_iter()
      .filter(|file| {
         !options.exclude.iter().any(|excluded| {
            Path::new(file)
               .file_stem()
               .is_some_and(|stem| stem == excluded.as_str())
         })
      })
      .map(|file| project(ctx, file, &options.properties))
      .collect()
}

fn configure(ctx: &Ctx, options: &Options) -> io::Result<()> {
   let projects = projects(ctx, options)?;
   for project in &projects {
      if let Some(directory) = Path::new(&project.output).parent() {
         create_dir_all(directory)?;
      }
      note(
         "project",
         &format!(
            "{} {} {}, {} objects",
            project.file,
            project.kind,
            project.name,
            project.inputs.len()
         ),
      );
   }
   write_file(
      &ctx.build.join("build.ninja"),
      ninja_file(&projects)?.as_bytes(),
      None,
   )?;
   write_file(
      &ctx.build.join("projects.json"),
      &serde_json::to_vec(&projects)?,
      None,
   )
}

fn install(ctx: &Ctx, options: &Options) -> io::Result<()> {
   let projects: Vec<Project> = serde_json::from_slice(&read(ctx.build.join("projects.json"))?)?;
   for project in projects {
      let directory = ctx.out.join(
         options
            .install
            .get(&project.kind)
            .map_or_else(|| project.kind.install_dir(), String::as_str),
      );
      create_dir_all(&directory)?;
      if project.kind != ConfigurationType::StaticLibrary {
         let name = Path::new(&project.output)
            .file_name()
            .ok_or_else(|| io::Error::other("project output has no file name"))?;
         copy(&project.output, directory.join(name))?;
      }
      if let Some(implib) = project.implib.filter(|path| Path::new(path).exists()) {
         let lib = ctx.out.join("lib");
         create_dir_all(&lib)?;
         let name = Path::new(&implib)
            .file_name()
            .ok_or_else(|| io::Error::other("import library has no file name"))?;
         copy(&implib, lib.join(name))?;
      }
   }
   Ok(())
}
