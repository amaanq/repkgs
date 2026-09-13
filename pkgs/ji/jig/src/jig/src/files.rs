use std::{
   fs::{
      self,
      Permissions,
   },
   io::{
      self,
      Write,
   },
   os::unix::fs::PermissionsExt,
   path::{
      Component,
      Path,
   },
};

use tempfile::NamedTempFile;

pub fn write_file(path: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
   let parent = path
      .parent()
      .filter(|parent| !parent.as_os_str().is_empty())
      .unwrap_or_else(|| Path::new("."));
   let mut temporary = NamedTempFile::new_in(parent)?;
   temporary.write_all(bytes)?;
   let permissions = match mode {
      Some(bits) => Permissions::from_mode(bits),
      None => {
         match fs::metadata(path) {
            Ok(metadata) => metadata.permissions(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Permissions::from_mode(0o644),
            Err(error) => return Err(error),
         }
      },
   };
   temporary.as_file().set_permissions(permissions)?;
   temporary.persist(path).map_err(|error| error.error)?;
   Ok(())
}

pub fn relative_path(text: &str) -> io::Result<&Path> {
   let path = Path::new(text);
   if text.is_empty()
      || text.contains(['\n', '\r', '\t', '\0'])
      || path
         .components()
         .any(|part| !matches!(part, Component::Normal(_)))
   {
      return Err(io::Error::other("invalid relative cache path"));
   }
   Ok(path)
}
