//! Microphones and speakers on Linux from PipeWire's registry (#1072).
//!
//! The ALSA view of a PipeWire system lists every PCM of every card, names them after the card
//! ("HD-Audio Generic, ALC1220 Alt Analog"), and a PCM that PipeWire holds open can't be probed,
//! so it vanished from one listing and came back in the next. PipeWire itself knows the devices
//! the user sees in the desktop's sound settings: its `Audio/Source` and `Audio/Sink` nodes, named
//! by their description, identified by their node name (stable across restarts and replugging),
//! and the session manager's default for each direction.
//!
//! One thread watches the registry for the life of the process; a device plugged in or out, or a
//! new default, reaches [`PipeWireDevices::subscribe`] at once instead of on the next poll. When
//! the daemon restarts, the thread reconnects with backoff and keeps the last lists until then.
//! Streams are opened through cpal's PipeWire host by the same node name.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use pipewire as pw;
use pw::registry::GlobalObject;
use pw::spa::utils::dict::DictRef;
use pw::types::ObjectType;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use crate::audio_io::{AudioDeviceId, AudioDeviceInfo};

const SOURCE_CLASS: &str = "Audio/Source";
const SINK_CLASS: &str = "Audio/Sink";
const DUPLEX_CLASS: &str = "Audio/Duplex";
/// The session manager's metadata object with the default devices.
const DEFAULT_METADATA: &str = "default";
const DEFAULT_SOURCE_KEY: &str = "default.audio.source";
const DEFAULT_SINK_KEY: &str = "default.audio.sink";
/// How long the first listing waits for PipeWire's answer before callers get what is known.
const FIRST_LISTING_TIMEOUT: Duration = Duration::from_secs(2);
/// Reconnecting after the daemon went away: the first wait, doubled up to the cap.
const RECONNECT_FIRST: Duration = Duration::from_millis(500);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

/// The audio devices PipeWire offers, kept current by a registry watcher.
pub struct PipeWireDevices {
  shared: Arc<Shared>,
  changes: watch::Sender<u64>,
}

/// The audio nodes and defaults of one registry, as far as they matter for a device picker.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DeviceGraph {
  nodes: BTreeMap<u32, AudioNode>,
  maybe_default_source: Option<String>,
  maybe_default_sink: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AudioNode {
  name: String,
  description: String,
  is_source: bool,
  is_sink: bool,
}

/// The published graph and whether the first listing arrived.
#[derive(Default)]
struct Shared {
  state: Mutex<Published>,
  first_listing: Condvar,
}

#[derive(Default)]
struct Published {
  graph: DeviceGraph,
  is_listed: bool,
}

/// How a watch session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionEnd {
  /// No connection at all (no daemon, or a sandbox without the socket).
  Unreachable,
  /// Connected, then the daemon went away.
  Lost,
}

impl PipeWireDevices {
  /// The process's watcher, started on first use; `None` without a reachable PipeWire daemon
  /// (the caller falls back to PulseAudio or ALSA through cpal).
  pub fn shared() -> Option<&'static Self> {
    static DEVICES: OnceLock<Option<PipeWireDevices>> = OnceLock::new();
    DEVICES.get_or_init(Self::start).as_ref()
  }

  /// Microphones: the default first, then by name.
  pub fn inputs(&self) -> Vec<AudioDeviceInfo> {
    self.wait_listed().inputs()
  }

  /// Speakers and headsets: the default first, then by name.
  pub fn outputs(&self) -> Vec<AudioDeviceInfo> {
    self.wait_listed().outputs()
  }

  /// Ticks whenever a device or a default changes.
  pub fn subscribe(&self) -> watch::Receiver<u64> {
    self.changes.subscribe()
  }

  fn start() -> Option<Self> {
    let shared = Arc::new(Shared::default());
    let (changes, _) = watch::channel(0);
    let (reachable_tx, reachable_rx) = std::sync::mpsc::channel::<bool>();
    let thread_shared = Arc::clone(&shared);
    let thread_changes = changes.clone();
    let spawned = std::thread::Builder::new()
      .name("pipewire-audio-devices".into())
      .spawn(move || watch_forever(&thread_shared, &thread_changes, &reachable_tx));
    if let Err(error) = spawned {
      warn!(%error, "could not start the PipeWire device watcher");
      return None;
    }
    // The thread answers once it connected or failed to.
    if !reachable_rx.recv().unwrap_or(false) {
      return None;
    }
    info!("audio devices from PipeWire");
    Some(Self { shared, changes })
  }

  /// The graph once the first listing arrived (or the wait for it gave up).
  fn wait_listed(&self) -> DeviceGraph {
    let state = self.shared.lock();
    let (state, timeout) = self
      .shared
      .first_listing
      .wait_timeout_while(state, FIRST_LISTING_TIMEOUT, |state| !state.is_listed)
      .unwrap_or_else(PoisonError::into_inner);
    if timeout.timed_out() {
      warn!("PipeWire didn't list its devices in time");
    }
    state.graph.clone()
  }
}

