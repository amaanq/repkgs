use std::{
   ffi::{
      CString,
      OsStr,
   },
   io,
   os::unix::ffi::OsStrExt as _,
   process::id,
};

use libc::{
   O_CREAT,
   O_EXCL,
   SEM_FAILED,
   sem_close,
   sem_open,
   sem_post,
   sem_t,
   sem_trywait,
   sem_unlink,
};

pub struct Sem {
   raw:  *mut sem_t,
   name: CString,
}

impl Sem {
   pub fn new() -> io::Result<Self> {
      let name = CString::new(format!("/jape-{}", id()))?;
      // SAFETY: name is terminated, O_CREAT supplies mode and initial count.
      let raw = unsafe { sem_open(name.as_ptr(), O_CREAT | O_EXCL, 0o600_u32, 0_u32) };

      if raw == SEM_FAILED {
         return Err(io::Error::last_os_error());
      }

      Ok(Self { raw, name })
   }

   pub fn name(&self) -> &OsStr {
      OsStr::from_bytes(self.name.as_bytes())
   }

   pub fn take(&self) -> io::Result<bool> {
      loop {
         // SAFETY: raw stays open until Drop, and only this thread owns Sem.
         if unsafe { sem_trywait(self.raw) } == 0 {
            return Ok(true);
         }

         let error = io::Error::last_os_error();

         if error.kind() == io::ErrorKind::WouldBlock {
            return Ok(false);
         }
         if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
         }
      }
   }

   pub fn post(&self) -> io::Result<()> {
      // SAFETY: raw stays open until Drop, and only this thread owns Sem.
      if unsafe { sem_post(self.raw) } != 0_i32 {
         return Err(io::Error::last_os_error());
      }

      Ok(())
   }
}

impl Drop for Sem {
   #[expect(
      clippy::multiple_unsafe_ops_per_block,
      reason = "both releases share one ownership proof"
   )]
   fn drop(&mut self) {
      // SAFETY: name stays NUL-terminated and raw stays open until this sole
      // owner is dropped.
      unsafe {
         sem_unlink(self.name.as_ptr());
         sem_close(self.raw);
      }
   }
}
