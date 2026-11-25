//! One request for callers that overlap (#2401): the apps read the meeting list from the meeting
//! watch, the meetings screen and its upkeep, often at the same moment (a foreground, a knock).
//!
//! A request runs at a time. Whoever asks while one runs waits for the next, which starts once the
//! running one ended and answers everybody who asked meanwhile: nobody gets an answer older than
//! their question, and a burst of asks costs at most two requests. The request is driven by one of
//! its callers (the apps' async calls run on their own executors, not on a runtime of ours); when
//! that caller is cancelled, its followers ask again and one of them drives the next.

use std::future::Future;
use std::sync::{Mutex, PoisonError};

use tokio::sync::oneshot;

/// Runs one request at a time for all callers of [`Self::run`] that overlap.
pub(crate) struct Coalesced<T> {
  state: Mutex<State<T>>,
}

/// What the callers share.
struct State<T> {
  /// A request runs.
  is_running: bool,
  /// Who asked while it runs: the next request answers them.
  next: Vec<oneshot::Sender<Turn<T>>>,
}

/// What a waiting caller is told.
enum Turn<T> {
  /// The request it waited for answered.
  Done(T),
  /// It drives the request, for itself and these followers.
  Lead(Vec<oneshot::Sender<Turn<T>>>),
}

/// Ends a lead however it ends: hands the next request on, or marks none running. Followers that
/// it still holds (its caller was cancelled mid-request) hear nothing and ask again.
struct Leading<'a, T> {
  coalesced: &'a Coalesced<T>,
  followers: Vec<oneshot::Sender<Turn<T>>>,
}

impl<T: Clone> Coalesced<T> {
  pub(crate) fn new() -> Self {
    Self {
      state: Mutex::new(State {
        is_running: false,
        next: Vec::new(),
      }),
    }
  }

  /// The answer of a request `request` started after this call: this caller's own, or one shared
  /// with the callers that asked while the previous request ran.
  pub(crate) async fn run<F, Fut>(&self, request: F) -> T
  where
    F: Fn() -> Fut,
    Fut: Future<Output = T>,
  {
    loop {
      let Some(waiting) = self.join() else {
        return self.lead(Vec::new(), &request).await;
      };
      match waiting.await {
        Ok(Turn::Done(answer)) => return answer,
        Ok(Turn::Lead(followers)) => return self.lead(followers, &request).await,
        // The caller that drove the request was cancelled: ask again.
        Err(_) => {},
      }
    }
  }

  /// Waits for the next request while one runs; `None` when none runs, and this caller now drives
  /// one.
  fn join(&self) -> Option<oneshot::Receiver<Turn<T>>> {
    let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
    if !state.is_running {
      state.is_running = true;
      return None;
    }
    let (turn, waiting) = oneshot::channel();
    state.next.push(turn);
    Some(waiting)
  }

  /// Runs the request for this caller and `followers`, then hands the next one on.
  async fn lead<F, Fut>(&self, followers: Vec<oneshot::Sender<Turn<T>>>, request: &F) -> T
  where
    F: Fn() -> Fut,
    Fut: Future<Output = T>,
  {
    let mut lead = Leading {
      coalesced: self,
      followers,
    };
    let answer = request().await;
    for follower in std::mem::take(&mut lead.followers) {
      // A follower that stopped waiting needs no answer.
      let _ = follower.send(Turn::Done(answer.clone()));
    }
    answer
  }
}

impl<T> Drop for Leading<'_, T> {
  fn drop(&mut self) {
    // Followers still held here were not answered: dropping them makes them ask again.
    self.followers.clear();
    hand_on(self.coalesced);
  }
}

/// The running request ended: the callers that asked meanwhile get the next, driven by the first
/// of them still waiting; with none, no request runs.
fn hand_on<T>(coalesced: &Coalesced<T>) {
  let mut state = coalesced.state.lock().unwrap_or_else(PoisonError::into_inner);
  let mut next = std::mem::take(&mut state.next);
  while !next.is_empty() {
    let leader = next.remove(0);
    match leader.send(Turn::Lead(next)) {
      Ok(()) => return,
      // That caller is gone: the next in line drives it.
      Err(Turn::Lead(rest)) => next = rest,
      Err(Turn::Done(_)) => return,
    }
  }
  state.is_running = false;
}

#[cfg(test)]
mod tests {
  use std::cell::Cell;
  use std::pin::pin;
  use std::task::{Context, Poll, Waker};

  use super::*;

  #[test]
  fn callers_that_ask_while_a_request_runs_share_the_next() {
    let coalesced = Coalesced::new();
    let started = Cell::new(0);
    let gates: [Gate; 2] = [Gate::new(), Gate::new()];
    let request = || {
      let index = started.get();
      started.set(index + 1);
      let gate = gates[index].clone_rx();
      async move {
        gate.await.unwrap();
        index
      }
    };

    let mut first = pin!(coalesced.run(&request));
    assert!(poll(first.as_mut()).is_pending());
    let mut second = pin!(coalesced.run(&request));
    let mut third = pin!(coalesced.run(&request));
    assert!(poll(second.as_mut()).is_pending());
    assert!(poll(third.as_mut()).is_pending());
    assert_eq!(started.get(), 1, "the others wait for the next request");

    gates[0].open();
    assert_eq!(poll(first.as_mut()), Poll::Ready(0));
    assert!(
      poll(second.as_mut()).is_pending(),
      "the second drives the next one"
    );
    assert!(poll(third.as_mut()).is_pending());
    assert_eq!(started.get(), 2, "one request for both");

    gates[1].open();
    assert_eq!(poll(second.as_mut()), Poll::Ready(1));
    assert_eq!(
      poll(third.as_mut()),
      Poll::Ready(1),
      "answered by the second's request"
    );

    let mut later = pin!(coalesced.run(|| async { 9 }));
    assert_eq!(poll(later.as_mut()), Poll::Ready(9), "none runs any more");
  }

  #[test]
  fn a_cancelled_leader_hands_its_followers_on() {
    let coalesced = Coalesced::new();
    let gate = Gate::new();
    let started = Cell::new(0);
    let request = || {
      started.set(started.get() + 1);
      let gate = gate.clone_rx();
      async move {
        gate.await.unwrap();
        7
      }
    };

    let mut leader = Box::pin(coalesced.run(&request));
    assert!(poll(leader.as_mut()).is_pending());
    let mut follower = pin!(coalesced.run(&request));
    assert!(poll(follower.as_mut()).is_pending());

    drop(leader);
    assert!(
      poll(follower.as_mut()).is_pending(),
      "it runs the request again itself"
    );
    assert_eq!(started.get(), 2);
    gate.open();
    assert_eq!(poll(follower.as_mut()), Poll::Ready(7));
  }

  fn poll<F: Future>(future: std::pin::Pin<&mut F>) -> Poll<F::Output> {
    future.poll(&mut Context::from_waker(Waker::noop()))
  }

  /// A signal every request waiting on it gets once it opens.
  struct Gate(tokio::sync::watch::Sender<bool>);

  impl Gate {
    fn new() -> Self {
      Self(tokio::sync::watch::Sender::new(false))
    }

    fn clone_rx(&self) -> impl Future<Output = Result<(), tokio::sync::watch::error::RecvError>> + use<> {
      let mut receiver = self.0.subscribe();
      async move { receiver.wait_for(|is_open| *is_open).await.map(drop) }
    }

    fn open(&self) {
      self.0.send_replace(true);
    }
  }
}
