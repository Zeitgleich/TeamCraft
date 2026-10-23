//! System audio on Linux through PipeWire: every other application's playback, without ours.
//!
//! Capturing the default sink's monitor would include the call itself, and the others would hear
//! themselves. Instead the capture is a stereo input stream that PipeWire does not connect on its
//! own; it watches the registry and links the output ports of every playback stream
//! (`Stream/Output/Audio`) of other processes into it, the way patchbays do. PipeWire mixes what
//! arrives at one input port. Streams that start later are linked when they appear. Which process
//! a stream belongs to is a property of its PipeWire client, not of the node.
//!
//! The ScreenCast portal has no audio, so this needs the PipeWire socket itself; in Flatpak that
//! is `--filesystem=xdg-run/pipewire-0` (packaging/linux). Without it starting fails with
//! [`SystemAudioError::Unsupported`].

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle;

use pipewire as pw;
use pw::properties::properties;
use pw::registry::GlobalObject;
use pw::spa;
use pw::spa::param::audio::{AudioFormat, AudioInfoRaw};
use pw::spa::pod::Pod;
use pw::spa::utils::dict::DictRef;
use pw::types::ObjectType;
use tracing::{debug, info, trace, warn};

use crate::audio_frame::SAMPLE_RATE_HZ;
use crate::system_audio::{MonoResampler, SystemAudioCapture, SystemAudioError, SystemAudioSender};

/// Name of our capture node in PipeWire (shows up in patchbays).
const NODE_NAME: &str = "teamcraft-screen-audio";
const PLAYBACK_STREAM_CLASS: &str = "Stream/Output/Audio";
const LINK_FACTORY: &str = "link-factory";

/// The computer's sound minus this app's, through PipeWire.
#[derive(Default)]
pub struct PipeWireSystemAudio {
  maybe_running: Option<(pw::channel::Sender<()>, JoinHandle<()>)>,
}

/// Left or right input of the capture stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Side {
  Left,
  Right,
}

/// What the registry told us, and the links made from it.
struct Graph {
  own_process: OwnProcess,
  /// Our capture node, once the stream is connected.
  maybe_own_node: Option<u32>,
  /// Clients whose process is known; streams of others wait until theirs is.
  clients: BTreeMap<u32, ClientKind>,
  /// Clients being asked for their full properties.
  client_watches: BTreeMap<u32, (pw::client::Client, pw::client::ClientListener)>,
  /// Playback streams and the client they belong to.
  playback_nodes: BTreeMap<u32, Option<u32>>,
  ports: BTreeMap<u32, PortInfo>,
  /// (output port, input port) pairs already linked.
  linked: BTreeSet<(u32, u32)>,
  /// Link proxies: dropping one removes its link.
  links: Vec<pw::link::Link>,
}

/// Whether a PipeWire client belongs to this process (the call's playback among them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientKind {
  Own,
  Other,
}

/// What to do after a registry event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Follow {
  Nothing,
  /// A node or port may complete a link.
  Relink,
  /// A client's registry entry doesn't say which process it is; its full properties do.
  AskClient,
}

/// How our own playback streams identify themselves.
struct OwnProcess {
  id: String,
  maybe_binary: Option<String>,
  /// The parent of a process id (Linux: `/proc`); our child processes (the WebView's web and
  /// network processes) count as ours, so nothing they play is ever captured.
  parent_of: fn(u32) -> Option<u32>,
}

/// How far up the process tree a child of ours is looked for.
const MAX_PROCESS_DEPTH: usize = 32;

#[derive(Debug, Clone)]
struct PortInfo {
  node: u32,
  is_output: bool,
  channel: String,
}

/// State of the stream callbacks.
struct StreamState {
  resampler: MonoResampler,
  samples: SystemAudioSender,
}

impl PipeWireSystemAudio {
  pub fn new() -> Self {
    Self::default()
  }
}

