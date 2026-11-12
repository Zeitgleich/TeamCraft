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
//! Proposals that nobody commits in time (#1555: every committer silent, broken or refusing a
//! joiner) don't wedge the call: the group starts over. Members hear the external sender again,
//! drop their session and come back with fresh key packages; the joiners of the stalled proposals
//! keep theirs but get a strike, are proposed alone and only after everyone else, and are refused
//! after a second stall, and told so. A broken member is repaired by its fresh session, a joiner
//! its members won't accept is kept out, and a member that left is no longer in any group with a
//! current key. Strikes and refusals outlast reconnects for a while, so each joiner costs at most
//! two restarts per refusal window.
//!
//! A member whose connection drops stays in the group for [`DaveDeadlines::resume`]: back within it
//! with the group as it was ([`DaveCoordinator::on_resume`]), it carries on and nobody re-keys, so
//! a flapping link doesn't rotate everybody's media key every few seconds. Otherwise it leaves the
//! group then, as it does at once when it leaves the call (e2ee.md §8).
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
  SfuCallIncidents,
};
use thiserror::Error;
use tracing::{debug, info, warn};

use crate::ids::{dave_call_id, dave_participant_id, device_id_from_dave};

/// Stalls a joiner may be part of before it is refused (#1555).
const STRIKE_LIMIT: u8 = 2;
/// Most devices with strikes a call remembers; beyond it the one whose strikes expire first goes.
const MAX_SUSPECTS: usize = 256;

/// DAVE membership state of one call.
pub struct DaveCoordinator {
  call_id: CallId,
  deadlines: DaveDeadlines,
  external_sender: DaveExternalSender,
  /// [`DaveExternalSender::package`], sent again to members when the group starts over.
  external_sender_package: Vec<u8>,
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
  /// Joiners of stalled proposals (at most [`MAX_SUSPECTS`]), kept across their reconnects until
  /// the refusal window after their last stall passed; cleared once they are in.
  suspects: HashMap<DeviceId, Suspect>,
  /// Members told to start over whose new key package hasn't arrived; joiners with a strike wait
  /// for them, at most until `maybe_restart_deadline`.
  restarting: BTreeSet<DeviceId>,
  maybe_restart_deadline: Option<Instant>,
  /// Members whose connection dropped, with when they leave the group unless back. Their leaves,
  /// bindings and transition modes stay meanwhile; they commit nothing and hold up no execution.
  away: HashMap<DeviceId, Instant>,
  /// Restarts, strikes, refusals and late transitions since the room last took them, for the
  /// crash reports' backend (docs/crash-reports.md, "The SFU's incidents"). Counts only.
  incidents: SfuCallIncidents,
}

/// How long the coordinator waits for its members.
#[derive(Debug, Clone, Copy)]
pub struct DaveDeadlines {
  /// How long an announced transition waits for its members' ready before it is executed anyway.
  pub transition: Duration,
  /// How long proposals wait for a commit before the group starts over (#1555).
  pub commit: Duration,
  /// How long a joiner's strikes count after its last stall, and so how long a refused joiner
  /// stays refused, reconnects included.
  pub refusal: Duration,
  /// How long a member whose connection dropped keeps its leaf, so it can come back without a new
  /// epoch for everybody. Zero: it leaves the group at once, as before.
  pub resume: Duration,
}

/// A joiner whose proposals stalled.
#[derive(Debug, Clone, Copy)]
struct Suspect {
  strikes: u8,
  /// When the strikes are forgotten (and a refusal lifts).
  until: Instant,
}

impl Suspect {
  fn is_refused(self) -> bool {
    self.strikes >= STRIKE_LIMIT
  }
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
  /// When the group starts over if nobody committed.
  deadline: Instant,
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
  /// A coordinator that waits for its members at most as long as `deadlines` say.
  pub fn new(call_id: CallId, deadlines: DaveDeadlines) -> Result<Self, DaveCoordinatorError> {
    let external_sender = DaveExternalSender::new()?;
    let external_sender_package = external_sender.package()?;
    Ok(Self {
      call_id,
      deadlines,
      external_sender,
      external_sender_package,
      maybe_epoch: None,
      leaves: Vec::new(),
      waiting: Vec::new(),
      key_bindings: HashMap::new(),
      pending_removals: Vec::new(),
      maybe_transition: None,
      transition_modes: HashMap::new(),
      last_transition_id: 0,
      maybe_execution: None,
      suspects: HashMap::new(),
      restarting: BTreeSet::new(),
      maybe_restart_deadline: None,
      away: HashMap::new(),
      incidents: SfuCallIncidents::default(),
    })
  }

