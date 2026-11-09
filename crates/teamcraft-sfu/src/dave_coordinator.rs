//! Orders DAVE membership changes for one call, like Discord's voice gateway.
//!
//! The coordinator is the MLS external sender: it turns joins and leaves into proposals, sends them
//! to every member, takes the first commit it receives for the epoch and announces it to everyone
//! (welcome to the joiners). Only one transition is in flight; changes that arrive meanwhile are
//! batched into the next one. It holds no media keys.
//!
//! The first two participants found the group: the earlier one keeps its pending single-member
//! group (epoch 0) and commits the other's Add proposal.
//!
//! Every later commit carries a transition id (DAVE opcodes 29/30 with `transition_id`) for the
//! members that joined with [`DaveTransitionMode::Coordinated`]: they install the new epoch's
//! decryption keys, say they are ready, and the coordinator executes the transition (opcode 22)
//! once all of them are, or have left, or its deadline passed (a straggler that never says it is
//! ready holds nobody up for longer). Only then do they encrypt with the new key, so nobody
//! misses frames at the switch. Clients from before transitions get no id and switch at once. The
//! founding commit has none either: nobody encrypted before it. The coordinator's word decides
//! nothing about keys: a client that hears no execution switches on its own after a while.
//!
//! The next proposals wait until the announced transition was executed (#1544): proposing at once
//! would make the next commit flush the execution before most members are ready, and they'd lose
//! frames at the switch. The execution's deadline bounds that wait.
//!
//! Pure state machine: every input returns the messages to send, and time comes in as an argument
//! ([`DaveCoordinator::on_timer`]), so it is tested without network or clock.

use std::{
  collections::{BTreeSet, HashMap},
  num::NonZeroU16,
  time::{Duration, Instant},
};

use teamcraft_media::dave::{DaveEpoch, DaveError, DaveExternalSender, DaveLeafIndex, DaveParticipantId};
use teamcraft_proto::{
  Base64Bytes, CallEvent, CallId, DaveKeyBinding, DaveTransitionId, DaveTransitionMode, DeviceId, IdError,
};
use thiserror::Error;
use tracing::{debug, info, warn};

use crate::ids::{dave_call_id, dave_participant_id, device_id_from_dave};

/// DAVE membership state of one call.
pub struct DaveCoordinator {
  call_id: CallId,
  /// How long an announced transition waits for its members' ready before it is executed anyway.
  transition_deadline: Duration,
  external_sender: DaveExternalSender,
  /// `None` until the group is founded (and again after everyone left).
  maybe_epoch: Option<DaveEpoch>,
  /// Leaves of the MLS ratchet tree, mirrored to address Remove proposals. MLS places new members
  /// into the leftmost blank leaf (RFC 9420 §7.7), which this reproduces.
  leaves: Vec<Option<DeviceId>>,
  /// Key packages not yet proposed, in arrival order.
  waiting: Vec<(DeviceId, Vec<u8>)>,
  /// Signed call key bindings as sent with the key packages (e2ee.md §8); relayed, never trusted.
  key_bindings: HashMap<DeviceId, Vec<u8>>,
  pending_removals: Vec<DaveLeafIndex>,
  maybe_transition: Option<Transition>,
  /// How each device with a key package applies transitions (from its `join`).
  transition_modes: HashMap<DeviceId, DaveTransitionMode>,
  /// The last transition id handed out; ids count up from 1 and wrap, skipping 0.
  last_transition_id: u16,
  /// The announced transition waiting for its members to be ready.
  maybe_execution: Option<Execution>,
}

/// One message to deliver over a participant's signaling stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outbound {
  pub recipient: DeviceId,
  pub event: CallEvent,
}

/// An announced commit's transition, waiting until every coordinated member is ready.
#[derive(Debug)]
struct Execution {
  id: DaveTransitionId,
  /// Coordinated members that got the commit or welcome; they hear the execution.
  recipients: BTreeSet<DeviceId>,
  /// Of those, the ones that haven't said they're ready.
  awaiting: BTreeSet<DeviceId>,
  /// When it is executed whether or not everyone is ready.
  deadline: Instant,
}

/// Proposals sent for `epoch`, waiting for the first commit.
#[derive(Debug)]
struct Transition {
  epoch: DaveEpoch,
  joiners: Vec<(DeviceId, Vec<u8>)>,
  removed: Vec<DaveLeafIndex>,
  committers: BTreeSet<DeviceId>,
  departed_joiners: BTreeSet<DeviceId>,
}