impl DeviceGraph {
  fn inputs(&self) -> Vec<AudioDeviceInfo> {
    self.devices(|node| node.is_source, self.maybe_default_source.as_deref())
  }

  fn outputs(&self) -> Vec<AudioDeviceInfo> {
    self.devices(|node| node.is_sink, self.maybe_default_sink.as_deref())
  }

  /// Records an audio device node; other objects are ignored. Whether the graph changed.
  fn on_node(&mut self, id: u32, props: &DictRef) -> bool {
    let Some(node) = AudioNode::from_props(props) else {
      return false;
    };
    self.nodes.insert(id, node.clone()) != Some(node)
  }

  fn on_remove(&mut self, id: u32) -> bool {
    self.nodes.remove(&id).is_some()
  }

  /// A property of the `default` metadata; `None` clears it.
  fn on_default(&mut self, key: &str, maybe_value: Option<&str>) -> bool {
    let slot = match key {
      DEFAULT_SOURCE_KEY => &mut self.maybe_default_source,
      DEFAULT_SINK_KEY => &mut self.maybe_default_sink,
      _ => return false,
    };
    let maybe_name = maybe_value.and_then(default_node_name);
    if *slot == maybe_name {
      return false;
    }
    *slot = maybe_name;
    true
  }

  fn devices(
    &self,
    is_kind: impl Fn(&AudioNode) -> bool,
    maybe_default: Option<&str>,
  ) -> Vec<AudioDeviceInfo> {
    let mut infos: Vec<AudioDeviceInfo> = self
      .nodes
      .values()
      .filter(|node| is_kind(node))
      .map(|node| AudioDeviceInfo {
        id: device_id(&node.name),
        name: node.description.clone(),
        is_default: maybe_default == Some(node.name.as_str()),
      })
      .collect();
    infos.sort_by(|a, b| (!a.is_default, &a.name).cmp(&(!b.is_default, &b.name)));
    infos
  }
}

impl AudioNode {
  /// An audio device from a node's registry properties; `None` for streams, video, MIDI and the
  /// internal nodes of filters.
  fn from_props(props: &DictRef) -> Option<Self> {
    let class = props.get(*pw::keys::MEDIA_CLASS)?;
    let (is_source, is_sink) = match class {
      SOURCE_CLASS => (true, false),
      SINK_CLASS => (false, true),
      DUPLEX_CLASS => (true, true),
      _ => return None,
    };
    let name = props.get(*pw::keys::NODE_NAME)?.to_owned();
    let description = props
      .get(*pw::keys::NODE_DESCRIPTION)
      .or_else(|| props.get(*pw::keys::NODE_NICK))
      .unwrap_or(&name)
      .to_owned();
    Some(Self {
      name,
      description,
      is_source,
      is_sink,
    })
  }
}

impl Shared {
  /// Replaces the graph; whether that changed anything callers see.
  fn publish(&self, graph: &DeviceGraph) -> bool {
    let mut state = self.lock();
    let is_changed = !state.is_listed || state.graph != *graph;
    state.graph = graph.clone();
    state.is_listed = true;
    self.first_listing.notify_all();
    is_changed
  }

  fn lock(&self) -> std::sync::MutexGuard<'_, Published> {
    // Poisoning only means a panic mid-update of plain device data; keep going with it.
    self.state.lock().unwrap_or_else(PoisonError::into_inner)
  }
}