  /// The incidents since the last call, and none from now.
  pub fn take_incidents(&mut self) -> SfuCallIncidents {
    std::mem::take(&mut self.incidents)
  }

  /// The `ExternalSender` package every participant installs on join.
  pub fn external_sender_package(&self) -> Vec<u8> {
    self.external_sender_package.clone()
  }

  /// A participant's key package arrived; `transition_mode` is what its connection's `join` said.
  /// A refused device hears so again instead.
  pub fn on_key_package(
    &mut self,
    participant_id: DeviceId,
    key_package: Vec<u8>,
    maybe_key_binding: Option<Vec<u8>>,
    transition_mode: DaveTransitionMode,
    now: Instant,
  ) -> Result<Vec<Outbound>, DaveCoordinatorError> {
    let actual = device_id_from_dave(self.external_sender.key_package_participant(&key_package)?)?;
    if actual != participant_id {
      return Err(DaveCoordinatorError::ParticipantMismatch {
        expected: participant_id,
        actual,
      });
    }
    self.forget_expired_suspects(now);
    if let Some(suspect) = self
      .suspects
      .get(&participant_id)
      .filter(|suspect| suspect.is_refused())
    {
      debug!(?participant_id, "key package of a refused device");
      return Ok(vec![refusal(
        participant_id,
        suspect.until.saturating_duration_since(now),
      )]);
    }
    if self.is_known(participant_id) {
      return Err(DaveCoordinatorError::AlreadyKnown(participant_id));
    }
    if let Some(binding) = maybe_key_binding {
      self.key_bindings.insert(participant_id, binding);
    }
    self.transition_modes.insert(participant_id, transition_mode);
    self.waiting.push((participant_id, key_package));
    self.restarting.remove(&participant_id);
    Ok(self.start_transition(now))
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
    // The joiners are in and the committer works: whatever stalled before is behind them.
    self.suspects.remove(&participant_id);
    for (joiner, _) in &transition.joiners {
      self.suspects.remove(joiner);
    }
    let mut outbound = self.apply(transition, commit, maybe_welcome, now);
    outbound.extend(self.start_transition(now));
    outbound
  }

  /// A member installed the keys of `transition_id`. The last one to do so executes it.
  pub fn on_transition_ready(
    &mut self,
    participant_id: DeviceId,
    transition_id: DaveTransitionId,
    now: Instant,
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
    outbound.extend(self.start_transition(now));
    outbound
  }

  /// Periodic check (the room's timer): executes a transition whose deadline passed, logging how
  /// many members weren't ready; starts the group over when proposals found no committer in time
  /// (#1555); and starts the proposals that waited.
  pub fn on_timer(&mut self, now: Instant) -> Vec<Outbound> {
    self.forget_expired_suspects(now);
    let mut outbound = Vec::new();
    let gone: Vec<DeviceId> = self
      .away
      .iter()
      .filter(|(_, until)| now >= **until)
      .map(|(id, _)| *id)
      .collect();
    for participant_id in gone {
      outbound.extend(self.on_leave(participant_id, now));
    }
    if self
      .maybe_execution
      .as_ref()
      .is_some_and(|execution| now >= execution.deadline)
    {
      outbound.extend(self.execute_now());
    }
    if self
      .maybe_transition
      .as_ref()
      .is_some_and(|transition| now >= transition.deadline)
    {
      outbound.extend(self.restart(now));
    }
    if self
      .maybe_restart_deadline
      .is_some_and(|deadline| now >= deadline)
    {
      // Members that didn't come back in time (clients from before restarts can't) hold nobody up.
      self.restarting.clear();
      self.maybe_restart_deadline = None;
    }
    outbound.extend(self.start_transition(now));
    outbound
  }

  /// A member's connection dropped. Its leaf stays for [`DaveDeadlines::resume`]: back within it
  /// with the group as it was, it carries on ([`Self::on_resume`]); otherwise it leaves the group
  /// then. A device that isn't a member yet leaves at once.
  pub fn on_lost(&mut self, participant_id: DeviceId, now: Instant) -> Vec<Outbound> {
    if self.leaf_of(participant_id).is_none() || self.deadlines.resume.is_zero() {
      return self.on_leave(participant_id, now);
    }
    self.away.insert(participant_id, now + self.deadlines.resume);
    if let Some(transition) = &mut self.maybe_transition {
      transition.committers.remove(&participant_id);
    }
    let Some(execution) = &mut self.maybe_execution else {
      return Vec::new();
    };
    execution.awaiting.remove(&participant_id);
    self.execute_when_ready()
  }

