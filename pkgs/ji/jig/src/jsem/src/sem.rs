//! A POSIX named semaphore somebody else created. Only `sem_trywait` and
//! `sem_post` observe the count, macOS has no `sem_getvalue`.

use std::{
   ffi::CString,
   io,
};

use libc::{
   SEM_FAILED,
   sem_close,
   sem_open,
   sem_post,
   sem_t,
   sem_trywait,
};

pub struct Sem {
   raw: *mut sem_t,
}

impl Sem {
   pub fn open(name: &str) -> io::Result<Self> {
      let cname = CString::new(name)?;
      // SAFETY: sem_open(3) without O_CREAT only reads the name
      let raw = unsafe { sem_open(cname.as_ptr(), 0) };
      if raw == SEM_FAILED {
         return Err(io::Error::last_os_error());
      }
      Ok(Self { raw })
   }

   pub fn try_wait(&self) -> bool {
      // SAFETY: raw came from sem_open and is closed only in Drop
      unsafe { sem_trywait(self.raw) == 0 }
   }

   pub fn post(&self) {
      // SAFETY: raw came from sem_open and is closed only in Drop
      unsafe {
         sem_post(self.raw);
      }
   }
}

impl Drop for Sem {
   fn drop(&mut self) {
      // SAFETY: raw came from sem_open and nothing uses it after this
      unsafe {
         sem_close(self.raw);
      }
   }
}