impl SystemAudioCapture for PipeWireSystemAudio {
  fn start(&mut self, samples: SystemAudioSender) -> Result<(), SystemAudioError> {
    self.stop();
    let (stop, stop_rx) = pw::channel::channel::<()>();
    let (ready, ready_rx) = std_mpsc::channel::<Result<(), SystemAudioError>>();
    let thread = std::thread::Builder::new()
      .name("pipewire-system-audio".into())
      .spawn(move || run_capture(samples, stop_rx, ready))
      .map_err(|error| SystemAudioError::Device(error.to_string()))?;
    // The thread reports whether PipeWire is reachable before it runs its loop.
    match ready_rx.recv() {
      Ok(Ok(())) => {
        self.maybe_running = Some((stop, thread));
        Ok(())
      },
      Ok(Err(error)) => {
        let _ = thread.join();
        Err(error)
      },
      Err(_) => Err(SystemAudioError::Device("the capture thread ended".into())),
    }
  }

  fn stop(&mut self) {
    if let Some((stop, thread)) = self.maybe_running.take() {
      // A closed channel means the loop already ended.
      let _ = stop.send(());
      if thread.join().is_err() {
        warn!("PipeWire system audio thread panicked");
      }
    }
  }
}

impl Drop for PipeWireSystemAudio {
  fn drop(&mut self) {
    self.stop();
  }
}

/// Connects, reports through `ready`, then runs the loop until `stop` fires or the consumer is gone.
fn run_capture(
  samples: SystemAudioSender,
  stop: pw::channel::Receiver<()>,
  ready: std_mpsc::Sender<Result<(), SystemAudioError>>,
) {
  // The loop and the objects it drives live on this thread's stack while it runs.
  let result = run_loop(samples, stop, &ready);
  if let Err(error) = result {
    // Before the loop ran, this is the answer to `start`; afterwards nobody listens any more.
    if ready.send(Err(error)).is_err() {
      debug!("system audio capture ended");
    }
  }
}

fn run_loop(
  samples: SystemAudioSender,
  stop: pw::channel::Receiver<()>,
  ready: &std_mpsc::Sender<Result<(), SystemAudioError>>,
) -> Result<(), SystemAudioError> {
  pw::init();
  let main_loop = pw::main_loop::MainLoopRc::new(None).map_err(device_error)?;
  let context = pw::context::ContextRc::new(&main_loop, None).map_err(device_error)?;
  // No PipeWire socket (Flatpak without the permission, or no PipeWire at all).
  let core = context
    .connect_rc(None)
    .map_err(|_| SystemAudioError::Unsupported)?;
  let registry = core.get_registry_rc().map_err(device_error)?;
  let graph = Rc::new(RefCell::new(Graph::new(OwnProcess::current())));

  let stream = pw::stream::StreamBox::new(
    &core,
    NODE_NAME,
    properties! {
      *pw::keys::MEDIA_TYPE => "Audio",
      *pw::keys::MEDIA_CATEGORY => "Capture",
      *pw::keys::MEDIA_ROLE => "Screen",
      *pw::keys::NODE_NAME => NODE_NAME,
      // We link it ourselves; the session manager must not connect it to a microphone.
      *pw::keys::NODE_AUTOCONNECT => "false",
    },
  )
  .map_err(device_error)?;
  let state = StreamState {
    resampler: MonoResampler::new(2, SAMPLE_RATE_HZ),
    samples,
  };
  let loop_for_quit = main_loop.clone();
  let graph_for_state = Rc::clone(&graph);
  let core_for_state = core.clone();
  let _stream_listener = stream
    .add_local_listener_with_user_data(state)
    .state_changed(move |stream, _, _, new| {
      trace!(?new, "system audio stream state");
      let node = stream.node_id();
      if node != u32::MAX {
        let mut graph = graph_for_state.borrow_mut();
        graph.maybe_own_node = Some(node);
        graph.link_all(&core_for_state);
      }
    })
    .param_changed(|_, state, id, param| {
      let Some(param) = param else {
        return;
      };
      if id != spa::param::ParamType::Format.as_raw() {
        return;
      }
      let mut format = AudioInfoRaw::new();
      match format.parse(param) {
        Ok(_) => state.resampler = MonoResampler::new(format.channels() as usize, format.rate()),
        Err(error) => warn!(?error, "unreadable system audio format"),
      }
    })
    .process(move |stream, state| {
      let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
      };
      let Some(data) = buffer.datas_mut().first_mut() else {
        return;
      };
      let size = data.chunk().size() as usize;
      let Some(bytes) = data.data().and_then(|bytes| bytes.get(..size)) else {
        return;
      };
      let interleaved: Vec<f32> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|sample| f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]))
        .collect();
      if !state.samples.send(state.resampler.process(&interleaved)) {
        loop_for_quit.quit();
      }
    })
    .register()
    .map_err(device_error)?;

  let graph_for_global = Rc::clone(&graph);
  let graph_for_remove = Rc::clone(&graph);
  let core_for_global = core.clone();
  let registry_for_global = registry.clone();
  let _registry_listener = registry
    .add_listener_local()
    .global(move |global| {
      let follow = graph_for_global.borrow_mut().on_global(global);
      match follow {
        Follow::Nothing => {},
        Follow::Relink => graph_for_global.borrow_mut().link_all(&core_for_global),
        Follow::AskClient => watch_client(&registry_for_global, global, &graph_for_global, &core_for_global),
      }
    })
    .global_remove(move |id| graph_for_remove.borrow_mut().on_global_remove(id))
    .register();

  let values = stereo_format().map_err(device_error)?;
  let mut params = [Pod::from_bytes(&values).ok_or_else(|| device_error(pw::Error::CreationFailed))?];
  stream
    .connect(
      spa::utils::Direction::Input,
      None,
      pw::stream::StreamFlags::MAP_BUFFERS,
      &mut params,
    )
    .map_err(device_error)?;

  let loop_for_stop = main_loop.clone();
  let _stop = stop.attach(main_loop.loop_(), move |()| loop_for_stop.quit());
  // `start` waits for this; it may have given up already.
  let _ = ready.send(Ok(()));
  info!("capturing system audio");
  main_loop.run();
  Ok(())
}