  /// A device joined again; `maybe_epoch` is the epoch of the group it still holds. `true` when it
  /// carries on in that group: it was away, nothing changed meanwhile and no change is under way,
  /// so nobody re-keys. Otherwise it is out of the group from now on and joins with a new key
  /// package, as every device did before resumes.
  pub fn on_resume(
    &mut self,
    participant_id: DeviceId,
    maybe_epoch: Option<DaveEpoch>,
    now: Instant,
  ) -> (bool, Vec<Outbound>) {
    let was_away = self.away.remove(&participant_id).is_some();
    let is_unchanged = maybe_epoch.is_some()
      && maybe_epoch == self.maybe_epoch
      && self.maybe_transition.is_none()
      && self.maybe_execution.is_none()
      && self.leaf_of(participant_id).is_some();
    if was_away && is_unchanged {
      debug!(?participant_id, "a member came back to the group as it was");
      return (true, Vec::new());
    }
    (false, self.on_leave(participant_id, now))
  }

  /// A participant left the call (or the group, its connection having dropped). Its strikes stay: a
  /// reconnect doesn't buy a joiner nobody accepts two more restarts.
  pub fn on_leave(&mut self, participant_id: DeviceId, now: Instant) -> Vec<Outbound> {
    self.away.remove(&participant_id);
    self.waiting.retain(|(id, _)| *id != participant_id);
    self.key_bindings.remove(&participant_id);
    self.transition_modes.remove(&participant_id);
    self.restarting.remove(&participant_id);
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
    outbound.extend(self.start_transition(now));
    outbound
  }

  /// Current epoch of the group, if founded.
  pub fn maybe_epoch(&self) -> Option<DaveEpoch> {
    self.maybe_epoch
  }

  fn start_transition(&mut self, now: Instant) -> Vec<Outbound> {
    // One change at a time: proposals wait while a commit or an execution is outstanding.
    if self.maybe_transition.is_some() || self.maybe_execution.is_some() {
      return Vec::new();
    }
    match self.maybe_epoch {
      None => self.found_group(now),
      Some(epoch) => {
        let joiners = self.take_batch();
        if joiners.is_empty() && self.pending_removals.is_empty() {
          return Vec::new();
        }
        let removed = std::mem::take(&mut self.pending_removals);
        self.propose(epoch, joiners, removed, now)
      },
    }
  }

  /// The founder is the first waiting device without a strike (a joiner that stalled proposals
  /// never founds while another can); it adds the next batch.
  fn found_group(&mut self, now: Instant) -> Vec<Outbound> {
    if self.waiting.len() < 2 {
      return Vec::new();
    }
    let founder_index = self
      .waiting
      .iter()
      .position(|(id, _)| !self.suspects.contains_key(id))
      .unwrap_or(0);
    let founder = self.waiting.remove(founder_index);
    let joiners = self.take_batch();
    if joiners.is_empty() {
      self.waiting.insert(founder_index, founder);
      return Vec::new();
    }
    let (founder, _) = founder;
    self.leaves = vec![Some(founder)];
    self.maybe_epoch = Some(DaveEpoch(0));
    debug!(call_id = %self.call_id, ?founder, "founding DAVE group");
    self.propose(DaveEpoch(0), joiners, Vec::new(), now)
  }

  /// The key packages the next proposals add: every waiting device without a strike; when there is
  /// none, one with a strike, alone, so a stall is its own (#1555). Those wait while members that
  /// start over are still expected.
  fn take_batch(&mut self) -> Vec<(DeviceId, Vec<u8>)> {
    let known = &self.suspects;
    let (clean, mut suspects): (Vec<_>, Vec<_>) = std::mem::take(&mut self.waiting)
      .into_iter()
      .partition(|(id, _)| !known.contains_key(id));
    if !clean.is_empty() || suspects.is_empty() || !self.restarting.is_empty() {
      self.waiting = suspects;
      return clean;
    }
    let first = suspects.remove(0);
    self.waiting = suspects;
    vec![first]
  }