/// Runs watch sessions until the process ends; tells `reachable` whether the first one connected.
fn watch_forever(
  shared: &Arc<Shared>,
  changes: &watch::Sender<u64>,
  reachable: &std::sync::mpsc::Sender<bool>,
) {
  let mut maybe_reachable = Some(reachable);
  let mut wait = RECONNECT_FIRST;
  loop {
    let end = run_session(shared, changes, &mut maybe_reachable);
    if let Some(reachable) = maybe_reachable.take() {
      // Unreachable from the start: the caller uses another backend; nobody needs this thread.
      let _ = reachable.send(false);
      return;
    }
    match end {
      SessionEnd::Lost => {
        warn!("lost the connection to PipeWire; reconnecting");
        wait = RECONNECT_FIRST;
      },
      SessionEnd::Unreachable => debug!(wait_ms = wait.as_millis(), "PipeWire still unreachable"),
    }
    std::thread::sleep(wait);
    wait = (wait * 2).min(RECONNECT_MAX);
  }
}

/// Connects, publishes the registry's audio devices as they change, and returns when the
/// connection ends.
fn run_session(
  shared: &Arc<Shared>,
  changes: &watch::Sender<u64>,
  maybe_reachable: &mut Option<&std::sync::mpsc::Sender<bool>>,
) -> SessionEnd {
  pw::init();
  let Ok(main_loop) = pw::main_loop::MainLoopRc::new(None) else {
    return SessionEnd::Unreachable;
  };
  let Ok(context) = pw::context::ContextRc::new(&main_loop, None) else {
    return SessionEnd::Unreachable;
  };
  // No socket: no daemon, or Flatpak without `--filesystem=xdg-run/pipewire-0`.
  let Ok(core) = context.connect_rc(None) else {
    return SessionEnd::Unreachable;
  };
  let Ok(registry) = core.get_registry_rc() else {
    return SessionEnd::Unreachable;
  };
  if let Some(reachable) = maybe_reachable.take() {
    // `start` may have stopped waiting; then nobody uses the lists either.
    let _ = reachable.send(true);
  }

  let graph = Rc::new(RefCell::new(DeviceGraph::default()));
  // Changes before the first `done` are collected silently: the initial listing is one change.
  let is_synced = Rc::new(Cell::new(false));
  let publish = {
    let graph = Rc::clone(&graph);
    let is_synced = Rc::clone(&is_synced);
    let shared = Arc::clone(shared);
    let changes = changes.clone();
    move || {
      if is_synced.get() && shared.publish(&graph.borrow()) {
        changes.send_modify(|generation| *generation += 1);
      }
    }
  };
  let publish = Rc::new(publish);

  // Round trips still open before the first listing is complete: the registry's objects, then
  // the properties of the `default` metadata bound on the way.
  let Ok(first_sync) = core.sync(0) else {
    return SessionEnd::Lost;
  };
  let pending = Rc::new(RefCell::new(vec![first_sync]));
  let loop_for_error = main_loop.clone();
  let _core_listener = core
    .add_listener_local()
    .done({
      let is_synced = Rc::clone(&is_synced);
      let publish = Rc::clone(&publish);
      let pending = Rc::clone(&pending);
      move |id, seq| {
        if id != pw::core::PW_ID_CORE {
          return;
        }
        let mut pending = pending.borrow_mut();
        pending.retain(|open| *open != seq);
        if pending.is_empty() && !is_synced.get() {
          is_synced.set(true);
          publish();
        }
      }
    })
    .error(move |id, _seq, _res, message| {
      if id == pw::core::PW_ID_CORE {
        debug!(message, "PipeWire connection error");
        loop_for_error.quit();
      }
    })
    .register();

  // The `default` metadata object and its listener, kept alive while the session runs.
  let metadata: Rc<RefCell<Option<(pw::metadata::Metadata, pw::metadata::MetadataListener)>>> =
    Rc::new(RefCell::new(None));
  let _registry_listener = registry
    .add_listener_local()
    .global({
      let graph = Rc::clone(&graph);
      let publish = Rc::clone(&publish);
      let registry = registry.clone();
      let metadata = Rc::clone(&metadata);
      let core = core.clone();
      let pending = Rc::clone(&pending);
      move |global| match global.type_ {
        ObjectType::Node => {
          let is_changed = global
            .props
            .is_some_and(|props| graph.borrow_mut().on_node(global.id, props));
          if is_changed {
            publish();
          }
        },
        ObjectType::Metadata => {
          if watch_defaults(&registry, global, &graph, &publish, &metadata)
            && let Ok(seq) = core.sync(0)
          {
            pending.borrow_mut().push(seq);
          }
        },
        _ => {},
      }
    })
    .global_remove({
      let graph = Rc::clone(&graph);
      let publish = Rc::clone(&publish);
      move |id| {
        if graph.borrow_mut().on_remove(id) {
          publish();
        }
      }
    })
    .register();

  main_loop.run();
  SessionEnd::Lost
}