/// Binds a client to read its full properties; classifies it when they arrive.
fn watch_client(
  registry: &pw::registry::RegistryRc,
  global: &GlobalObject<&DictRef>,
  graph: &Rc<RefCell<Graph>>,
  core: &pw::core::CoreRc,
) {
  let client: pw::client::Client = match registry.bind(global) {
    Ok(client) => client,
    Err(error) => {
      // Without its properties the client counts as another process's.
      debug!(%error, client = global.id, "client not readable");
      graph.borrow_mut().set_client(global.id, ClientKind::Other);
      return;
    },
  };
  let id = global.id;
  let graph_for_info = Rc::clone(graph);
  let core_for_info = core.clone();
  let listener = client
    .add_listener_local()
    .info(move |info| {
      let Some(props) = info.props() else {
        return;
      };
      let mut graph = graph_for_info.borrow_mut();
      let kind = if graph.own_process.owns(props) {
        ClientKind::Own
      } else {
        ClientKind::Other
      };
      graph.set_client(id, kind);
      graph.link_all(&core_for_info);
    })
    .register();
  graph.borrow_mut().client_watches.insert(id, (client, listener));
}

impl Graph {
  fn new(own_process: OwnProcess) -> Self {
    Self {
      own_process,
      maybe_own_node: None,
      clients: BTreeMap::new(),
      client_watches: BTreeMap::new(),
      playback_nodes: BTreeMap::new(),
      ports: BTreeMap::new(),
      linked: BTreeSet::new(),
      links: Vec::new(),
    }
  }