#[derive(Debug, Error)]
pub enum DaveCoordinatorError {
  #[error(transparent)]
  Dave(#[from] DaveError),
  #[error("key package belongs to participant {actual:?}, not {expected:?}")]
  ParticipantMismatch { expected: DeviceId, actual: DeviceId },
  #[error("invalid participant id: {0}")]
  InvalidParticipantId(#[from] IdError),
  #[error("participant {0:?} already has a key package in the group or queue")]
  AlreadyKnown(DeviceId),
}

impl DaveCoordinator {
  /// A coordinator that executes each transition at most `transition_deadline` after announcing it.
  pub fn new(call_id: CallId, transition_deadline: Duration) -> Result<Self, DaveCoordinatorError> {
    Ok(Self {
      call_id,
      transition_deadline,
      external_sender: DaveExternalSender::new()?,
      maybe_epoch: None,
      leaves: Vec::new(),
      waiting: Vec::new(),
      key_bindings: HashMap::new(),
      pending_removals: Vec::new(),
      maybe_transition: None,
      transition_modes: HashMap::new(),
      last_transition_id: 0,
      maybe_execution: None,
    })
  }

  /// The `ExternalSender` package every participant installs on join.
  pub fn external_sender_package(&self) -> Result<Vec<u8>, DaveCoordinatorError> {
    Ok(self.external_sender.package()?)
  }

  /// A participant's key package arrived; `transition_mode` is what its connection's `join` said.
  pub fn on_key_package(
    &mut self,
    participant_id: DeviceId,
    key_package: Vec<u8>,
    maybe_key_binding: Option<Vec<u8>>,
    transition_mode: DaveTransitionMode,
  ) -> Result<Vec<Outbound>, DaveCoordinatorError> {
    let actual = device_id_from_dave(self.external_sender.key_package_participant(&key_package)?)?;
    if actual != participant_id {
      return Err(DaveCoordinatorError::ParticipantMismatch {
        expected: participant_id,
        actual,
      });
    }
    if self.is_known(participant_id) {
      return Err(DaveCoordinatorError::AlreadyKnown(participant_id));
    }
    if let Some(binding) = maybe_key_binding {
      self.key_bindings.insert(participant_id, binding);
    }
    self.transition_modes.insert(participant_id, transition_mode);
    self.waiting.push((participant_id, key_package));
    Ok(self.start_transition())
  }

  /// A participant committed the proposals of `epoch`. Late or foreign commits are ignored.
  pub fn on_commit(
    &mut self,
    participant_id: DeviceId,
    epoch: DaveEpoch,
    commit: Vec<u8>,
    maybe_welcome: Option<Vec<u8>>,
    now: Instant,
  ) -> Vec<Outbound> {
    let Some(transition) = &self.maybe_transition else {
      debug!(?participant_id, "commit without a transition in flight");
      return Vec::new();
    };
    let is_valid = transition.epoch == epoch
      && transition.committers.contains(&participant_id)
      && (transition.joiners.is_empty() || maybe_welcome.is_some());
    if !is_valid {
      debug!(?participant_id, epoch = epoch.0, "ignoring commit");
      return Vec::new();
    }
    let Some(transition) = self.maybe_transition.take() else {
      return Vec::new();
    };
    let mut outbound = self.apply(transition, commit, maybe_welcome, now);
    outbound.extend(self.start_transition());
    outbound
  }

  /// A member installed the keys of `transition_id`. The last one to do so executes it.
  pub fn on_transition_ready(
    &mut self,
    participant_id: DeviceId,
    transition_id: DaveTransitionId,
  ) -> Vec<Outbound> {
    let Some(execution) = &mut self.maybe_execution else {
      debug!(?participant_id, "transition ready without a transition in flight");
      return Vec::new();
    };
    if execution.id != transition_id {
      debug!(
        ?participant_id,
        transition_id = transition_id.0.get(),
        "ignoring ready for another transition"
      );
      return Vec::new();
    }
    execution.awaiting.remove(&participant_id);
    let mut outbound = self.execute_when_ready();
    outbound.extend(self.start_transition());
    outbound
  }

  /// Periodic check (the room's timer): executes a transition whose deadline passed, logging how
  /// many members weren't ready, and starts the proposals that waited for it.
  pub fn on_timer(&mut self, now: Instant) -> Vec<Outbound> {
    let is_overdue = self
      .maybe_execution
      .as_ref()
      .is_some_and(|execution| now >= execution.deadline);
    if !is_overdue {
      return Vec::new();
    }
    let mut outbound = self.execute_now();
    outbound.extend(self.start_transition());
    outbound
  }

  /// A participant left the call (or its connection dropped).
  pub fn on_leave(&mut self, participant_id: DeviceId) -> Vec<Outbound> {
    self.waiting.retain(|(id, _)| *id != participant_id);
    self.key_bindings.remove(&participant_id);
    self.transition_modes.remove(&participant_id);
    let mut outbound = Vec::new();
    if let Some(execution) = &mut self.maybe_execution {
      execution.recipients.remove(&participant_id);
      execution.awaiting.remove(&participant_id);
      outbound = self.execute_when_ready();
    }
    if let Some(leaf_index) = self.leaf_of(participant_id) {
      self.leaves[leaf_index] = None;
      let leaf = DaveLeafIndex(u32::try_from(leaf_index).unwrap_or(u32::MAX));
      let is_already_removed = self
        .maybe_transition
        .as_ref()
        .is_some_and(|t| t.removed.contains(&leaf));
      if !is_already_removed {
        self.pending_removals.push(leaf);
      }
    }
    if let Some(transition) = &mut self.maybe_transition {
      transition.committers.remove(&participant_id);
      if transition.joiners.iter().any(|(id, _)| *id == participant_id) {
        transition.departed_joiners.insert(participant_id);
      }
    }
    if self.leaves.iter().all(Option::is_none) {
      self.reset_group();
    }
    outbound.extend(self.start_transition());
    outbound
  }

  /// Current epoch of the group, if founded.
  pub fn maybe_epoch(&self) -> Option<DaveEpoch> {
    self.maybe_epoch
  }

  fn start_transition(&mut self) -> Vec<Outbound> {
    // One change at a time: proposals wait while a commit or an execution is outstanding.
    if self.maybe_transition.is_some() || self.maybe_execution.is_some() {
      return Vec::new();
    }
    match self.maybe_epoch {
      None => self.found_group(),
      Some(epoch) if !self.waiting.is_empty() || !self.pending_removals.is_empty() => {
        let joiners = std::mem::take(&mut self.waiting);
        let removed = std::mem::take(&mut self.pending_removals);
        self.propose(epoch, joiners, removed)
      },
      Some(_) => Vec::new(),
    }
  }

  fn found_group(&mut self) -> Vec<Outbound> {
    if self.waiting.len() < 2 {
      return Vec::new();
    }
    let mut joiners = std::mem::take(&mut self.waiting);
    let (founder, _) = joiners.remove(0);
    self.leaves = vec![Some(founder)];
    self.maybe_epoch = Some(DaveEpoch(0));
    debug!(call_id = %self.call_id, ?founder, "founding DAVE group");
    self.propose(DaveEpoch(0), joiners, Vec::new())
  }

  fn propose(
    &mut self,
    epoch: DaveEpoch,
    joiners: Vec<(DeviceId, Vec<u8>)>,
    removed: Vec<DaveLeafIndex>,
  ) -> Vec<Outbound> {
    let joiner_refs: Vec<(DaveParticipantId, &[u8])> = joiners
      .iter()
      .map(|(id, key_package)| (dave_participant_id(*id), key_package.as_slice()))
      .collect();
    let proposals =
      match self
        .external_sender
        .propose(dave_call_id(self.call_id), epoch, &joiner_refs, &removed)
      {
        Ok(proposals) => proposals,
        Err(error) => {
          // Key packages are validated on arrival, so this is unexpected; drop the batch rather than
          // wedge the call.
          warn!(call_id = %self.call_id, %error, "creating DAVE proposals failed");
          return Vec::new();
        },
      };
    let committers: BTreeSet<DeviceId> = self.members().collect();
    let expected_participants: Vec<DeviceId> = committers
      .iter()
      .copied()
      .chain(joiners.iter().map(|(id, _)| *id))
      .collect();
    let outbound = committers
      .iter()
      .map(|&recipient| Outbound {
        recipient,
        event: CallEvent::DaveProposals {
          epoch: epoch.0,
          proposals: Base64Bytes::new(proposals.clone()),
          expected_participants: expected_participants.clone(),
          key_bindings: self.bindings_of(joiners.iter().map(|(id, _)| *id)),
        },
      })
      .collect();
    self.maybe_transition = Some(Transition {
      epoch,
      joiners,
      removed,
      committers,
      departed_joiners: BTreeSet::new(),
    });
    outbound
  }

  fn apply(
    &mut self,
    transition: Transition,
    commit: Vec<u8>,
    maybe_welcome: Option<Vec<u8>>,
    now: Instant,
  ) -> Vec<Outbound> {
    // Proposals wait for the previous execution, so there is none here; should one be left, it goes
    // now, before the next epoch's commit on the same ordered streams.
    let mut outbound = self.execute_now();
    let maybe_id = self.next_transition_id(&transition);
    // Members before the joiners are placed get the commit; joiners get the welcome instead.
    let members: Vec<DeviceId> = self.members().collect();
    for recipient in members {
      outbound.push(Outbound {
        recipient,
        event: CallEvent::DaveAnnounceCommit {
          epoch: transition.epoch.0,
          commit: Base64Bytes::new(commit.clone()),
          maybe_transition_id: self.transition_for(recipient, maybe_id),
        },
      });
    }
    let mut welcomed = Vec::new();
    for (joiner, _) in &transition.joiners {
      let leaf_index = self.place(*joiner);
      if transition.departed_joiners.contains(joiner) {
        self.leaves[leaf_index] = None;
        self
          .pending_removals
          .push(DaveLeafIndex(u32::try_from(leaf_index).unwrap_or(u32::MAX)));
      } else {
        welcomed.push(*joiner);
      }
    }
    if let Some(welcome) = &maybe_welcome {
      for joiner in welcomed {
        // The joiner verifies everyone already in the group (including other joiners).
        let others: Vec<DeviceId> = self.members().filter(|member| *member != joiner).collect();
        outbound.push(Outbound {
          recipient: joiner,
          event: CallEvent::DaveWelcome {
            welcome: Base64Bytes::new(welcome.clone()),
            key_bindings: self.bindings_of(others),
            maybe_transition_id: self.transition_for(joiner, maybe_id),
          },
        });
      }
    }
    while self.leaves.last().is_some_and(Option::is_none) {
      self.leaves.pop();
    }
    self.maybe_epoch = Some(DaveEpoch(transition.epoch.0 + 1));
    if let Some(id) = maybe_id {
      let recipients: BTreeSet<DeviceId> = outbound
        .iter()
        .filter(|message| {
          matches!(
            message.event,
            CallEvent::DaveAnnounceCommit {
              maybe_transition_id: Some(_),
              ..
            } | CallEvent::DaveWelcome {
              maybe_transition_id: Some(_),
              ..
            }
          )
        })
        .map(|message| message.recipient)
        .collect();
      self.maybe_execution = Some(Execution {
        id,
        awaiting: recipients.clone(),
        recipients,
        deadline: now + self.transition_deadline,
      });
      outbound.extend(self.execute_when_ready());
    }
    outbound
  }

  /// A fresh id for a commit's transition; none for the founding commit (nobody encrypted before
  /// it) or when no member takes transitions.
  fn next_transition_id(&mut self, transition: &Transition) -> Option<DaveTransitionId> {
    let is_founding = transition.epoch == DaveEpoch(0);
    let has_coordinated = self
      .members()
      .chain(transition.joiners.iter().map(|(id, _)| *id))
      .any(|device| self.is_coordinated(device));
    if is_founding || !has_coordinated {
      return None;
    }
    self.last_transition_id = self.last_transition_id.checked_add(1).unwrap_or(1);
    // Invariant: the counter is at least 1 here.
    NonZeroU16::new(self.last_transition_id).map(DaveTransitionId)
  }

  fn transition_for(
    &self,
    recipient: DeviceId,
    maybe_id: Option<DaveTransitionId>,
  ) -> Option<DaveTransitionId> {
    maybe_id.filter(|_| self.is_coordinated(recipient))
  }

  fn is_coordinated(&self, device: DeviceId) -> bool {
    self.transition_modes.get(&device) == Some(&DaveTransitionMode::Coordinated)
  }

  /// Executes the transition in flight once nobody it waits for is left.
  fn execute_when_ready(&mut self) -> Vec<Outbound> {
    if self
      .maybe_execution
      .as_ref()
      .is_some_and(|execution| execution.awaiting.is_empty())
    {
      return self.execute_now();
    }
    Vec::new()
  }

  /// Executes the transition in flight, if any, ready or not.
  fn execute_now(&mut self) -> Vec<Outbound> {
    let Some(execution) = self.maybe_execution.take() else {
      return Vec::new();
    };
    if !execution.awaiting.is_empty() {
      // A count only: which members lag says nothing useful in a log and is nobody else's business.
      info!(
        call_id = %self.call_id,
        not_ready = execution.awaiting.len(),
        "executing a DAVE transition before every member is ready"
      );
    }
    execution
      .recipients
      .into_iter()
      .map(|recipient| Outbound {
        recipient,
        event: CallEvent::DaveExecuteTransition {
          transition_id: execution.id,
        },
      })
      .collect()
  }

  /// Puts a new member into the leftmost blank leaf and returns its index.
  fn place(&mut self, participant_id: DeviceId) -> usize {
    if let Some(index) = self.leaves.iter().position(Option::is_none) {
      self.leaves[index] = Some(participant_id);
      return index;
    }
    self.leaves.push(Some(participant_id));
    self.leaves.len() - 1
  }

  /// Everyone left: forget the group. Joiners of an aborted transition queue up again; their
  /// pending groups are still at epoch 0, so one of them can found the next group.
  fn reset_group(&mut self) {
    self.maybe_epoch = None;
    self.leaves.clear();
    self.pending_removals.clear();
    self.maybe_execution = None;
    if let Some(transition) = self.maybe_transition.take() {
      let mut requeued: Vec<_> = transition
        .joiners
        .into_iter()
        .filter(|(id, _)| !transition.departed_joiners.contains(id))
        .collect();
      requeued.append(&mut self.waiting);
      self.waiting = requeued;
    }
  }

  fn bindings_of(&self, devices: impl IntoIterator<Item = DeviceId>) -> Vec<DaveKeyBinding> {
    devices
      .into_iter()
      .filter_map(|device_id| {
        let binding = self.key_bindings.get(&device_id)?;
        Some(DaveKeyBinding {
          device_id,
          binding: Base64Bytes::new(binding.clone()),
        })
      })
      .collect()
  }

  fn members(&self) -> impl Iterator<Item = DeviceId> + '_ {
    self.leaves.iter().flatten().copied()
  }

  fn leaf_of(&self, participant_id: DeviceId) -> Option<usize> {
    self.leaves.iter().position(|leaf| *leaf == Some(participant_id))
  }

  fn is_known(&self, participant_id: DeviceId) -> bool {
    self.leaf_of(participant_id).is_some()
      || self.waiting.iter().any(|(id, _)| *id == participant_id)
      || self.maybe_transition.as_ref().is_some_and(|t| {
        // A joiner that left during the transition may come back with a new key package.
        !t.departed_joiners.contains(&participant_id) && t.joiners.iter().any(|(id, _)| *id == participant_id)
      })
  }
}

#[cfg(test)]
mod tests {
  use std::collections::{BTreeMap, BTreeSet, VecDeque};

