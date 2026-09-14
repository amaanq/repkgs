use std::{
   collections::HashMap,
   io,
   os::unix::net::UnixStream,
   sync::{
      Condvar,
      Mutex,
   },
   time::Duration,
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

struct Waiter {
   ticket: u64,
   build:  String,
}

#[derive(Default)]
struct State {
   next_ticket: u64,
   occupied:    usize,
   held:        HashMap<String, usize>,
   waiting:     Vec<Waiter>,
   stopped:     bool,
}

pub struct Slots {
   limit:   usize,
   state:   Mutex<State>,
   changed: Condvar,
}

impl Slots {
   pub fn new(limit: usize) -> Self {
      assert!(limit > 0, "the slot pool must admit at least one compiler");
      Self {
         limit,
         state: Mutex::new(State::default()),
         changed: Condvar::new(),
      }
   }

   pub fn acquire(&self, build: &str, stream: &UnixStream) -> io::Result<()> {
      let mut state = self
         .state
         .lock()
         .map_err(|_error| io::Error::other("slot lock poisoned"))?;
      let ticket = state.next_ticket;
      state.next_ticket += 1;
      state.waiting.push(Waiter {
         ticket,
         build: build.to_owned(),
      });

      loop {
         let disconnected = peer_closed(stream);
         if state.stopped || !matches!(disconnected, Ok(false)) {
            state.waiting.retain(|waiter| waiter.ticket != ticket);
            self.changed.notify_all();
            return match disconnected {
               Err(error) => Err(error),
               Ok(_) => {
                  Err(io::Error::new(
                     io::ErrorKind::ConnectionAborted,
                     "slot request cancelled",
                  ))
               },
            };
         }

         let selected = state.waiting.iter().enumerate().min_by_key(|&(_, waiter)| {
            (
               state.held.get(&waiter.build).copied().unwrap_or(0),
               waiter.ticket,
            )
         });
         if state.occupied < self.limit
            && let Some((position, waiter)) = selected
            && waiter.ticket == ticket
         {
            state.waiting.remove(position);
            state.occupied += 1;
            *state.held.entry(build.to_owned()).or_default() += 1;
            self.changed.notify_all();
            return Ok(());
         }

         let (next_state, _) = self
            .changed
            .wait_timeout(state, Duration::from_millis(100))
            .map_err(|_error| io::Error::other("slot lock poisoned"))?;
         state = next_state;
      }
   }

   #[expect(
      clippy::expect_used,
      reason = "release requires a slot owned by the build"
   )]
   pub fn release(&self, build: &str, count: usize) {
      let mut state = self.state.lock().expect("slot lock poisoned");
      let held = state.held.get_mut(build).expect("released an unowned slot");
      assert!(*held >= count, "released more slots than the build owns");
      *held -= count;
      if *held == 0 {
         state.held.remove(build);
      }
      state.occupied -= count;
      drop(state);
      self.changed.notify_all();
   }

   #[expect(
      clippy::expect_used,
      reason = "poisoned slot state violates scheduling invariants"
   )]
   pub fn stats(&self) -> (usize, usize) {
      let state = self.state.lock().expect("slot lock poisoned");
      (state.occupied, state.waiting.len())
   }

   #[expect(
      clippy::expect_used,
      reason = "poisoned slot state violates scheduling invariants"
   )]
   pub fn stop(&self) {
      let mut state = self.state.lock().expect("slot lock poisoned");
      state.stopped = true;
      drop(state);
      self.changed.notify_all();
   }
}

fn peer_closed(stream: &UnixStream) -> io::Result<bool> {
   let mut descriptors = [PollFd::new(stream, PollFlags::RDHUP)];
   let timeout = Timespec {
      tv_sec:  0,
      tv_nsec: 0,
   };
   match poll(&mut descriptors, Some(&timeout)) {
      Ok(_) => {
         Ok(descriptors[0]
            .revents()
            .intersects(PollFlags::HUP | PollFlags::RDHUP | PollFlags::ERR | PollFlags::NVAL))
      },
      Err(Errno::INTR) => Ok(false),
      Err(error) => Err(error.into()),
   }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "tests")]
mod tests {
   use std::{
      os::unix::net::UnixStream,
      sync::{
         Arc,
         mpsc,
      },
      thread,
      time::{
         Duration,
         Instant,
      },
   };

   use crate::slots::Slots;

   #[test]
   fn slots_fewest_held_first() {
      let slots = Arc::new(Slots::new(2));
      let (stream, _peer) = UnixStream::pair().expect("socket pair");
      slots.acquire("big", &stream).expect("first slot");
      slots.acquire("big", &stream).expect("second slot");
      let (sender, receiver) = mpsc::channel();
      let wait_for_queue = |length| {
         let deadline = Instant::now() + Duration::from_secs(5);
         while slots.stats().1 != length {
            assert!(Instant::now() < deadline, "slot requests did not queue");
            thread::sleep(Duration::from_millis(1));
         }
      };
      let mut workers = Vec::new();
      for (position, build) in ["big", "small"].into_iter().enumerate() {
         let pool = Arc::clone(&slots);
         let connection = stream.try_clone().expect("copy socket handle");
         let replies = sender.clone();
         workers.push(thread::spawn(move || {
            pool.acquire(build, &connection).expect("queued slot");
            replies.send(build).expect("report granted slot");
         }));
         wait_for_queue(position + 1);
      }
      assert_eq!(slots.stats(), (2, 2));
      slots.release("big", 1);
      assert_eq!(
         receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("first grant"),
         "small"
      );
      slots.release("big", 1);
      assert_eq!(
         receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("second grant"),
         "big"
      );
      for worker in workers {
         worker.join().expect("slot worker panicked");
      }
      slots.release("small", 1);
      slots.release("big", 1);
   }
}