  /// Records a client, node or port.
  fn on_global(&mut self, global: &GlobalObject<&DictRef>) -> Follow {
    let Some(props) = global.props else {
      return Follow::Nothing;
    };
    match global.type_ {
      ObjectType::Client => {
        // The pid the server saw on the socket: ours unless we are in a sandbox with its own
        // pid namespace (Flatpak), where only the client's own properties tell.
        if props
          .get(*pw::keys::SEC_PID)
          .is_some_and(|pid| self.own_process.is_own_or_child(pid))
        {
          self.set_client(global.id, ClientKind::Own);
          return Follow::Relink;
        }
        Follow::AskClient
      },
      ObjectType::Node => {
        if props.get(*pw::keys::MEDIA_CLASS) != Some(PLAYBACK_STREAM_CLASS) {
          return Follow::Nothing;
        }
        let maybe_client = props.get(*pw::keys::CLIENT_ID).and_then(|id| id.parse().ok());
        trace!(node = global.id, ?maybe_client, "playback stream");
        self.playback_nodes.insert(global.id, maybe_client);
        Follow::Relink
      },
      ObjectType::Port => {
        let Some(node) = props.get(*pw::keys::NODE_ID).and_then(|id| id.parse().ok()) else {
          return Follow::Nothing;
        };
        // Monitor ports mirror a node's input; they are not playback.
        if props.get("port.monitor") == Some("true") {
          return Follow::Nothing;
        }
        self.ports.insert(
          global.id,
          PortInfo {
            node,
            is_output: props.get(*pw::keys::PORT_DIRECTION) == Some("out"),
            channel: props.get(*pw::keys::AUDIO_CHANNEL).unwrap_or("MONO").to_owned(),
          },
        );
        Follow::Relink
      },
      _ => Follow::Nothing,
    }
  }

  /// A client's process is known (from its full properties when needed).
  fn set_client(&mut self, client: u32, kind: ClientKind) {
    trace!(client, ?kind, "client classified");
    self.clients.insert(client, kind);
    self.client_watches.remove(&client);
  }

  fn on_global_remove(&mut self, id: u32) {
    // PipeWire removes the links of a vanished port itself.
    self.clients.remove(&id);
    self.client_watches.remove(&id);
    self.playback_nodes.remove(&id);
    self.ports.remove(&id);
    self
      .linked
      .retain(|(output, input)| *output != id && *input != id);
  }

  /// Links every source output port to the matching input of our node, once.
  fn link_all(&mut self, core: &pw::core::CoreRc) {
    for (output, input) in self.missing_links() {
      let Some(own_node) = self.maybe_own_node else {
        return;
      };
      let Some(source_node) = self.ports.get(&output).map(|port| port.node) else {
        continue;
      };
      let link = core.create_object::<pw::link::Link>(
        LINK_FACTORY,
        &properties! {
          *pw::keys::LINK_OUTPUT_NODE => source_node.to_string(),
          *pw::keys::LINK_OUTPUT_PORT => output.to_string(),
          *pw::keys::LINK_INPUT_NODE => own_node.to_string(),
          *pw::keys::LINK_INPUT_PORT => input.to_string(),
          // The link goes when our proxy does (the capture stops).
          *pw::keys::OBJECT_LINGER => "false",
        },
      );
      match link {
        Ok(link) => {
          self.links.push(link);
          self.linked.insert((output, input));
        },
        Err(error) => warn!(%error, output, input, "linking a playback stream failed"),
      }
    }
  }

  /// (output port, input port) pairs that should exist and don't yet.
  fn missing_links(&self) -> Vec<(u32, u32)> {
    let Some(own_node) = self.maybe_own_node else {
      return Vec::new();
    };
    let inputs: BTreeMap<Side, u32> = self
      .ports
      .iter()
      .filter(|(_, port)| port.node == own_node && !port.is_output)
      .filter_map(|(&id, port)| side_of_input(&port.channel).map(|side| (side, id)))
      .collect();
    self
      .ports
      .iter()
      .filter(|(_, port)| port.is_output && self.is_source(port.node))
      .flat_map(|(&output, port)| {
        targets_of(&port.channel)
          .iter()
          .filter_map(|side| inputs.get(side).map(|&input| (output, input)))
          .collect::<Vec<_>>()
      })
      .filter(|pair| !self.linked.contains(pair))
      .collect()
  }
}

impl Graph {
  /// A playback stream of another process. Streams of clients not classified yet wait, so the
  /// call's own playback is never linked, not even briefly.
  fn is_source(&self, node: u32) -> bool {
    match self.playback_nodes.get(&node) {
      Some(Some(client)) => self.clients.get(client) == Some(&ClientKind::Other),
      // A stream without a client can't be told apart; better not to capture it.
      Some(None) | None => false,
    }
  }
}