  fn propose(
    &mut self,
    epoch: DaveEpoch,
    joiners: Vec<(DeviceId, Vec<u8>)>,
    removed: Vec<DaveLeafIndex>,
    now: Instant,
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
    let committers: BTreeSet<DeviceId> = self
      .members()
      .filter(|member| !self.away.contains_key(member))
      .collect();
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
      deadline: now + self.deadlines.commit,
    });
    outbound
  }

  /// Nobody committed the proposals in time (#1555): every committer is silent, whether broken,
  /// hostile or refusing a joiner it can't verify. Their proposals can't be taken back, so the
  /// group starts over: members get the external sender again and answer with fresh sessions,
  /// which repairs a broken one; the joiners keep their key packages with a strike, and are
  /// refused (and told) at the second. A count only goes into the log.
  fn restart(&mut self, now: Instant) -> Vec<Outbound> {
    let Some(transition) = self.maybe_transition.take() else {
      return Vec::new();
    };
    let mut requeued = Vec::new();
    let mut outbound = Vec::new();
    let struck = transition.joiners.len();
    for (joiner, key_package) in transition.joiners {
      // A joiner that left (and maybe came back on a new connection) still stalled them.
      if self.strike(joiner, now).is_refused() {
        self.waiting.retain(|(id, _)| *id != joiner);
        self.key_bindings.remove(&joiner);
        outbound.push(refusal(joiner, self.deadlines.refusal));
      } else if !transition.departed_joiners.contains(&joiner) {
        requeued.push((joiner, key_package));
      }
    }
    let refused = outbound.len();
    self.incidents.dave_restarts = self.incidents.dave_restarts.saturating_add(1);
    self.incidents.dave_strikes = self
      .incidents
      .dave_strikes
      .saturating_add(u32::try_from(struck).unwrap_or(u32::MAX));
    self.incidents.dave_refusals = self
      .incidents
      .dave_refusals
      .saturating_add(u32::try_from(refused).unwrap_or(u32::MAX));
    let members: Vec<DeviceId> = self.members().collect();
    warn!(
      call_id = %self.call_id,
      epoch = transition.epoch.0,
      committers = transition.committers.len(),
      requeued = requeued.len(),
      refused,
      "no member committed the DAVE proposals in time; starting the group over"
    );
    self.maybe_epoch = None;
    self.leaves.clear();
    self.pending_removals.clear();
    self.maybe_execution = None;
    requeued.append(&mut self.waiting);
    self.waiting = requeued;
    self.restarting = members.iter().copied().collect();
    self.maybe_restart_deadline = Some(now + self.deadlines.commit);
    outbound.extend(members.into_iter().map(|recipient| Outbound {
      recipient,
      event: CallEvent::DaveExternalSender {
        package: Base64Bytes::new(self.external_sender_package.clone()),
      },
    }));
    outbound
  }

  /// Gives a joiner of stalled proposals a strike that counts for the refusal window. A full map
  /// makes room by forgetting the suspect whose strikes would expire first.
  fn strike(&mut self, joiner: DeviceId, now: Instant) -> Suspect {
    if !self.suspects.contains_key(&joiner) && self.suspects.len() >= MAX_SUSPECTS {
      let maybe_oldest = self
        .suspects
        .iter()
        .min_by_key(|(_, suspect)| suspect.until)
        .map(|(id, _)| *id);
      if let Some(oldest) = maybe_oldest {
        self.suspects.remove(&oldest);
      }
    }
    let until = now + self.deadlines.refusal;
    let suspect = self
      .suspects
      .entry(joiner)
      .or_insert(Suspect { strikes: 0, until });
    suspect.strikes = suspect.strikes.saturating_add(1);
    suspect.until = until;
    *suspect
  }

  fn forget_expired_suspects(&mut self, now: Instant) {
    self.suspects.retain(|_, suspect| now < suspect.until);
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
        .filter(|recipient| !self.away.contains_key(recipient))
        .collect();
      self.maybe_execution = Some(Execution {
        id,
        awaiting: recipients.clone(),
        recipients,
        deadline: now + self.deadlines.transition,
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
      self.incidents.dave_late_transitions = self.incidents.dave_late_transitions.saturating_add(1);
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

/// Tells a refused device so, and when a key package may be taken again.
fn refusal(recipient: DeviceId, retry_after: Duration) -> Outbound {
  // Rounded up: a retry at the given second must not be refused again.
  let secs = retry_after.as_secs() + u64::from(retry_after.subsec_nanos() > 0);
  Outbound {
    recipient,
    event: CallEvent::DaveRefused {
      retry_after_secs: u32::try_from(secs).unwrap_or(u32::MAX),
    },
  }
}

#[cfg(test)]
mod tests {
  use std::collections::{BTreeMap, BTreeSet, VecDeque};

  use teamcraft_media::dave::{DaveParticipant, FrameCodec, MediaKind};

  use super::*;

  const FRAME: &[u8] = &[0x78, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
  const DEADLINE: Duration = Duration::from_secs(2);
  const COMMIT_DEADLINE: Duration = Duration::from_secs(10);
  const REFUSAL_WINDOW: Duration = Duration::from_secs(300);

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
      let outbound = harness.coordinator.on_leave(device(3), harness.now);
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

  mod resumes {
    use super::*;

    const RESUME_GRACE: Duration = Duration::from_secs(10);

    #[test]
    fn a_member_back_within_the_grace_carries_on_and_nobody_re_keys() {
      let mut harness = Harness::with_resume(RESUME_GRACE);
      for id in 1..=3 {
        harness.join(id);
      }
      let epoch = harness.coordinator.maybe_epoch();
      let away = harness.lose(2);
      harness.advance(RESUME_GRACE / 2);
      assert!(harness.resume(2, away));
      assert_eq!(harness.coordinator.maybe_epoch(), epoch, "no new epoch");
      harness.assert_all_exchange_frames(&[1, 2, 3]);
    }

    #[test]
    fn a_member_not_back_in_time_leaves_the_group() {
      let mut harness = Harness::with_resume(RESUME_GRACE);
      for id in 1..=3 {
        harness.join(id);
      }
      let epoch = harness.coordinator.maybe_epoch().unwrap();
      let away = harness.lose(2);
      harness.advance(RESUME_GRACE + Duration::from_millis(1));
      assert!(
        harness.coordinator.maybe_epoch().unwrap() > epoch,
        "removed with a new epoch"
      );
      harness.assert_all_exchange_frames(&[1, 3]);
      assert!(!harness.resume(2, away));
      harness.join(2);
      harness.assert_all_exchange_frames(&[1, 2, 3]);
    }

    #[test]
    fn a_member_back_after_the_group_moved_joins_with_a_new_key_package() {
      let mut harness = Harness::with_resume(RESUME_GRACE);
      for id in 1..=3 {
        harness.join(id);
      }
      let away = harness.lose(2);
      harness.join(4);
      assert!(!harness.resume(2, away));
      harness.join(2);
      harness.assert_all_exchange_frames(&[1, 2, 3, 4]);
    }

    #[test]
    fn an_away_member_holds_up_no_commit() {
      let mut harness = Harness::with_resume(RESUME_GRACE);
      harness.join(1);
      harness.join(2);
      let epoch = harness.coordinator.maybe_epoch().unwrap();
      let away = harness.lose(2);
      // Only 1 can commit 3's addition; it doesn't wait for 2, whose leaf stays meanwhile.
      harness.join(3);
      assert!(
        harness.coordinator.maybe_epoch().unwrap() > epoch,
        "committed without the away member"
      );
      // The group moved on: 2 comes back with a new key package.
      assert!(!harness.resume(2, away));
      harness.join(2);
      harness.assert_all_exchange_frames(&[1, 2, 3]);
    }

    #[test]
    fn without_a_grace_a_lost_member_leaves_at_once() {
      let mut harness = Harness::new();
      for id in 1..=3 {
        harness.join(id);
      }
      let epoch = harness.coordinator.maybe_epoch().unwrap();
      let away = harness.lose(2);
      assert!(harness.coordinator.maybe_epoch().unwrap() > epoch);
      assert!(!harness.resume(2, away));
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
          .on_transition_ready(device(1), stranger, harness.now)
          .is_empty()
      );
    }
  }

  /// Proposals nobody commits (#1555).
  mod stalls {
    use super::*;

    /// Every committer's session is broken: before the deadline nothing moves, after it the group
    /// starts over with fresh sessions, the joiner comes in alone after the members, and the next
    /// join goes through.
    #[test]
    fn broken_committers_are_repaired_by_a_restart() {
      let mut harness = Harness::new();
      for id in 1..=3 {
        harness.join(id);
      }
      harness.broken.extend([device(1), device(2), device(3)]);
      harness.join(4);
      harness.advance(COMMIT_DEADLINE - Duration::from_millis(1));
      assert_eq!(harness.coordinator.maybe_epoch(), Some(DaveEpoch(2)), "stalled");
      assert_eq!(harness.restarts_to(1), 0);

      harness.advance(Duration::from_millis(1));

      for id in 1..=3 {
        assert_eq!(harness.restarts_to(id), 1, "member {id}");
      }
      assert_eq!(harness.restarts_to(4), 0, "the joiner keeps its key package");
      harness.assert_all_exchange_frames(&[1, 2, 3, 4]);
      harness.join(5);
      harness.assert_all_exchange_frames(&[1, 2, 3, 4, 5]);
    }

    /// A member that left is removed even when nobody commits its removal: the new group is the
    /// one of those still here.
    #[test]
    fn a_departed_member_is_dropped_when_its_removal_stalls() {
      let mut harness = Harness::new();
      for id in 1..=3 {
        harness.join(id);
      }
      harness.broken.extend([device(2), device(3)]);
      harness.leave(1);
      assert_eq!(harness.coordinator.maybe_epoch(), Some(DaveEpoch(2)), "stalled");

      harness.advance(COMMIT_DEADLINE);

      harness.assert_all_exchange_frames(&[2, 3]);
      assert!(!harness.coordinator.leaves.contains(&Some(device(1))));
    }

    /// A joiner its members can't verify stalls its proposals twice: the members start over twice,
    /// then it is refused, the members keep talking and later joins go through.
    #[test]
    fn a_joiner_nobody_accepts_is_refused_after_two_stalls() {
      let mut harness = Harness::new();
      for id in 1..=3 {
        harness.join(id);
      }
      harness.unverifiable.insert(device(4));
      harness.join(4);
      harness.advance(COMMIT_DEADLINE);
      harness.assert_all_exchange_frames(&[1, 2, 3]);
      harness.advance(COMMIT_DEADLINE);

      for id in 1..=3 {
        assert_eq!(harness.restarts_to(id), 2, "member {id}");
      }
      harness.assert_all_exchange_frames(&[1, 2, 3]);
      assert_eq!(
        harness.refusals_to(4),
        vec![300],
        "the refused joiner is told, with the window"
      );

      harness.advance(COMMIT_DEADLINE);
      assert_eq!(harness.restarts_to(1), 2, "no further restarts");
      harness.join(5);
      harness.assert_all_exchange_frames(&[1, 2, 3, 5]);
    }

    /// The case of the issue: the only committer never commits. The joiner is refused after two
    /// restarts, and the call stays quiet afterwards instead of restarting for good.
    #[test]
    fn a_hostile_only_committer_costs_a_joiner_two_restarts() {
      let mut harness = Harness::new();
      harness.join(1);
      harness.join(2);
      harness.leave(2);
      harness.hostile.insert(device(1));
      harness.join(3);

      harness.advance(COMMIT_DEADLINE);
      harness.advance(COMMIT_DEADLINE);
      harness.advance(COMMIT_DEADLINE);
      harness.advance(COMMIT_DEADLINE);

      assert_eq!(harness.restarts_to(1), 2);
      assert_eq!(harness.refusals_to(3).len(), 1);
      assert!(harness.coordinator.maybe_transition.is_none());
      // The crash reports' backend hears of it as counts.
      let incidents = harness.coordinator.take_incidents();
      assert_eq!(incidents.dave_restarts, 2);
      assert_eq!(incidents.dave_strikes, 2);
      assert_eq!(incidents.dave_refusals, 1);
      assert!(harness.coordinator.take_incidents().is_empty());
    }

    /// A refused device that reconnects is refused again at once, with the time left, and costs
    /// nobody a restart; once the window passed it is proposed again (and refused after two more
    /// stalls).
    #[test]
    fn a_refused_device_that_reconnects_restarts_nothing_within_the_window() {
      let mut harness = Harness::new();
      for id in 1..=3 {
        harness.join(id);
      }
      harness.unverifiable.insert(device(4));
      harness.join(4);
      harness.advance(COMMIT_DEADLINE);
      harness.advance(COMMIT_DEADLINE);
      assert_eq!(harness.restarts_to(1), 2);

      harness.advance(Duration::from_secs(60));
      harness.leave(4);
      harness.join(4);
      harness.advance(COMMIT_DEADLINE);
      harness.advance(COMMIT_DEADLINE);

      assert_eq!(harness.restarts_to(1), 2, "no restart within the window");
      // Refused at 20 s, back at 80 s.
      assert_eq!(*harness.refusals_to(4).last().unwrap(), 300 - 60);
      harness.assert_all_exchange_frames(&[1, 2, 3]);

      harness.advance(REFUSAL_WINDOW);
      harness.leave(4);
      harness.join(4);
      harness.advance(COMMIT_DEADLINE);
      assert_eq!(harness.restarts_to(1), 3, "proposed again after the window");
    }

    /// A strike survives a reconnect: the joiner's next stall refuses it.
    #[test]
    fn a_strike_outlasts_a_reconnect() {
      let mut harness = Harness::new();
      for id in 1..=3 {
        harness.join(id);
      }
      harness.unverifiable.insert(device(4));
      harness.join(4);
      harness.advance(COMMIT_DEADLINE);
      harness.leave(4);
      harness.join(4);
      harness.advance(COMMIT_DEADLINE);

      assert_eq!(harness.restarts_to(1), 2);
      assert_eq!(harness.refusals_to(4), vec![300]);
    }

    /// The suspects a call remembers are bounded; the one whose strikes expire first goes.
    #[test]
    fn suspects_are_bounded() {
      let mut harness = Harness::new();
      let now = harness.now;
      for raw in 0..MAX_SUSPECTS + 10 {
        let at = now + Duration::from_millis(u64::try_from(raw).unwrap());
        harness
          .coordinator
          .strike(device(1000 + u64::try_from(raw).unwrap()), at);
      }
      assert_eq!(harness.coordinator.suspects.len(), MAX_SUSPECTS);
      assert!(!harness.coordinator.suspects.contains_key(&device(1000)));
      assert!(harness.coordinator.suspects.contains_key(&device(1000 + 265)));
    }

    /// A commit that was on its way when the group started over arrives before the member's new
    /// key package (one ordered stream) and is ignored.
    #[test]
    fn a_commit_from_before_the_restart_is_ignored() {
      let mut harness = Harness::new();
      harness.join(1);
      harness.join(2);
      harness.holds_commits.extend([device(1), device(2)]);
      harness.join(3);
      assert_eq!(harness.held_commits.len(), 2);
      harness.holds_commits.clear();

      harness.now += COMMIT_DEADLINE;
      let restart = harness.coordinator.on_timer(harness.now);
      for HeldCommit {
        member,
        epoch,
        commit,
        maybe_welcome,
      } in std::mem::take(&mut harness.held_commits)
      {
        assert!(
          harness
            .coordinator
            .on_commit(member, epoch, commit, maybe_welcome, harness.now)
            .is_empty()
        );
      }
      harness.queue.extend(restart);
      harness.deliver_all();

      harness.assert_all_exchange_frames(&[1, 2, 3]);
    }

    /// A member that drops while the group starts over doesn't hold it up.
    #[test]
    fn a_member_leaving_during_the_restart() {
      let mut harness = Harness::new();
      for id in 1..=3 {
        harness.join(id);
      }
      harness.broken.extend([device(1), device(2), device(3)]);
      harness.join(4);
      harness.now += COMMIT_DEADLINE;
      let restart = harness.coordinator.on_timer(harness.now);
      harness.queue.extend(restart);
      harness.deliver_one();
      harness.leave(2);

      harness.assert_all_exchange_frames(&[1, 3, 4]);
    }

    /// A member that never answers the restart (a client from before) holds the joiner with a
    /// strike up for one deadline only.
    #[test]
    fn a_member_that_never_comes_back_holds_nobody_up() {
      let mut harness = Harness::new();
      for id in 1..=3 {
        harness.join(id);
      }
      harness.broken.extend([device(1), device(2), device(3)]);
      harness.ignores_restart.insert(device(3));
      harness.join(4);
      harness.advance(COMMIT_DEADLINE);
      assert!(
        !harness
          .events_to(4)
          .iter()
          .any(|event| matches!(event, CallEvent::DaveWelcome { .. })),
        "4 waits for 3"
      );

      harness.advance(COMMIT_DEADLINE);

      harness.assert_all_exchange_frames(&[1, 2, 4]);
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
      let result = harness.coordinator.on_key_package(
        device(6),
        key_package,
        None,
        DaveTransitionMode::Coordinated,
        harness.now,
      );
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
      let result = harness.coordinator.on_key_package(
        device(1),
        key_package,
        None,
        DaveTransitionMode::Coordinated,
        harness.now,
      );
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
    /// Members whose session is broken: they commit nothing until a restart gives them a new one.
    broken: BTreeSet<DeviceId>,
    /// Members that never commit, whatever their session.
    hostile: BTreeSet<DeviceId>,
    /// Members whose commits are held back here (slow signaling).
    holds_commits: BTreeSet<DeviceId>,
    held_commits: Vec<HeldCommit>,
    /// Clients from before restarts: they don't answer the external sender a second time.
    ignores_restart: BTreeSet<DeviceId>,
    /// Devices whose key binding no member can verify: proposals adding one aren't committed.
    unverifiable: BTreeSet<DeviceId>,
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
      Self::with(mode, Duration::ZERO)
    }

    fn with_resume(grace: Duration) -> Self {
      Self::with(DaveTransitionMode::Coordinated, grace)
    }

    fn with(mode: DaveTransitionMode, resume: Duration) -> Self {
      Self {
        coordinator: DaveCoordinator::new(
          call(),
          DaveDeadlines {
            transition: DEADLINE,
            commit: COMMIT_DEADLINE,
            refusal: REFUSAL_WINDOW,
            resume,
          },
        )
        .unwrap(),
        clients: BTreeMap::new(),
        queue: VecDeque::new(),
        mode,
        holds_ready: BTreeSet::new(),
        broken: BTreeSet::new(),
        hostile: BTreeSet::new(),
        holds_commits: BTreeSet::new(),
        held_commits: Vec::new(),
        ignores_restart: BTreeSet::new(),
        unverifiable: BTreeSet::new(),
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
        .set_external_sender(&self.coordinator.external_sender_package())
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
          self.now,
        )
        .unwrap();
      self.queue.extend(outbound);
    }

    /// Sends the ready messages held back so far.
    fn release_ready(&mut self) {
      self.holds_ready.clear();
      for (recipient, transition_id) in std::mem::take(&mut self.held_ready) {
        let outbound = self
          .coordinator
          .on_transition_ready(recipient, transition_id, self.now);
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

    /// `id`'s connection drops: it hears nothing until it is back; its session is returned.
    fn lose(&mut self, id: u64) -> DaveParticipant {
      let client = self.clients.remove(&device(id)).unwrap();
      let outbound = self.coordinator.on_lost(device(id), self.now);
      self.queue.extend(outbound);
      self.deliver_all();
      client
    }

    /// `id` joins again holding `client`'s group; whether it carries on in it.
    fn resume(&mut self, id: u64, client: DaveParticipant) -> bool {
      let (is_resumed, outbound) = self
        .coordinator
        .on_resume(device(id), client.maybe_epoch(), self.now);
      if is_resumed {
        self.clients.insert(device(id), client);
      }
      self.queue.extend(outbound);
      self.deliver_all();
      is_resumed
    }

    fn leave(&mut self, id: u64) {
      self.clients.remove(&device(id));
      let outbound = self.coordinator.on_leave(device(id), self.now);
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
            let adds_unverifiable = expected_participants
              .iter()
              .any(|id| *id != recipient && self.unverifiable.contains(id));
            if self.broken.contains(&recipient) || self.hostile.contains(&recipient) || adds_unverifiable {
              return;
            }
            let expected: Vec<_> = expected_participants
              .iter()
              .map(|id| dave_participant_id(*id))
              .collect();
            if let Some(commit) = client.process_proposals(proposals.as_slice(), &expected).unwrap() {
              if self.holds_commits.contains(&recipient) {
                self.held_commits.push(HeldCommit {
                  member: recipient,
                  epoch: DaveEpoch(epoch),
                  commit: commit.commit,
                  maybe_welcome: commit.maybe_welcome,
                });
                return;
              }
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
            // The trusted roster comes from the conversation: members whose connection dropped
            // are on it too.
            let roster: Vec<_> = self
              .clients
              .keys()
              .chain(self.coordinator.away.keys())
              .map(|id| dave_participant_id(*id))
              .collect();
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
          CallEvent::DaveRefused { .. } => None,
          CallEvent::DaveExternalSender { .. } => {
            // The group starts over: a fresh session answers, like the engine.
            if !self.ignores_restart.contains(&recipient) {
              self.rejoin(recipient);
            }
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
              let outbound = self
                .coordinator
                .on_transition_ready(recipient, transition_id, self.now);
              self.queue.extend(outbound);
            }
          }
        }
      }
    }

    /// A member answering a restart: a fresh session and key package on the same connection.
    fn rejoin(&mut self, recipient: DeviceId) {
      let raw = dave_participant_id(recipient).0;
      let mut client = self.new_client(raw);
      let key_package = client.create_key_package().unwrap();
      self.clients.insert(recipient, client);
      self.broken.remove(&recipient);
      let outbound = self
        .coordinator
        .on_key_package(
          recipient,
          key_package,
          Some(vec![u8::try_from(raw).unwrap()]),
          self.mode,
          self.now,
        )
        .unwrap();
      self.queue.extend(outbound);
    }

    fn refusals_to(&self, id: u64) -> Vec<u32> {
      self
        .delivered
        .iter()
        .filter_map(|outbound| match outbound.event {
          CallEvent::DaveRefused { retry_after_secs } if outbound.recipient == device(id) => {
            Some(retry_after_secs)
          },
          _ => None,
        })
        .collect()
    }

    fn restarts_to(&self, id: u64) -> usize {
      self
        .delivered
        .iter()
        .filter(|outbound| {
          outbound.recipient == device(id) && matches!(outbound.event, CallEvent::DaveExternalSender { .. })
        })
        .count()
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

  /// A commit the harness holds back instead of sending.
  struct HeldCommit {
    member: DeviceId,
    epoch: DaveEpoch,
    commit: Vec<u8>,
    maybe_welcome: Option<Vec<u8>>,
  }

  fn call() -> CallId {
    "77".parse().unwrap()
  }

  fn device(raw: u64) -> DeviceId {
    crate::ids::device_id_from_dave(teamcraft_media::dave::DaveParticipantId(raw)).unwrap()
  }
}
