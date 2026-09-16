//! A token in the semaphore is a daemon slot this build holds idle. The
//! broker keeps one such spare, orders another slot when it was taken and
//! returns slots when ghc gives tokens back. Tokens are anonymous, so one
//! broker per semaphore (../broker.qnt).

use std::{
   io::{
      BufReader,
      Write as _,
   },
   os::unix::net::UnixStream,
   path::PathBuf,
};

use rustix::{
   event::{
      PollFd,
      PollFlags,
      Timespec,
      poll,
   },
   io::Errno,
};
use wire::read_line;

use crate::sem::Sem;

/// One slot per order over its own connection, which the daemon frees on
/// close.
pub struct Orders {
   socket:  PathBuf,
   build:   String,
   pending: Option<BufReader<UnixStream>>,
}

impl Orders {
   pub const fn new(socket: PathBuf, build: String) -> Self {
      Self {
         socket,
         build,
         pending: None,
      }
   }

   /// `want` places or checks on an order, otherwise cancels a pending one.
   pub fn poll(&mut self, want: bool) -> Option<UnixStream> {
      if !want {
         self.pending = None;
         return None;
      }
      if self.pending.is_none() {
         let mut stream = UnixStream::connect(&self.socket).ok()?;
         writeln!(stream, "SLOT {}", self.build).ok()?;
         self.pending = Some(BufReader::new(stream));
      }
      if !readable(self.pending.as_ref()?.get_ref()) {
         return None;
      }
      let mut order = self.pending.take()?;
      let granted = read_line(&mut order)
         .ok()?
         .is_some_and(|reply| reply == "OK");
      granted.then(|| order.into_inner())
   }
}

fn readable(stream: &UnixStream) -> bool {
   let mut descriptors = [PollFd::new(stream, PollFlags::IN)];
   let timeout = Timespec {
      tv_sec:  0,
      tv_nsec: 0,
   };
   match poll(&mut descriptors, Some(&timeout)) {
      Ok(_) => !descriptors[0].revents().is_empty(),
      Err(Errno::INTR) => false,
      Err(_) => true,
   }
}

pub struct Broker {
   sem:    Sem,
   orders: Orders,
   held:   Vec<UnixStream>,
}

impl Broker {
   pub const fn new(sem: Sem, orders: Orders) -> Self {
      Self {
         sem,
         orders,
         held: Vec::new(),
      }
   }

   /// Keep one idle token, return the rest, order one if none.
   pub fn tick(&mut self) {
      let mut idle = 0;
      while self.sem.try_wait() {
         idle += 1;
      }
      // tokens ghc holds = held - idle. Put the spare back only while a slot
      // backs it, else a waiter could take a token nobody admitted
      let in_use = self.held.len().saturating_sub(idle);
      self.held.truncate(in_use + 1);
      let spare = self.held.len() == in_use + 1;
      if let Some(token) = self.orders.poll(!spare) {
         self.held.push(token);
      }
      if self.held.len() > in_use {
         self.sem.post();
      }
   }
}

impl Drop for Broker {
   fn drop(&mut self) {
      for _ in 0..self.held.len() {
         if !self.sem.try_wait() {
            break;
         }
      }
   }
}