impl OwnProcess {
  fn current() -> Self {
    Self {
      id: std::process::id().to_string(),
      maybe_binary: std::env::current_exe()
        .ok()
        .and_then(|path| path.file_name().map(|name| name.to_string_lossy().into_owned())),
      parent_of: parent_from_proc,
    }
  }

  /// This process or one it started (directly or further down). Misjudging another app's
  /// process as ours only leaves its sound out, never adds ours.
  fn is_own_or_child(&self, pid: &str) -> bool {
    if pid == self.id {
      return true;
    }
    let (Ok(mut current), Ok(own)) = (pid.parse::<u32>(), self.id.parse::<u32>()) else {
      return false;
    };
    for _ in 0..MAX_PROCESS_DEPTH {
      match (self.parent_of)(current) {
        Some(parent) if parent == own => return true,
        // Process 1 (and 0) has no parent of interest.
        Some(parent) if parent > 1 => current = parent,
        _ => return false,
      }
    }
    false
  }

  /// A client of this process (from its properties). Sandboxes have their own process ids, so
  /// another sandboxed app can have ours; the binary name tells them apart where the client
  /// names it.
  fn owns(&self, props: &DictRef) -> bool {
    let maybe_pid = props.get(*pw::keys::APP_PROCESS_ID);
    let is_same_id = maybe_pid == Some(self.id.as_str());
    let is_same_binary = match (props.get(*pw::keys::APP_PROCESS_BINARY), &self.maybe_binary) {
      (Some(binary), Some(own)) => binary == own,
      _ => true,
    };
    // Our children run other binaries (WebKitWebProcess); their parent tells.
    let is_child = !is_same_id && maybe_pid.is_some_and(|pid| self.is_own_or_child(pid));
    (is_same_id && is_same_binary) || is_child
  }
}

/// The parent process id from `/proc/<pid>/stat` (the field after the command name, which may
/// itself contain spaces and parentheses).
fn parent_from_proc(pid: u32) -> Option<u32> {
  let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
  let (_, after_name) = stat.rsplit_once(')')?;
  after_name.split_whitespace().nth(1)?.parse().ok()
}

/// Which of our inputs a source channel goes to: left and right channels to their side,
/// everything else (mono, center, LFE) to both.
fn targets_of(channel: &str) -> &'static [Side] {
  match channel {
    "FL" | "SL" | "RL" | "FLC" | "TFL" | "TRL" => &[Side::Left],
    "FR" | "SR" | "RR" | "FRC" | "TFR" | "TRR" => &[Side::Right],
    _ => &[Side::Left, Side::Right],
  }
}

fn side_of_input(channel: &str) -> Option<Side> {
  match channel {
    "FL" => Some(Side::Left),
    "FR" => Some(Side::Right),
    _ => None,
  }
}

/// 32-bit float stereo at the call rate; PipeWire converts whatever the sources play.
fn stereo_format() -> Result<Vec<u8>, pw::Error> {
  let mut info = AudioInfoRaw::new();
  info.set_format(AudioFormat::F32LE);
  info.set_rate(SAMPLE_RATE_HZ);
  info.set_channels(2);
  let mut position = [0; spa::sys::SPA_AUDIO_MAX_CHANNELS as usize];
  position[0] = spa::sys::SPA_AUDIO_CHANNEL_FL;
  position[1] = spa::sys::SPA_AUDIO_CHANNEL_FR;
  info.set_position(position);
  let object = spa::pod::Object {
    type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
    id: spa::param::ParamType::EnumFormat.as_raw(),
    properties: info.into(),
  };
  spa::pod::serialize::PodSerializer::serialize(
    std::io::Cursor::new(Vec::new()),
    &spa::pod::Value::Object(object),
  )
  .map(|(cursor, _)| cursor.into_inner())
  .map_err(|_| pw::Error::CreationFailed)
}