/// Binds the session manager's `default` metadata and follows its default devices; whether it
/// was that object.
fn watch_defaults(
  registry: &pw::registry::RegistryRc,
  global: &GlobalObject<&DictRef>,
  graph: &Rc<RefCell<DeviceGraph>>,
  publish: &Rc<impl Fn() + 'static>,
  slot: &Rc<RefCell<Option<(pw::metadata::Metadata, pw::metadata::MetadataListener)>>>,
) -> bool {
  let is_default = global
    .props
    .is_some_and(|props| props.get("metadata.name") == Some(DEFAULT_METADATA));
  if !is_default {
    return false;
  }
  let metadata: pw::metadata::Metadata = match registry.bind(global) {
    Ok(metadata) => metadata,
    Err(error) => {
      // Without it every device is listed, just none marked as the default.
      warn!(%error, "PipeWire's default devices are not readable");
      return false;
    },
  };
  let graph = Rc::clone(graph);
  let publish = Rc::clone(publish);
  let listener = metadata
    .add_listener_local()
    .property(move |subject, maybe_key, _type, maybe_value| {
      // Subject 0 is the whole system; `None` as key clears every property.
      if subject != 0 {
        return 0;
      }
      let is_changed = match maybe_key {
        Some(key) => graph.borrow_mut().on_default(key, maybe_value),
        None => {
          let mut graph = graph.borrow_mut();
          graph.on_default(DEFAULT_SOURCE_KEY, None) | graph.on_default(DEFAULT_SINK_KEY, None)
        },
      };
      if is_changed {
        publish();
      }
      0
    })
    .register();
  *slot.borrow_mut() = Some((metadata, listener));
  true
}

/// The node name in a default's value, `{"name": "alsa_input.usb-…"}`.
fn default_node_name(value: &str) -> Option<String> {
  let json: serde_json::Value = serde_json::from_str(value).ok()?;
  json.get("name")?.as_str().map(str::to_owned)
}

/// The id cpal's PipeWire host gives the node, so a stored choice opens it.
fn device_id(node_name: &str) -> AudioDeviceId {
  AudioDeviceId::new(cpal::DeviceId::new(cpal::HostId::PipeWire, node_name).to_string())
}

#[cfg(test)]
mod tests {
  use pw::properties::properties;

  use super::*;

  const HEADSET_MIC: &str = "alsa_input.usb-Headset_Vendor_Headset-00.mono-fallback";
  const ONBOARD_MIC: &str = "alsa_input.pci-0000_0f_00.4.analog-stereo";
  const ONBOARD_OUT: &str = "alsa_output.pci-0000_0f_00.4.analog-stereo";