  use teamcraft_media::dave::{DaveParticipant, FrameCodec, MediaKind};

  use super::*;

  const FRAME: &[u8] = &[0x78, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
  const DEADLINE: Duration = Duration::from_secs(2);

  mod membership {
    use super::*;

    #[test]
    fn single_participant_waits() {
      let mut harness = Harness::new();
      harness.join(1);
      assert_eq!(harness.coordinator.maybe_epoch(), None);
      assert!(!harness.clients[&device(1)].is_ready());
    }

    #[test]
    fn three_participants_establish_the_group() {
      let mut harness = Harness::new();
      for id in 1..=3 {
        harness.join(id);
      }
      harness.assert_all_exchange_frames(&[1, 2, 3]);
      assert_eq!(harness.coordinator.maybe_epoch(), Some(DaveEpoch(2)));
    }

    #[test]
    fn simultaneous_joins_are_batched() {
      let mut harness = Harness::new();
      // All key packages arrive before any commit is delivered.
      for id in 1..=4 {
        harness.join_without_delivery(id);
      }
      harness.deliver_all();
      harness.assert_all_exchange_frames(&[1, 2, 3, 4]);
      // Founding (epoch 0 → 1) carried participant 2; 3 and 4 were batched into epoch 1 → 2.
      assert_eq!(harness.coordinator.maybe_epoch(), Some(DaveEpoch(2)));
    }

    #[test]
    fn leave_removes_member_and_join_reuses_its_leaf() {
      let mut harness = Harness::new();
      for id in 1..=3 {
        harness.join(id);
      }
      harness.leave(2);
      harness.assert_all_exchange_frames(&[1, 3]);
      harness.join(4);
      harness.assert_all_exchange_frames(&[1, 3, 4]);
      assert_eq!(
        harness.coordinator.leaves,
        vec![Some(device(1)), Some(device(4)), Some(device(3))]
      );
    }

    #[test]
    fn founder_leaving_before_commit_lets_the_next_one_found() {
      let mut harness = Harness::new();
      harness.join_without_delivery(1);
      harness.join_without_delivery(2);
      harness.join_without_delivery(3);
      harness.leave(1);
      harness.deliver_all();
      harness.assert_all_exchange_frames(&[2, 3]);
    }

    #[test]
    fn member_that_reconnects_rejoins_with_a_new_key_package() {
      let mut harness = Harness::new();
      for id in 1..=3 {
        harness.join(id);
      }
      harness.leave(2);
      harness.join(2);
      harness.assert_all_exchange_frames(&[1, 2, 3]);
    }

    #[test]
    fn joiner_that_drops_mid_transition_can_join_again() {
      let mut harness = Harness::new();
      harness.join(1);
      harness.join(2);
      // Proposals adding 3 are in flight when its connection drops; it is back at once.
      harness.join_without_delivery(3);
      harness.clients.remove(&device(3));
      let outbound = harness.coordinator.on_leave(device(3));
      harness.queue.extend(outbound);
      harness.join_without_delivery(3);
      harness.deliver_all();
      harness.assert_all_exchange_frames(&[1, 2, 3]);
    }

    #[test]
    fn everyone_leaving_resets_the_group() {
      let mut harness = Harness::new();
      harness.join(1);
      harness.join(2);
      harness.leave(1);
      harness.leave(2);
      assert_eq!(harness.coordinator.maybe_epoch(), None);
      harness.join(3);
      harness.join(4);
      harness.assert_all_exchange_frames(&[3, 4]);
    }
  }

  mod transitions {
    use super::*;

    /// The founding commit switches at once; later ones wait for every member's ready, then
    /// everyone hears the execution once (#1541).
    #[test]
    fn later_commits_execute_once_everyone_is_ready() {
      let mut harness = Harness::new();
      harness.join(1);
      harness.join(2);
      assert!(harness.delivered.iter().all(|outbound| !matches!(
        outbound.event,
        CallEvent::DaveAnnounceCommit {
          maybe_transition_id: Some(_),
          ..
        } | CallEvent::DaveWelcome {
          maybe_transition_id: Some(_),
          ..
        }
      )));
      harness.holds_ready.insert(device(2));
      harness.join(3);
      assert_eq!(harness.executions_to(1), 0, "member 2 isn't ready yet");
      assert_eq!(harness.executions_to(3), 0);

      harness.release_ready();

      for id in 1..=3 {
        assert_eq!(harness.executions_to(id), 1);
      }
      harness.assert_all_exchange_frames(&[1, 2, 3]);
    }

    /// The next proposals wait until the transition in flight was executed (#1544), so its members
    /// switch once they are ready rather than when the next commit happens to arrive.
    #[test]
    fn the_next_proposals_wait_for_the_execution() {
      let mut harness = Harness::new();
      harness.join(1);
      harness.join(2);
      harness.holds_ready.insert(device(2));
      harness.join(3);
      let before = harness.delivered.len();

      harness.join(4);

      assert_eq!(
        harness.delivered.len(),
        before,
        "no proposals while 2 isn't ready"
      );
      assert_eq!(harness.executions_to(1), 0);
      harness.release_ready();

      let first_id = harness.first_execution_id().unwrap();
      for id in 1..=3 {
        let events = harness.events_to(id);
        let executed = events
          .iter()
          .position(|event| matches!(event, CallEvent::DaveExecuteTransition { transition_id } if *transition_id == first_id))
          .unwrap();
        let next_proposals = events
          .iter()
          .rposition(|event| matches!(event, CallEvent::DaveProposals { .. }))
          .unwrap();
        assert!(
          executed < next_proposals,
          "member {id} executes before the next proposals"
        );
      }
      harness.assert_all_exchange_frames(&[1, 2, 3, 4]);
    }

    /// A member that never says it is ready holds the transition until the deadline only, then
    /// everyone switches and the joiner can talk (#1544).
    #[test]
    fn a_straggler_is_executed_over_at_the_deadline() {
      let mut harness = Harness::new();
      harness.join(1);
      harness.join(2);
      harness.holds_ready.insert(device(2));
      harness.join(3);

      harness.advance(DEADLINE - Duration::from_millis(1));
      assert_eq!(harness.executions_to(3), 0, "before the deadline");
      harness.advance(Duration::from_millis(1));

      for id in 1..=3 {
        assert_eq!(harness.executions_to(id), 1, "member {id}");
      }
      harness.assert_all_exchange_frames(&[1, 2, 3]);
      // The straggler's late ready and further ticks change nothing.
      let delivered = harness.delivered.len();
      harness.release_ready();
      harness.advance(DEADLINE);
      assert_eq!(harness.delivered.len(), delivered);
    }

    /// A straggler delays a waiting join by at most one deadline per transition.
    #[test]
    fn a_straggler_delays_the_next_join_by_the_deadline_only() {
      let mut harness = Harness::new();
      harness.join(1);
      harness.join(2);
      harness.holds_ready.insert(device(2));
      harness.join(3);
      harness.join(4);
      assert!(harness.events_to(4).is_empty(), "4 waits");

      harness.advance(DEADLINE);
      assert!(
        harness
          .events_to(4)
          .iter()
          .any(|event| matches!(event, CallEvent::DaveWelcome { .. })),
        "4 is welcomed after one deadline"
      );
      harness.advance(DEADLINE);
      harness.assert_all_exchange_frames(&[1, 2, 3, 4]);
      assert_eq!(harness.coordinator.maybe_epoch(), Some(DaveEpoch(3)));
    }

    /// Joins and leaves while a transition waits are batched into the next one and converge.
    #[test]
    fn churn_during_a_transition_converges() {
      let mut harness = Harness::new();
      for id in 1..=3 {
        harness.join(id);
      }
      harness.holds_ready.insert(device(3));
      harness.join(4);
      harness.join(5);
      harness.leave(1);
      harness.join(6);
      harness.leave(5);
      assert_eq!(
        harness.coordinator.maybe_epoch(),
        Some(DaveEpoch(3)),
        "nothing new proposed yet"
      );

      harness.release_ready();

      harness.assert_all_exchange_frames(&[2, 3, 4, 6]);
      assert_eq!(
        harness.coordinator.maybe_epoch(),
        Some(DaveEpoch(4)),
        "one batched commit"
      );
      assert!(harness.coordinator.maybe_execution.is_none());
    }

    /// The straggler leaving ends the wait at once, and the change it caused follows.
    #[test]
    fn the_straggler_leaving_releases_the_waiting_join() {
      let mut harness = Harness::new();
      for id in 1..=3 {
        harness.join(id);
      }
      harness.holds_ready.insert(device(2));
      harness.join(4);
      harness.join(5);
      assert!(harness.events_to(5).is_empty());

      harness.leave(2);
      harness.holds_ready.clear();
      harness.held_ready.clear();
      harness.deliver_all();

      harness.assert_all_exchange_frames(&[1, 3, 4, 5]);
    }

    /// Transition ids count up to 65535 and continue at 1, never 0.
    #[test]
    fn transition_ids_wrap_to_one() {
      let mut harness = Harness::new();
      harness.join(1);
      harness.join(2);
      harness.coordinator.last_transition_id = u16::MAX;
      harness.join(3);
      assert_eq!(harness.first_execution_id().unwrap().0.get(), 1);
      harness.assert_all_exchange_frames(&[1, 2, 3]);
    }

    /// A member that leaves before it is ready doesn't hold the others up.
    #[test]
    fn a_member_leaving_unblocks_the_execution() {
      let mut harness = Harness::new();
      harness.join(1);
      harness.join(2);
      harness.holds_ready.insert(device(2));
      harness.join(3);
      assert_eq!(harness.executions_to(3), 0);

      harness.leave(2);
      harness.holds_ready.clear();
      harness.held_ready.clear();
      harness.deliver_all();

      assert!(harness.executions_to(3) >= 1);
      harness.assert_all_exchange_frames(&[1, 3]);
    }

    /// Clients from before transitions get no ids and switch at once; the coordinated ones still
    /// get theirs and don't wait for the old ones.
    #[test]
    fn clients_without_transitions_switch_at_once() {
      let mut harness = Harness::new();
      harness.join(1);
      harness.join_with_mode(2, DaveTransitionMode::AtOnce);
      harness.deliver_all();
      harness.join(3);

      let ids_to = |id: u64| {
        harness
          .delivered
          .iter()
          .filter_map(|outbound| match &outbound.event {
            CallEvent::DaveAnnounceCommit {
              maybe_transition_id, ..
            }
            | CallEvent::DaveWelcome {
              maybe_transition_id, ..
            } if outbound.recipient == device(id) => Some(*maybe_transition_id),
            _ => None,
          })
          .collect::<Vec<_>>()
      };
      assert!(ids_to(2).iter().all(Option::is_none));
      assert!(ids_to(3).iter().all(Option::is_some));
      assert_eq!(harness.executions_to(2), 0);
      assert_eq!(harness.executions_to(1), 1);
      harness.assert_all_exchange_frames(&[1, 2, 3]);
    }

    /// Only clients from before transitions: nothing changes for them.
    #[test]
    fn a_call_of_old_clients_has_no_transitions() {
      let mut harness = Harness::with_mode(DaveTransitionMode::AtOnce);
      for id in 1..=3 {
        harness.join(id);
      }
      assert!(
        harness
          .delivered
          .iter()
          .all(|outbound| !matches!(outbound.event, CallEvent::DaveExecuteTransition { .. }))
      );
      harness.assert_all_exchange_frames(&[1, 2, 3]);
    }

    #[test]
    fn ready_for_another_transition_is_ignored() {
      let mut harness = Harness::new();
      harness.join(1);
      harness.join(2);
      let stranger = DaveTransitionId(NonZeroU16::new(999).unwrap());
      assert!(
        harness
          .coordinator
          .on_transition_ready(device(1), stranger)
          .is_empty()
      );
    }
  }

  mod bindings {
    use super::*;

    #[test]
    fn bindings_are_relayed_with_proposals_and_welcomes() {
      let mut harness = Harness::new();
      harness.join(1);
      harness.join(2);
      harness.join_without_delivery(3);
      let proposals = harness
        .queue
        .iter()
        .find_map(|outbound| match &outbound.event {
          CallEvent::DaveProposals { key_bindings, .. } => Some(key_bindings.clone()),
          _ => None,
        })
        .unwrap();
      assert_eq!(proposals, vec![binding(3)]);
      // Deliver until device 3's welcome is queued.
      let mut welcome_bindings = None;
      while let Some(outbound) = harness.queue.pop_front() {
        if let CallEvent::DaveWelcome { key_bindings, .. } = &outbound.event
          && outbound.recipient == device(3)
        {
          welcome_bindings = Some(key_bindings.clone());
        }
        harness.queue.push_front(outbound);
        harness.deliver_one();
        if welcome_bindings.is_some() {
          break;
        }
      }
      let mut welcome_bindings = welcome_bindings.unwrap();
      welcome_bindings.sort_by_key(|binding| binding.device_id);
      assert_eq!(welcome_bindings, vec![binding(1), binding(2)]);
    }

    fn binding(id: u64) -> DaveKeyBinding {
      DaveKeyBinding {
        device_id: device(id),
        binding: Base64Bytes::new(vec![u8::try_from(id).unwrap()]),
      }
    }
  }

  mod rejection {
    use super::*;

    #[test]
    fn key_package_for_someone_else() {
      let mut harness = Harness::new();
      let mut client = harness.new_client(5);
      let key_package = client.create_key_package().unwrap();
      let result =
        harness
          .coordinator
          .on_key_package(device(6), key_package, None, DaveTransitionMode::Coordinated);
      assert!(matches!(
        result,
        Err(DaveCoordinatorError::ParticipantMismatch { .. })
      ));
    }

    #[test]
    fn duplicate_key_package() {
      let mut harness = Harness::new();
      harness.join(1);
      let key_package = harness
        .clients
        .get_mut(&device(1))
        .unwrap()
        .create_key_package()
        .unwrap();
      let result =
        harness
          .coordinator
          .on_key_package(device(1), key_package, None, DaveTransitionMode::Coordinated);
      assert!(matches!(result, Err(DaveCoordinatorError::AlreadyKnown(_))));
    }

    #[test]
    fn commit_from_wrong_epoch_or_stranger_is_ignored() {
      let mut harness = Harness::new();
      harness.join_without_delivery(1);
      harness.join_without_delivery(2);
      assert!(
        harness
          .coordinator
          .on_commit(device(2), DaveEpoch(0), vec![1], Some(vec![2]), Instant::now())
          .is_empty()
      );
      assert!(
        harness
          .coordinator
          .on_commit(device(1), DaveEpoch(5), vec![1], Some(vec![2]), Instant::now())
          .is_empty()
      );
      assert!(
        harness
          .coordinator
          .on_commit(device(1), DaveEpoch(0), vec![1], None, Instant::now())
          .is_empty()
      );
    }
  }

  /// Simulated clients connected to a coordinator through in-memory queues. Clients join with
  /// `mode` unless [`Self::join_with_mode`] says otherwise, and answer transitions like the engine.
  struct Harness {
    coordinator: DaveCoordinator,
    clients: BTreeMap<DeviceId, DaveParticipant>,
    queue: VecDeque<Outbound>,
    mode: DaveTransitionMode,
    /// Ready messages are held here instead of sent while set (a slow member).
    holds_ready: BTreeSet<DeviceId>,
    held_ready: Vec<(DeviceId, DaveTransitionId)>,
    /// Every event delivered, in order, by recipient.
    delivered: Vec<Outbound>,
    /// The simulated clock; moves only with [`Self::advance`].
    now: Instant,
  }

  impl Harness {
    fn new() -> Self {
      Self::with_mode(DaveTransitionMode::Coordinated)
    }

    fn with_mode(mode: DaveTransitionMode) -> Self {
      Self {
        coordinator: DaveCoordinator::new(call(), DEADLINE).unwrap(),
        clients: BTreeMap::new(),
        queue: VecDeque::new(),
        mode,
        holds_ready: BTreeSet::new(),
        held_ready: Vec::new(),
        delivered: Vec::new(),
        now: Instant::now(),
      }
    }

    /// Moves the clock on and runs the room timer once.
    fn advance(&mut self, by: Duration) {
      self.now += by;
      let outbound = self.coordinator.on_timer(self.now);
      self.queue.extend(outbound);
      self.deliver_all();
    }

    fn events_to(&self, id: u64) -> Vec<&CallEvent> {
      self
        .delivered
        .iter()
        .filter(|outbound| outbound.recipient == device(id))
        .map(|outbound| &outbound.event)
        .collect()
    }

    fn first_execution_id(&self) -> Option<DaveTransitionId> {
      self.delivered.iter().find_map(|outbound| match outbound.event {
        CallEvent::DaveExecuteTransition { transition_id } => Some(transition_id),
        _ => None,
      })
    }

    fn new_client(&self, id: u64) -> DaveParticipant {
      let mut client = DaveParticipant::new(DaveParticipantId(id), dave_call_id(call())).unwrap();
      client
        .set_external_sender(&self.coordinator.external_sender_package().unwrap())
        .unwrap();
      client
    }

    fn join(&mut self, id: u64) {
      self.join_without_delivery(id);
      self.deliver_all();
    }

    fn join_without_delivery(&mut self, id: u64) {
      self.join_with_mode(id, self.mode);
    }

    fn join_with_mode(&mut self, id: u64, mode: DaveTransitionMode) {
      let mut client = self.new_client(id);
      let key_package = client.create_key_package().unwrap();
      self.clients.insert(device(id), client);
      let outbound = self
        .coordinator
        .on_key_package(
          device(id),
          key_package,
          Some(vec![u8::try_from(id).unwrap()]),
          mode,
        )
        .unwrap();
      self.queue.extend(outbound);
    }

    /// Sends the ready messages held back so far.
    fn release_ready(&mut self) {
      self.holds_ready.clear();
      for (recipient, transition_id) in std::mem::take(&mut self.held_ready) {
        let outbound = self.coordinator.on_transition_ready(recipient, transition_id);
        self.queue.extend(outbound);
      }
      self.deliver_all();
    }

    fn executions_to(&self, id: u64) -> usize {
      self
        .delivered
        .iter()
        .filter(|outbound| {
          outbound.recipient == device(id)
            && matches!(outbound.event, CallEvent::DaveExecuteTransition { .. })
        })
        .count()
    }

    fn leave(&mut self, id: u64) {
      self.clients.remove(&device(id));
      let outbound = self.coordinator.on_leave(device(id));
      self.queue.extend(outbound);
      self.deliver_all();
    }

    /// Delivers queued messages the way the SFU would, feeding commits back into the coordinator.
    fn deliver_all(&mut self) {
      while !self.queue.is_empty() {
        self.deliver_one();
      }
    }

    /// Delivers the first queued message.
    fn deliver_one(&mut self) {
      if let Some(Outbound { recipient, event }) = self.queue.pop_front() {
        self.delivered.push(Outbound {
          recipient,
          event: event.clone(),
        });
        let Some(client) = self.clients.get_mut(&recipient) else {
          return;
        };
        let maybe_transition = match event {
          CallEvent::DaveProposals {
            epoch,
            proposals,
            expected_participants,
            ..
          } => {
            let expected: Vec<_> = expected_participants
              .iter()
              .map(|id| dave_participant_id(*id))
              .collect();
            if let Some(commit) = client.process_proposals(proposals.as_slice(), &expected).unwrap() {
              let outbound = self.coordinator.on_commit(
                recipient,
                DaveEpoch(epoch),
                commit.commit,
                commit.maybe_welcome,
                self.now,
              );
              self.queue.extend(outbound);
            }
            None
          },
          CallEvent::DaveAnnounceCommit {
            commit,
            maybe_transition_id,
            ..
          } => {
            assert!(client.process_commit(commit.as_slice()).unwrap());
            Some(maybe_transition_id)
          },
          CallEvent::DaveWelcome {
            welcome,
            maybe_transition_id,
            ..
          } => {
            let roster: Vec<_> = self.clients.keys().map(|id| dave_participant_id(*id)).collect();
            let client = self.clients.get_mut(&recipient).unwrap();
            client.process_welcome(welcome.as_slice(), &roster).unwrap();
            Some(maybe_transition_id)
          },
          CallEvent::DaveExecuteTransition { transition_id } => {
            assert!(
              client.execute_transition(transition_id).unwrap(),
              "executes a prepared transition"
            );
            None
          },
          other => panic!("unexpected event {other:?}"),
        };
        if let Some(maybe_transition_id) = maybe_transition {
          let client = self.clients.get_mut(&recipient).unwrap();
          client.prepare_transition(maybe_transition_id).unwrap();
          if let Some(transition_id) = maybe_transition_id {
            if self.holds_ready.contains(&recipient) {
              self.held_ready.push((recipient, transition_id));
            } else {
              let outbound = self.coordinator.on_transition_ready(recipient, transition_id);
              self.queue.extend(outbound);
            }
          }
        }
      }
    }

    fn assert_all_exchange_frames(&mut self, ids: &[u64]) {
      for &sender in ids {
        let encrypted = {
          let client = self.clients.get_mut(&device(sender)).unwrap();
          assert_eq!(
            client.participant_ids().len(),
            ids.len(),
            "group size seen by {sender}"
          );
          client
            .encrypt_frame(MediaKind::Audio, FrameCodec::Opus, FRAME)
            .unwrap()
            .into_owned()
        };
        for &receiver in ids.iter().filter(|&&id| id != sender) {
          let client = self.clients.get_mut(&device(receiver)).unwrap();
          let decrypted = client
            .decrypt_frame(DaveParticipantId(sender), MediaKind::Audio, &encrypted)
            .unwrap();
          assert_eq!(decrypted, FRAME, "{sender} -> {receiver}");
        }
      }
    }
  }

  fn call() -> CallId {
    "77".parse().unwrap()
  }

  fn device(raw: u64) -> DeviceId {
    crate::ids::device_id_from_dave(teamcraft_media::dave::DaveParticipantId(raw)).unwrap()
  }
}