fn device_error(error: pw::Error) -> SystemAudioError {
  SystemAudioError::Device(error.to_string())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn stereo_and_surround_channels_go_to_their_side() {
    assert_eq!(targets_of("FL"), &[Side::Left]);
    assert_eq!(targets_of("RR"), &[Side::Right]);
    assert_eq!(targets_of("MONO"), &[Side::Left, Side::Right]);
    assert_eq!(targets_of("FC"), &[Side::Left, Side::Right]);
    assert_eq!(side_of_input("FR"), Some(Side::Right));
    assert_eq!(side_of_input("AUX0"), None);
  }

  #[test]
  fn our_child_processes_count_as_ours() {
    // 300 is the WebView's web process, started by 200, started by us (100); 400 is another app.
    let own = OwnProcess {
      id: "100".into(),
      maybe_binary: Some("teamcraft-desktop".into()),
      parent_of: |pid| match pid {
        300 => Some(200),
        200 => Some(100),
        400 => Some(1),
        _ => None,
      },
    };
    assert!(own.is_own_or_child("100"));
    assert!(own.is_own_or_child("300"));
    assert!(!own.is_own_or_child("400"));
    assert!(!own.is_own_or_child("not a pid"));
  }

  #[test]
  fn the_parent_is_read_from_proc() {
    let child = std::process::Command::new("sleep").arg("5").spawn().unwrap();
    assert_eq!(parent_from_proc(child.id()), Some(std::process::id()));
    let mut child = child;
    child.kill().unwrap();
    child.wait().unwrap();
  }

  #[test]
  fn other_apps_are_linked_and_ours_is_not() {
    let mut graph = Graph::new(OwnProcess {
      id: "100".into(),
      maybe_binary: Some("teamcraft-desktop".into()),
      parent_of: |_| None,
    });
    graph.maybe_own_node = Some(1);
    graph.ports.insert(10, port(1, false, "FL"));
    graph.ports.insert(11, port(1, false, "FR"));
    // A music player in stereo and a mono notification sound from other clients, and the call's
    // own playback.
    graph.clients.extend([
      (90, ClientKind::Own),
      (91, ClientKind::Other),
      (92, ClientKind::Other),
    ]);
    graph
      .playback_nodes
      .extend([(2, Some(91)), (3, Some(92)), (4, Some(90))]);
    graph.ports.insert(40, port(4, true, "FL"));
    graph.ports.insert(20, port(2, true, "FL"));
    graph.ports.insert(21, port(2, true, "FR"));
    graph.ports.insert(30, port(3, true, "MONO"));
    assert_eq!(
      graph.missing_links(),
      vec![(20, 10), (21, 11), (30, 10), (30, 11)]
    );
    graph.linked.extend([(20, 10), (21, 11)]);
    assert_eq!(graph.missing_links(), vec![(30, 10), (30, 11)]);
    graph.on_global_remove(3);
    graph.on_global_remove(30);
    assert!(graph.missing_links().is_empty());
  }

  #[test]
  fn nothing_is_linked_before_our_node_exists() {
    let mut graph = Graph::new(OwnProcess {
      id: "100".into(),
      maybe_binary: None,
      parent_of: |_| None,
    });
    graph.playback_nodes.insert(2, Some(91));
    graph.ports.insert(20, port(2, true, "FL"));
    assert!(graph.missing_links().is_empty());
  }

  #[test]
  fn streams_of_unclassified_clients_wait() {
    let mut graph = Graph::new(OwnProcess {
      id: "100".into(),
      maybe_binary: None,
      parent_of: |_| None,
    });
    graph.maybe_own_node = Some(1);
    graph.ports.insert(10, port(1, false, "FL"));
    graph.playback_nodes.insert(2, Some(91));
    graph.ports.insert(20, port(2, true, "FL"));
    assert!(graph.missing_links().is_empty());
    graph.set_client(91, ClientKind::Other);
    assert_eq!(graph.missing_links(), vec![(20, 10)]);
  }

  #[test]
  fn the_stream_format_serializes() {
    let bytes = stereo_format().unwrap();
    assert!(Pod::from_bytes(&bytes).is_some());
  }

  fn port(node: u32, is_output: bool, channel: &str) -> PortInfo {
    PortInfo {
      node,
      is_output,
      channel: channel.to_owned(),
    }
  }
}
