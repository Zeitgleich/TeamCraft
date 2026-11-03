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
//! Pure state machine: every input returns the messages to send, so it is tested without network.

use std::collections::{BTreeSet, HashMap};

use teamcraft_media::dave::{DaveEpoch, DaveError, DaveExternalSender, DaveLeafIndex, DaveParticipantId};
use teamcraft_proto::{Base64Bytes, CallEvent, CallId, DaveKeyBinding, DeviceId, IdError};
use thiserror::Error;
use tracing::{debug, warn};

use crate::ids::{dave_call_id, dave_participant_id, device_id_from_dave};

/// DAVE membership state of one call.
pub struct DaveCoordinator {
  call_id: CallId,
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
}

/// One message to deliver over a participant's signaling stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outbound {
  pub recipient: DeviceId,
  pub event: CallEvent,
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
  pub fn new(call_id: CallId) -> Result<Self, DaveCoordinatorError> {
    Ok(Self {
      call_id,
      external_sender: DaveExternalSender::new()?,
      maybe_epoch: None,
      leaves: Vec::new(),
      waiting: Vec::new(),
      key_bindings: HashMap::new(),
      pending_removals: Vec::new(),
      maybe_transition: None,
    })
  }

  /// The `ExternalSender` package every participant installs on join.
  pub fn external_sender_package(&self) -> Result<Vec<u8>, DaveCoordinatorError> {
    Ok(self.external_sender.package()?)
  }

  /// A participant's key package arrived.
  pub fn on_key_package(
    &mut self,
    participant_id: DeviceId,
    key_package: Vec<u8>,
    maybe_key_binding: Option<Vec<u8>>,
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
    let mut outbound = self.apply(transition, commit, maybe_welcome);
    outbound.extend(self.start_transition());
    outbound
  }

  /// A participant left the call (or its connection dropped).
  pub fn on_leave(&mut self, participant_id: DeviceId) -> Vec<Outbound> {
    self.waiting.retain(|(id, _)| *id != participant_id);
    self.key_bindings.remove(&participant_id);
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
    self.start_transition()
  }

  /// Current epoch of the group, if founded.
  pub fn maybe_epoch(&self) -> Option<DaveEpoch> {
    self.maybe_epoch
  }

  fn start_transition(&mut self) -> Vec<Outbound> {
    if self.maybe_transition.is_some() {
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
  ) -> Vec<Outbound> {
    // Members before the joiners are placed get the commit; joiners get the welcome instead.
    let mut outbound: Vec<Outbound> = self
      .members()
      .map(|recipient| Outbound {
        recipient,
        event: CallEvent::DaveAnnounceCommit {
          epoch: transition.epoch.0,
          commit: Base64Bytes::new(commit.clone()),
        },
      })
      .collect();
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
          },
        });
      }
    }
    while self.leaves.last().is_some_and(Option::is_none) {
      self.leaves.pop();
    }
    self.maybe_epoch = Some(DaveEpoch(transition.epoch.0 + 1));
    outbound
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
  use std::collections::{BTreeMap, VecDeque};

  use teamcraft_media::dave::{DaveParticipant, FrameCodec, MediaKind};

  use super::*;

  const FRAME: &[u8] = &[0x78, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];

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
      let result = harness.coordinator.on_key_package(device(6), key_package, None);
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
      let result = harness.coordinator.on_key_package(device(1), key_package, None);
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
          .on_commit(device(2), DaveEpoch(0), vec![1], Some(vec![2]))
          .is_empty()
      );
      assert!(
        harness
          .coordinator
          .on_commit(device(1), DaveEpoch(5), vec![1], Some(vec![2]))
          .is_empty()
      );
      assert!(
        harness
          .coordinator
          .on_commit(device(1), DaveEpoch(0), vec![1], None)
          .is_empty()
      );
    }
  }

  /// Simulated clients connected to a coordinator through in-memory queues.
  struct Harness {
    coordinator: DaveCoordinator,
    clients: BTreeMap<DeviceId, DaveParticipant>,
    queue: VecDeque<Outbound>,
  }

  impl Harness {
    fn new() -> Self {
      Self {
        coordinator: DaveCoordinator::new(call()).unwrap(),
        clients: BTreeMap::new(),
        queue: VecDeque::new(),
      }
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
      let mut client = self.new_client(id);
      let key_package = client.create_key_package().unwrap();
      self.clients.insert(device(id), client);
      let outbound = self
        .coordinator
        .on_key_package(device(id), key_package, Some(vec![u8::try_from(id).unwrap()]))
        .unwrap();
      self.queue.extend(outbound);
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
        let Some(client) = self.clients.get_mut(&recipient) else {
          return;
        };
        match event {
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
              let outbound =
                self
                  .coordinator
                  .on_commit(recipient, DaveEpoch(epoch), commit.commit, commit.maybe_welcome);
              self.queue.extend(outbound);
            }
          },
          CallEvent::DaveAnnounceCommit { commit, .. } => client.process_commit(commit.as_slice()).unwrap(),
          CallEvent::DaveWelcome { welcome, .. } => client.process_welcome(welcome.as_slice()).unwrap(),
          other => panic!("unexpected event {other:?}"),
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