  #[test]
  fn sources_and_sinks_are_separate_lists_named_by_description_default_first() {
    let mut graph = DeviceGraph::default();
    graph.on_node(1, node(SOURCE_CLASS, ONBOARD_MIC, "Onboard Analog Stereo").dict());
    graph.on_node(2, node(SOURCE_CLASS, HEADSET_MIC, "Headset Mono").dict());
    graph.on_node(3, node(SINK_CLASS, ONBOARD_OUT, "Onboard Analog Stereo").dict());
    graph.on_default(
      DEFAULT_SOURCE_KEY,
      Some(&format!(r#"{{"name":"{HEADSET_MIC}"}}"#)),
    );
    let inputs = graph.inputs();
    assert_eq!(names(&inputs), ["Headset Mono", "Onboard Analog Stereo"]);
    assert!(inputs[0].is_default);
    assert_eq!(inputs[0].id.as_str(), format!("pipewire:{HEADSET_MIC}"));
    assert_eq!(names(&graph.outputs()), ["Onboard Analog Stereo"]);
  }

  #[test]
  fn streams_and_internal_nodes_are_no_devices() {
    let mut graph = DeviceGraph::default();
    assert!(!graph.on_node(1, node("Stream/Input/Audio", "browser", "Browser").dict()));
    assert!(!graph.on_node(2, node("Audio/Source/Internal", "filter", "Filter").dict()));
    assert!(!graph.on_node(3, node("Video/Source", "camera", "Camera").dict()));
    assert!(graph.inputs().is_empty() && graph.outputs().is_empty());
  }

  #[test]
  fn a_duplex_node_is_in_both_lists() {
    let mut graph = DeviceGraph::default();
    graph.on_node(1, node(DUPLEX_CLASS, "pro-audio", "Interface").dict());
    assert_eq!(names(&graph.inputs()), ["Interface"]);
    assert_eq!(names(&graph.outputs()), ["Interface"]);
  }

  /// #1072: the same devices announced again (a node's properties re-sent, the same default) are
  /// no change, so nothing tells the user a device went or came.
  #[test]
  fn repeated_announcements_are_no_change() {
    let mut graph = DeviceGraph::default();
    assert!(graph.on_node(1, node(SOURCE_CLASS, HEADSET_MIC, "Headset Mono").dict()));
    assert!(!graph.on_node(1, node(SOURCE_CLASS, HEADSET_MIC, "Headset Mono").dict()));
    let value = format!(r#"{{"name":"{HEADSET_MIC}"}}"#);
    assert!(graph.on_default(DEFAULT_SOURCE_KEY, Some(&value)));
    assert!(!graph.on_default(DEFAULT_SOURCE_KEY, Some(&value)));
    assert!(!graph.on_default("default.configured.audio.source", Some(&value)));
    assert!(!graph.on_remove(7));
  }

  #[test]
  fn unplugging_removes_the_device_and_a_cleared_default_marks_none() {
    let mut graph = DeviceGraph::default();
    graph.on_node(1, node(SOURCE_CLASS, HEADSET_MIC, "Headset Mono").dict());
    graph.on_default(
      DEFAULT_SOURCE_KEY,
      Some(&format!(r#"{{"name":"{HEADSET_MIC}"}}"#)),
    );
    assert!(graph.on_default(DEFAULT_SOURCE_KEY, None));
    assert!(!graph.inputs()[0].is_default);
    assert!(graph.on_remove(1));
    assert!(graph.inputs().is_empty());
  }

  #[test]
  fn malformed_defaults_name_no_device() {
    assert_eq!(default_node_name("not json"), None);
    assert_eq!(default_node_name(r#"{"name":3}"#), None);
    assert_eq!(default_node_name("{}"), None);
    assert_eq!(default_node_name(r#"{ "name": "a" }"#).as_deref(), Some("a"));
  }

  #[test]
  fn a_node_without_description_is_named_by_nick_then_name() {
    let props = properties! {
      *pw::keys::MEDIA_CLASS => SOURCE_CLASS,
      *pw::keys::NODE_NAME => "raw-name",
      *pw::keys::NODE_NICK => "Nick",
    };
    assert_eq!(AudioNode::from_props(props.dict()).unwrap().description, "Nick");
    let props = properties! {
      *pw::keys::MEDIA_CLASS => SOURCE_CLASS,
      *pw::keys::NODE_NAME => "raw-name",
    };
    assert_eq!(
      AudioNode::from_props(props.dict()).unwrap().description,
      "raw-name"
    );
  }

  fn node(class: &str, name: &str, description: &str) -> pw::properties::PropertiesBox {
    properties! {
      *pw::keys::MEDIA_CLASS => class,
      *pw::keys::NODE_NAME => name,
      *pw::keys::NODE_DESCRIPTION => description,
    }
  }

  fn names(infos: &[AudioDeviceInfo]) -> Vec<&str> {
    infos.iter().map(|info| info.name.as_str()).collect()
  }
}
