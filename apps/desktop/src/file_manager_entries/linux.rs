//! The Linux side of [`super`]: the files, per file manager.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager};
use tracing::{debug, warn};

use super::FileManagerEntriesError;
use crate::autostart::xdg;

/// Written into every file (and action) of ours; the only files the app changes or deletes.
const MARKER: &str = "X-TeamCraft-Generated=true";
/// The Flatpak's ID, which `flatpak run` starts.
const APP_ID: &str = "de.zeitgleich.teamcraft";
/// The command the packages install.
const INSTALLED_COMMAND: &str = "teamcraft-desktop";
/// Thunar finds its action in `uca.xml` by this.
const THUNAR_ACTION_ID: &str = "de.zeitgleich.teamcraft.send-to";
/// File names are fixed for desktop entries and actions; scripts are named like their menu entry.
const ENTRY_FILE_NAME: &str = "de.zeitgleich.teamcraft.send-to";
/// How much of a file is read to look for the marker; ours are far smaller.
const MARKER_SCAN_BYTES: u64 = 8 * 1024;
/// A `uca.xml` larger than this is not touched.
const MAX_UCA_BYTES: u64 = 4 * 1024 * 1024;
/// The user's choice when it is off; no file means on.
const STATE_FILE: &str = "file-manager-entries";
const STATE_OFF: &str = "off";

const EN_SOURCE: &str = include_str!("../../../../crates/teamcraft-ui/i18n/en/shell.ftl");
const DE_SOURCE: &str = include_str!("../../../../crates/teamcraft-ui/i18n/de/shell.ftl");

pub(super) fn is_enabled(app: &AppHandle) -> Option<bool> {
  Some(is_on(&state_path(app)?))
}

pub(super) async fn set_enabled(app: &AppHandle, enabled: bool) -> Result<(), FileManagerEntriesError> {
  let state = state_path(app).ok_or_else(|| failed("no configuration directory"))?;
  let plan = Plan::detect(app);
  tauri::async_runtime::spawn_blocking(move || {
    sync(&plan, enabled)?;
    store(&state, enabled).map_err(failed)
  })
  .await
  .map_err(failed)?
}

pub(super) fn repair(app: &AppHandle) {
  let Some(state) = state_path(app) else {
    return;
  };
  let plan = Plan::detect(app);
  tauri::async_runtime::spawn_blocking(move || {
    if let Err(error) = sync(&plan, is_on(&state)) {
      warn!(error = %error.0, "file manager entries not up to date");
    }
  });
}

/// Everything the entries depend on, so the files can be computed (and tested) without the system.
#[derive(Debug, Clone)]
struct Plan {
  /// `~/.local/share` (the host's, also inside the Flatpak).
  data_home: PathBuf,
  /// `~/.config`.
  config_home: PathBuf,
  launcher: Launcher,
  language: Language,
  /// Managers to write for; the others only have stale entries removed.
  installed: Vec<FileManager>,
}

/// How the entry starts the app.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Launcher {
  /// deb and rpm: `teamcraft-desktop` from the `PATH`.
  Installed,
  /// The AppImage file, or a binary that is not the installed one.
  Path(PathBuf),
  Flatpak,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Language {
  En,
  De,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileManager {
  Dolphin,
  Nautilus,
  Nemo,
  Caja,
  Thunar,
}

/// One file to keep: written when wanted, otherwise removed (if it is ours).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
  directory: PathBuf,
  file_name: String,
  content: String,
  is_executable: bool,
}

impl FileManager {
  const ALL: [FileManager; 5] = [
    Self::Dolphin,
    Self::Nautilus,
    Self::Nemo,
    Self::Caja,
    Self::Thunar,
  ];

  /// The program whose presence on the `PATH` says the manager is installed.
  fn program(self) -> &'static str {
    match self {
      Self::Dolphin => "dolphin",
      Self::Nautilus => "nautilus",
      Self::Nemo => "nemo",
      Self::Caja => "caja",
      Self::Thunar => "thunar",
    }
  }

  /// The directory that holds the entry (for Thunar the directory of `uca.xml`).
  fn directory(self, plan: &Plan) -> PathBuf {
    match self {
      Self::Dolphin => plan.data_home.join("kio/servicemenus"),
      Self::Nautilus => plan.data_home.join("nautilus/scripts"),
      Self::Nemo => plan.data_home.join("nemo/actions"),
      Self::Caja => plan.config_home.join("caja/scripts"),
      Self::Thunar => plan.config_home.join("Thunar"),
    }
  }
}

impl Language {
  /// The first set of `LC_ALL`, `LC_MESSAGES`, `LANGUAGE`, `LANG`, as the C library picks messages.
  fn from_environment(variable: impl Fn(&str) -> Option<OsString>) -> Self {
    ["LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"]
      .into_iter()
      .filter_map(&variable)
      .map(|value| value.to_string_lossy().to_ascii_lowercase())
      .find(|value| !value.is_empty() && value != "c" && value != "posix")
      .map_or(Self::En, |value| {
        if value.starts_with("de") {
          Self::De
        } else {
          Self::En
        }
      })
  }

  fn text(self, key: &str) -> &'static str {
    let source = match self {
      Self::En => EN_SOURCE,
      Self::De => DE_SOURCE,
    };
    ftl_value(source, key)
      .or_else(|| ftl_value(EN_SOURCE, key))
      .unwrap_or_default()
  }
}

impl Launcher {
  /// The words of the command, before `--share`.
  fn words(&self) -> Vec<String> {
    match self {
      Self::Installed => vec![INSTALLED_COMMAND.to_owned()],
      Self::Path(path) => vec![path.to_string_lossy().into_owned()],
      Self::Flatpak => ["flatpak", "run", "--file-forwarding", APP_ID]
        .map(str::to_owned)
        .to_vec(),
    }
  }

  /// The `Exec` line of a desktop entry or Nemo action; `%F` stands for the selected paths.
  fn desktop_exec(&self) -> String {
    let command: Vec<String> = self.words().iter().map(|word| exec_word(word)).collect();
    format!("{} --share -- {}", command.join(" "), self.files("%F"))
  }

  /// The command line of a shell script or Thunar action; `files` is what stands for the paths.
  fn shell_command(&self, files: &str) -> String {
    let command: Vec<String> = self.words().iter().map(|word| shell_quote(word)).collect();
    format!("{} --share -- {}", command.join(" "), self.files(files))
  }

  /// Flatpak exports the files it finds between `@@` markers through the document portal.
  fn files(&self, files: &str) -> String {
    match self {
      Self::Flatpak => format!("@@ {files} @@"),
      Self::Installed | Self::Path(_) => files.to_owned(),
    }
  }
}

impl Plan {
  fn detect(app: &AppHandle) -> Self {
    let variable = |name: &str| std::env::var_os(name);
    let is_flatpak = xdg::is_flatpak();
    let (data_home, config_home) = home_directories(variable, is_flatpak);
    let launcher = detect_launcher(
      app.env().appimage.map(PathBuf::from),
      std::env::current_exe().ok(),
      is_flatpak,
      variable("PATH"),
    );
    let mut plan = Self {
      data_home,
      config_home,
      launcher,
      language: Language::from_environment(variable),
      installed: Vec::new(),
    };
    plan.installed = FileManager::ALL
      .into_iter()
      // The Flatpak can't see the host's programs; it has write access to the directories instead.
      .filter(|manager| is_flatpak || is_installed(*manager, &plan, variable("PATH")))
      .collect();
    plan
  }

  /// The entries wanted for `manager`.
  fn entries(&self, manager: FileManager) -> Vec<Entry> {
    let name = self.language.text("shell-send-to-name");
    let directory = manager.directory(self);
    match manager {
      FileManager::Dolphin => vec![Entry {
        directory,
        file_name: format!("{ENTRY_FILE_NAME}.desktop"),
        content: self.service_menu(),
        // Plasma 6 only shows user service menus that are executable.
        is_executable: true,
      }],
      FileManager::Nemo => vec![Entry {
        directory,
        file_name: format!("{ENTRY_FILE_NAME}.nemo_action"),
        content: self.nemo_action(),
        is_executable: false,
      }],
      FileManager::Nautilus => vec![self.script(directory, name, "NAUTILUS_SCRIPT_SELECTED_FILE_PATHS")],
      FileManager::Caja => vec![self.script(directory, name, "CAJA_SCRIPT_SELECTED_FILE_PATHS")],
      FileManager::Thunar => Vec::new(),
    }
  }

  fn service_menu(&self) -> String {
    let (name, name_de) = (
      Language::En.text("shell-send-to-name"),
      Language::De.text("shell-send-to-name"),
    );
    format!(
      "[Desktop Entry]\n\
       Type=Service\n\
       Name={name}\n\
       Name[de]={name_de}\n\
       X-KDE-ServiceTypes=KonqPopupMenu/Plugin\n\
       MimeType=all/allfiles;\n\
       X-KDE-Protocols=file\n\
       Actions=SendToTeamCraft;\n\
       {MARKER}\n\
       \n\
       [Desktop Action SendToTeamCraft]\n\
       Name={name}\n\
       Name[de]={name_de}\n\
       Icon={APP_ID}\n\
       Exec={}\n",
      self.launcher.desktop_exec()
    )
  }

  fn nemo_action(&self) -> String {
    let (name, name_de) = (
      Language::En.text("shell-send-to-name"),
      Language::De.text("shell-send-to-name"),
    );
    let (comment, comment_de) = (
      Language::En.text("shell-send-to-comment"),
      Language::De.text("shell-send-to-comment"),
    );
    format!(
      "[Nemo Action]\n\
       Name={name}\n\
       Name[de]={name_de}\n\
       Comment={comment}\n\
       Comment[de]={comment_de}\n\
       Exec={}\n\
       Icon-Name={APP_ID}\n\
       Selection=NotNone\n\
       Extensions=any;\n\
       Quote=double\n\
       {MARKER}\n",
      self.launcher.desktop_exec()
    )
  }

  /// A script for the managers that list scripts under their name. The paths come newline-separated
  /// in `variable`, only for local files; they are read into the arguments, so spaces and quotes
  /// in names stay what they are.
  fn script(&self, directory: PathBuf, name: &str, variable: &str) -> Entry {
    let content = format!(
      "#!/bin/sh\n\
       # {MARKER}\n\
       # Written by TeamCraft; turn it off in Settings > Preferences.\n\
       set --\n\
       while IFS= read -r path; do\n\
       \x20 [ -n \"$path\" ] && set -- \"$@\" \"$path\"\n\
       done <<EOF\n\
       ${variable}\n\
       EOF\n\
       [ \"$#\" -gt 0 ] || exit 0\n\
       exec {}\n",
      self.launcher.shell_command("\"$@\"")
    );
    Entry {
      directory,
      file_name: name.to_owned(),
      content,
      is_executable: true,
    }
  }

  /// The `<action>` of Thunar's `uca.xml`.
  fn thunar_action(&self) -> String {
    let name = xml_escape(self.language.text("shell-send-to-name"));
    let comment = xml_escape(self.language.text("shell-send-to-comment"));
    let command = xml_escape(&self.launcher.shell_command("%F"));
    format!(
      "<action>\n\
       \t<!-- {MARKER} -->\n\
       \t<icon>{APP_ID}</icon>\n\
       \t<name>{name}</name>\n\
       \t<unique-id>{THUNAR_ACTION_ID}</unique-id>\n\
       \t<command>{command}</command>\n\
       \t<description>{comment}</description>\n\
       \t<patterns>*</patterns>\n\
       \t<audio-files/>\n\
       \t<image-files/>\n\
       \t<other-files/>\n\
       \t<text-files/>\n\
       \t<video-files/>\n\
       </action>"
    )
  }
}

/// Brings the files in line: wanted entries written, every other entry of ours removed. Continues
/// past a failure so one unwritable directory doesn't keep the others stale; the first failure is
/// the error.
fn sync(plan: &Plan, is_enabled: bool) -> Result<(), FileManagerEntriesError> {
  let mut failures: Vec<String> = Vec::new();
  for manager in FileManager::ALL {
    let is_wanted = is_enabled && plan.installed.contains(&manager);
    let result = match manager {
      FileManager::Thunar => sync_thunar(plan, is_wanted),
      _ => sync_directory(plan, manager, is_wanted),
    };
    if let Err(error) = result {
      warn!(?manager, %error, "file manager entry not written");
      failures.push(format!("{manager:?}: {error}"));
    }
  }
  match failures.first() {
    None => Ok(()),
    Some(first) => Err(failed(format!("{first} ({} failed)", failures.len()))),
  }
}

fn sync_directory(plan: &Plan, manager: FileManager, is_wanted: bool) -> io::Result<()> {
  let directory = manager.directory(plan);
  let wanted = if is_wanted {
    plan.entries(manager)
  } else {
    Vec::new()
  };
  // Ours but not wanted any more: a changed language renames a script, a manager was uninstalled.
  for stale in ours_in(&directory)? {
    if !wanted.iter().any(|entry| entry.file_name == stale) {
      debug!(path = %directory.join(&stale).display(), "removing entry");
      fs::remove_file(directory.join(&stale))?;
    }
  }
  wanted.iter().try_for_each(write_entry)
}

/// The names of our files in `directory`; none if it doesn't exist.
fn ours_in(directory: &Path) -> io::Result<Vec<String>> {
  let reader = match fs::read_dir(directory) {
    Ok(reader) => reader,
    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
    Err(error) => return Err(error),
  };
  let mut found = Vec::new();
  for item in reader {
    let item = item?;
    // Sub-folders and links are never ours: we write plain files.
    if item.file_type()?.is_file()
      && let Some(name) = item.file_name().to_str()
      && read_head(&item.path()).is_ok_and(|head| is_ours(&head))
    {
      found.push(name.to_owned());
    }
  }
  Ok(found)
}

/// Writes the entry unless it already says exactly this. A file of that name that isn't ours is left
/// alone.
fn write_entry(entry: &Entry) -> io::Result<()> {
  let path = entry.directory.join(&entry.file_name);
  let mode = if entry.is_executable { 0o755 } else { 0o644 };
  match fs::symlink_metadata(&path) {
    Ok(metadata) => {
      if !metadata.is_file() || !is_ours(&read_head(&path)?) {
        warn!(path = %path.display(), "not ours, left as it is");
        return Ok(());
      }
      if fs::read_to_string(&path).is_ok_and(|existing| existing == entry.content)
        && metadata.permissions().mode() & 0o777 == mode
      {
        return Ok(());
      }
    },
    Err(error) if error.kind() == io::ErrorKind::NotFound => {},
    Err(error) => return Err(error),
  }
  fs::create_dir_all(&entry.directory)?;
  // A dot file next to it is no menu entry yet; the rename makes the finished file appear at once.
  let temporary = entry.directory.join(format!(".{}.tmp", entry.file_name));
  fs::write(&temporary, &entry.content)?;
  fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))?;
  fs::rename(&temporary, &path).inspect_err(|_| {
    // Best effort: the rename's error is the one that counts.
    let _ = fs::remove_file(&temporary);
  })
}

fn sync_thunar(plan: &Plan, is_wanted: bool) -> io::Result<()> {
  let path = FileManager::Thunar.directory(plan).join("uca.xml");
  let existing = match fs::metadata(&path) {
    Ok(metadata) if metadata.len() > MAX_UCA_BYTES => {
      return Err(io::Error::other("uca.xml is unusually large"));
    },
    Ok(_) => Some(fs::read_to_string(&path)?),
    Err(error) if error.kind() == io::ErrorKind::NotFound => None,
    Err(error) => return Err(error),
  };
  let action = is_wanted.then(|| plan.thunar_action());
  let merged = merge_uca(existing.as_deref(), action.as_deref())
    .map_err(|error| io::Error::other(format!("uca.xml: {error}")))?;
  let Some(content) = merged else {
    return Ok(());
  };
  if let Some(directory) = path.parent() {
    fs::create_dir_all(directory)?;
  }
  let temporary = path.with_extension("xml.tmp");
  fs::write(&temporary, content)?;
  fs::rename(&temporary, &path).inspect_err(|_| {
    let _ = fs::remove_file(&temporary);
  })
}

/// Why Thunar's action file can't be merged into; the file is then left as it is.
#[derive(Debug, thiserror::Error)]
enum UcaError {
  #[error("not well-formed: {0}")]
  Parse(#[from] roxmltree::Error),
  #[error("the root element is not <actions>")]
  NotActions,
}

/// `uca.xml` with our action added, replaced or removed, as text: everything else stays byte for byte
/// (the user's own actions, comments, order). Our action is found by its `<unique-id>`. `None` when
/// nothing changes, which includes removing from a missing file.
fn merge_uca(existing: Option<&str>, action: Option<&str>) -> Result<Option<String>, UcaError> {
  let Some(text) = existing else {
    return Ok(action.map(|action| {
      format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<actions>\n{action}\n</actions>\n")
    }));
  };
  let document = roxmltree::Document::parse(text)?;
  let root = document.root_element();
  if root.tag_name().name() != "actions" {
    return Err(UcaError::NotActions);
  }
  let ours = root.children().find(|node| {
    node.has_tag_name("action")
      && node
        .children()
        .any(|child| child.has_tag_name("unique-id") && child.text().map(str::trim) == Some(THUNAR_ACTION_ID))
  });
  let mut merged = text.to_owned();
  match (ours, action) {
    (None, None) => return Ok(None),
    (Some(node), Some(action)) => {
      let range = node.range();
      if &text[range.clone()] == action {
        return Ok(None);
      }
      merged.replace_range(range, action);
    },
    (Some(node), None) => {
      let range = node.range();
      // The line goes, not only the element: the indentation before it and the line break after.
      let start = text[..range.start].trim_end_matches([' ', '\t']).len();
      let end = if text[range.end..].starts_with('\n') {
        range.end + 1
      } else {
        range.end
      };
      merged.replace_range(start..end, "");
    },
    (None, Some(action)) => {
      let range = root.range();
      let closing = "</actions>";
      if text[range.clone()].ends_with(closing) {
        merged.insert_str(range.end - closing.len(), &format!("{action}\n"));
      } else {
        // `<actions/>`
        merged.replace_range(range, &format!("<actions>\n{action}\n</actions>"));
      }
    },
  }
  Ok(Some(merged))
}

fn is_ours(head: &str) -> bool {
  head
    .lines()
    .any(|line| line.trim_start_matches('#').trim() == MARKER)
}

fn read_head(path: &Path) -> io::Result<String> {
  let mut bytes = Vec::new();
  fs::File::open(path)?
    .take(MARKER_SCAN_BYTES)
    .read_to_end(&mut bytes)?;
  Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The host's data and config directories. Inside the Flatpak `XDG_*_HOME` point into the app's
/// own folder; the host's are in `HOST_XDG_*_HOME` (newer Flatpak) or the usual places in `$HOME`.
fn home_directories(variable: impl Fn(&str) -> Option<OsString>, is_flatpak: bool) -> (PathBuf, PathBuf) {
  let home = variable("HOME").map(PathBuf::from).unwrap_or_default();
  let directory = |name: &str, fallback: &str| {
    let name = if is_flatpak {
      format!("HOST_{name}")
    } else {
      name.to_owned()
    };
    variable(&name)
      .map(PathBuf::from)
      .filter(|path| path.is_absolute())
      .unwrap_or_else(|| home.join(fallback))
  };
  (
    directory("XDG_DATA_HOME", ".local/share"),
    directory("XDG_CONFIG_HOME", ".config"),
  )
}

/// `teamcraft-desktop` when the running binary is the one on the `PATH` (deb, rpm); otherwise the
/// running file, which also is the AppImage's own file.
fn detect_launcher(
  maybe_appimage: Option<PathBuf>,
  maybe_exe: Option<PathBuf>,
  is_flatpak: bool,
  maybe_path: Option<OsString>,
) -> Launcher {
  if is_flatpak {
    return Launcher::Flatpak;
  }
  if let Some(appimage) = maybe_appimage {
    return Launcher::Path(appimage);
  }
  let Some(exe) = maybe_exe else {
    return Launcher::Installed;
  };
  let on_path = find_on_path(INSTALLED_COMMAND, maybe_path.as_deref());
  match (
    on_path.and_then(|path| fs::canonicalize(path).ok()),
    fs::canonicalize(&exe),
  ) {
    (Some(installed), Ok(running)) if installed == running => Launcher::Installed,
    _ => Launcher::Path(exe),
  }
}

fn is_installed(manager: FileManager, plan: &Plan, maybe_path: Option<OsString>) -> bool {
  find_on_path(manager.program(), maybe_path.as_deref()).is_some() || manager.directory(plan).is_dir()
}

fn find_on_path(program: &str, maybe_path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
  std::env::split_paths(maybe_path?)
    .map(|directory| directory.join(program))
    .find(|candidate| candidate.is_file())
}

/// An argument of an `Exec` key or action: as is when plain, else quoted by the desktop entry spec.
fn exec_word(word: &str) -> String {
  if is_plain(word) {
    word.to_owned()
  } else {
    xdg::exec_argument(word)
  }
}

/// An argument of a POSIX shell command: as is when plain, else in single quotes.
fn shell_quote(word: &str) -> String {
  if is_plain(word) {
    word.to_owned()
  } else {
    format!("'{}'", word.replace('\'', "'\\''"))
  }
}

fn is_plain(word: &str) -> bool {
  !word.is_empty()
    && word
      .chars()
      .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+' | ':' | '@' | '='))
}

fn xml_escape(text: &str) -> String {
  text
    .replace('&', "&amp;")
    .replace('<', "&lt;")
    .replace('>', "&gt;")
    .replace('"', "&quot;")
}

/// The value of `key = value` in plain-text Fluent.
fn ftl_value(source: &'static str, key: &str) -> Option<&'static str> {
  source.lines().find_map(|line| {
    let (name, value) = line.split_once('=')?;
    (name.trim() == key).then(|| value.trim())
  })
}

fn state_path(app: &AppHandle) -> Option<PathBuf> {
  app.path().app_config_dir().ok().map(|dir| dir.join(STATE_FILE))
}

/// On unless the state file says off.
fn is_on(path: &Path) -> bool {
  fs::read_to_string(path).map_or(true, |text| text.trim() != STATE_OFF)
}

fn store(path: &Path, is_enabled: bool) -> io::Result<()> {
  if let Some(directory) = path.parent() {
    fs::create_dir_all(directory)?;
  }
  if is_enabled {
    match fs::remove_file(path) {
      Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
      _ => Ok(()),
    }
  } else {
    fs::write(path, STATE_OFF)
  }
}

fn failed(error: impl std::fmt::Display) -> FileManagerEntriesError {
  FileManagerEntriesError::new(error)
}

#[cfg(test)]
mod tests {
  use super::*;

  const SERVICE_MENU: &str = "[Desktop Entry]
Type=Service
Name=Send to TeamCraft
Name[de]=An TeamCraft senden
X-KDE-ServiceTypes=KonqPopupMenu/Plugin
MimeType=all/allfiles;
X-KDE-Protocols=file
Actions=SendToTeamCraft;
X-TeamCraft-Generated=true

[Desktop Action SendToTeamCraft]
Name=Send to TeamCraft
Name[de]=An TeamCraft senden
Icon=de.zeitgleich.teamcraft
Exec=teamcraft-desktop --share -- %F
";

  const NAUTILUS_SCRIPT: &str = "#!/bin/sh
# X-TeamCraft-Generated=true
# Written by TeamCraft; turn it off in Settings > Preferences.
set --
while IFS= read -r path; do
  [ -n \"$path\" ] && set -- \"$@\" \"$path\"
done <<EOF
$NAUTILUS_SCRIPT_SELECTED_FILE_PATHS
EOF
[ \"$#\" -gt 0 ] || exit 0
exec teamcraft-desktop --share -- \"$@\"
";

  mod contents {
    use super::*;

    #[test]
    fn dolphin_service_menu() {
      assert_eq!(plan(Launcher::Installed).service_menu(), SERVICE_MENU);
    }

    #[test]
    fn nemo_action() {
      assert_eq!(
        plan(Launcher::Installed).nemo_action(),
        "[Nemo Action]
Name=Send to TeamCraft
Name[de]=An TeamCraft senden
Comment=Choose a conversation for the selected files
Comment[de]=Eine Unterhaltung für die ausgewählten Dateien wählen
Exec=teamcraft-desktop --share -- %F
Icon-Name=de.zeitgleich.teamcraft
Selection=NotNone
Extensions=any;
Quote=double
X-TeamCraft-Generated=true
"
      );
    }

    #[test]
    fn nautilus_and_caja_scripts_read_their_own_variable() {
      let plan = plan(Launcher::Installed);
      let nautilus = &plan.entries(FileManager::Nautilus)[0];
      assert_eq!(nautilus.file_name, "Send to TeamCraft");
      assert_eq!(nautilus.content, NAUTILUS_SCRIPT);
      assert!(nautilus.is_executable);
      let caja = &plan.entries(FileManager::Caja)[0];
      assert_eq!(caja.content, NAUTILUS_SCRIPT.replace("NAUTILUS", "CAJA"));
    }

    #[test]
    fn the_script_passes_each_selected_path_as_one_argument() {
      let directory = temporary_directory("script-run");
      let program = directory.join("print args");
      fs::write(
        &program,
        "#!/bin/sh\nfor a in \"$@\"; do printf '[%s]' \"$a\"; done\n",
      )
      .unwrap();
      fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
      let script = plan(Launcher::Path(program)).script(directory.clone(), "run", "SELECTED");
      let path = directory.join("run");
      fs::write(&path, script.content).unwrap();
      let output = std::process::Command::new("/bin/sh")
        .arg(&path)
        .env("SELECTED", "/a b/c.txt\n/it's \"q\"/$HOME.pdf\n")
        .output()
        .unwrap();
      assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "[--share][--][/a b/c.txt][/it's \"q\"/$HOME.pdf]"
      );
      let none = std::process::Command::new("/bin/sh")
        .arg(&path)
        .env("SELECTED", "")
        .output()
        .unwrap();
      assert!(
        none.status.success() && none.stdout.is_empty(),
        "nothing selected, nothing started"
      );
      fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn the_script_is_named_in_the_users_language() {
      let mut german = plan(Launcher::Installed);
      german.language = Language::De;
      assert_eq!(
        german.entries(FileManager::Nautilus)[0].file_name,
        "An TeamCraft senden"
      );
    }

    #[test]
    fn thunar_action() {
      assert_eq!(
        plan(Launcher::Installed).thunar_action(),
        "<action>
\t<!-- X-TeamCraft-Generated=true -->
\t<icon>de.zeitgleich.teamcraft</icon>
\t<name>Send to TeamCraft</name>
\t<unique-id>de.zeitgleich.teamcraft.send-to</unique-id>
\t<command>teamcraft-desktop --share -- %F</command>
\t<description>Choose a conversation for the selected files</description>
\t<patterns>*</patterns>
\t<audio-files/>
\t<image-files/>
\t<other-files/>
\t<text-files/>
\t<video-files/>
</action>"
      );
    }

    #[test]
    fn every_file_carries_the_marker() {
      let plan = plan(Launcher::Installed);
      for manager in FileManager::ALL {
        for entry in plan.entries(manager) {
          assert!(is_ours(&entry.content), "{manager:?}");
        }
      }
      assert!(plan.thunar_action().contains(MARKER));
    }
  }

  mod launchers {
    use super::*;

    #[test]
    fn the_flatpak_forwards_the_files_through_the_document_portal() {
      let plan = plan(Launcher::Flatpak);
      assert!(
        plan
          .service_menu()
          .contains("Exec=flatpak run --file-forwarding de.zeitgleich.teamcraft --share -- @@ %F @@\n")
      );
      assert!(
        plan.entries(FileManager::Caja)[0]
          .content
          .ends_with("exec flatpak run --file-forwarding de.zeitgleich.teamcraft --share -- @@ \"$@\" @@\n")
      );
      assert!(plan.thunar_action().contains(
        "<command>flatpak run --file-forwarding de.zeitgleich.teamcraft --share -- @@ %F @@</command>"
      ));
    }

    #[test]
    fn an_appimage_path_with_spaces_is_quoted_for_each_format() {
      let plan = plan(Launcher::Path(PathBuf::from(
        "/home/a b/Apps/Team'Craft.AppImage",
      )));
      assert!(
        plan
          .service_menu()
          .contains("Exec=\"/home/a b/Apps/Team'Craft.AppImage\" --share -- %F\n")
      );
      assert!(
        plan.entries(FileManager::Nautilus)[0]
          .content
          .ends_with("exec '/home/a b/Apps/Team'\\''Craft.AppImage' --share -- \"$@\"\n")
      );
      assert!(
        plan
          .thunar_action()
          .contains("<command>'/home/a b/Apps/Team'\\''Craft.AppImage' --share -- %F</command>")
      );
    }

    #[test]
    fn the_binary_on_the_path_is_started_by_name_any_other_by_its_file() {
      let directory = temporary_directory("launcher");
      let bin = directory.join("bin");
      fs::create_dir_all(&bin).unwrap();
      let installed = bin.join(INSTALLED_COMMAND);
      fs::write(&installed, "").unwrap();
      let other = directory.join("target-teamcraft-desktop");
      fs::write(&other, "").unwrap();
      let path = Some(bin.clone().into_os_string());
      assert_eq!(
        detect_launcher(None, Some(installed.clone()), false, path.clone()),
        Launcher::Installed
      );
      assert_eq!(
        detect_launcher(None, Some(other.clone()), false, path.clone()),
        Launcher::Path(other)
      );
      let appimage = PathBuf::from("/home/u/TeamCraft.AppImage");
      assert_eq!(
        detect_launcher(
          Some(appimage.clone()),
          Some(installed.clone()),
          false,
          path.clone()
        ),
        Launcher::Path(appimage)
      );
      assert_eq!(
        detect_launcher(None, Some(installed), true, path),
        Launcher::Flatpak
      );
      fs::remove_dir_all(directory).unwrap();
    }
  }

  mod environment {
    use super::*;

    #[test]
    fn the_language_follows_the_usual_variables() {
      let with = |pairs: &'static [(&str, &str)]| {
        Language::from_environment(|name| {
          pairs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| OsString::from(value))
        })
      };
      assert_eq!(with(&[("LANG", "de_DE.UTF-8")]), Language::De);
      assert_eq!(with(&[("LANG", "en_US.UTF-8")]), Language::En);
      assert_eq!(
        with(&[("LC_ALL", "en_US.UTF-8"), ("LANG", "de_DE.UTF-8")]),
        Language::En
      );
      assert_eq!(with(&[("LC_ALL", "C"), ("LANG", "de_DE.UTF-8")]), Language::De);
      assert_eq!(with(&[]), Language::En);
    }

    #[test]
    fn the_flatpak_writes_to_the_hosts_directories() {
      let variable = |name: &str| {
        Some(OsString::from(match name {
          "HOME" => "/home/u",
          "XDG_DATA_HOME" => "/home/u/.var/app/x/data",
          "XDG_CONFIG_HOME" => "/home/u/.var/app/x/config",
          "HOST_XDG_CONFIG_HOME" => "/home/u/cfg",
          _ => return None,
        }))
      };
      assert_eq!(
        home_directories(variable, true),
        (
          PathBuf::from("/home/u/.local/share"),
          PathBuf::from("/home/u/cfg")
        )
      );
      assert_eq!(
        home_directories(variable, false),
        (
          PathBuf::from("/home/u/.var/app/x/data"),
          PathBuf::from("/home/u/.var/app/x/config")
        )
      );
    }

    #[test]
    fn the_texts_come_from_the_fluent_files() {
      assert_eq!(Language::En.text("shell-send-to-name"), "Send to TeamCraft");
      assert_eq!(Language::De.text("shell-send-to-name"), "An TeamCraft senden");
    }
  }

  mod files {
    use super::*;

    #[test]
    fn turning_on_writes_for_the_installed_managers_only_and_off_removes_everything() {
      let (directory, plan) = sandbox("onoff", &[FileManager::Dolphin, FileManager::Nautilus]);
      sync(&plan, true).unwrap();
      let service_menu = directory.join("data/kio/servicemenus/de.zeitgleich.teamcraft.send-to.desktop");
      assert_eq!(fs::read_to_string(&service_menu).unwrap(), SERVICE_MENU);
      assert_eq!(
        fs::metadata(&service_menu).unwrap().permissions().mode() & 0o777,
        0o755
      );
      let script = directory.join("data/nautilus/scripts/Send to TeamCraft");
      assert_eq!(fs::read_to_string(&script).unwrap(), NAUTILUS_SCRIPT);
      assert_eq!(fs::metadata(&script).unwrap().permissions().mode() & 0o777, 0o755);
      assert!(
        !directory.join("data/nemo").exists(),
        "no directory for a manager that isn't there"
      );
      assert!(!directory.join("config").exists());

      sync(&plan, false).unwrap();
      assert!(!service_menu.exists());
      assert!(!script.exists());
      sync(&plan, false).unwrap();
      fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn writing_again_changes_nothing_and_an_outdated_path_is_repaired() {
      let (directory, mut plan) = sandbox("repair", &[FileManager::Nemo]);
      sync(&plan, true).unwrap();
      let path = directory.join("data/nemo/actions/de.zeitgleich.teamcraft.send-to.nemo_action");
      let modified = fs::metadata(&path).unwrap().modified().unwrap();
      sync(&plan, true).unwrap();
      assert_eq!(
        fs::metadata(&path).unwrap().modified().unwrap(),
        modified,
        "not rewritten"
      );
      plan.launcher = Launcher::Path(PathBuf::from("/opt/moved/TeamCraft.AppImage"));
      sync(&plan, true).unwrap();
      assert!(
        fs::read_to_string(&path)
          .unwrap()
          .contains("Exec=/opt/moved/TeamCraft.AppImage --share -- %F")
      );
      assert!(
        !directory
          .join("data/nemo/actions/.de.zeitgleich.teamcraft.send-to.nemo_action.tmp")
          .exists()
      );
      fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_script_renamed_by_the_language_leaves_no_second_entry() {
      let (directory, mut plan) = sandbox("rename", &[FileManager::Nautilus]);
      sync(&plan, true).unwrap();
      plan.language = Language::De;
      sync(&plan, true).unwrap();
      let names: Vec<String> = fs::read_dir(directory.join("data/nautilus/scripts"))
        .unwrap()
        .map(|item| item.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
      assert_eq!(names, ["An TeamCraft senden"]);
      fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn files_that_are_not_ours_are_never_changed_or_removed() {
      let (directory, plan) = sandbox("foreign", &[FileManager::Dolphin, FileManager::Nautilus]);
      let menus = directory.join("data/kio/servicemenus");
      let scripts = directory.join("data/nautilus/scripts");
      fs::create_dir_all(&menus).unwrap();
      fs::create_dir_all(&scripts).unwrap();
      // The same name as ours, and a user's script that even mentions the marker's key.
      let same_name = menus.join("de.zeitgleich.teamcraft.send-to.desktop");
      fs::write(&same_name, "[Desktop Entry]\nName=Mine\n").unwrap();
      let users = scripts.join("Send to TeamCraft");
      fs::write(&users, "#!/bin/sh\necho mine\n").unwrap();
      let other = scripts.join("Other");
      fs::write(
        &other,
        "#!/bin/sh\n# see X-TeamCraft-Generated=true in the docs\n",
      )
      .unwrap();
      fs::create_dir(scripts.join("sub")).unwrap();

      sync(&plan, true).unwrap();
      sync(&plan, false).unwrap();
      assert_eq!(
        fs::read_to_string(&same_name).unwrap(),
        "[Desktop Entry]\nName=Mine\n"
      );
      assert_eq!(fs::read_to_string(&users).unwrap(), "#!/bin/sh\necho mine\n");
      assert!(other.exists());
      assert!(scripts.join("sub").is_dir());
      fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_failing_directory_does_not_stop_the_others() {
      let (directory, plan) = sandbox("failing", &[FileManager::Dolphin, FileManager::Nemo]);
      // A file where Dolphin's directory should be.
      fs::create_dir_all(directory.join("data/kio")).unwrap();
      fs::write(directory.join("data/kio/servicemenus"), "").unwrap();
      let error = sync(&plan, true).unwrap_err();
      assert!(error.0.contains("Dolphin"), "{}", error.0);
      assert!(
        directory
          .join("data/nemo/actions/de.zeitgleich.teamcraft.send-to.nemo_action")
          .exists()
      );
      fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn the_choice_is_remembered_and_on_by_default() {
      let directory = temporary_directory("state");
      let path = directory.join("config").join(STATE_FILE);
      assert!(is_on(&path));
      store(&path, false).unwrap();
      assert!(!is_on(&path));
      store(&path, true).unwrap();
      store(&path, true).unwrap();
      assert!(is_on(&path));
      fs::remove_dir_all(directory).unwrap();
    }
  }

  mod thunar {
    use super::*;

    const USERS: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<actions>
<!-- mine -->
<action>
\t<icon>utilities-terminal</icon>
\t<name>Open Terminal Here</name>
\t<unique-id>1-1</unique-id>
\t<command>exo-open %f</command>
\t<patterns>*</patterns>
\t<directories/>
</action>
</actions>
";

    #[test]
    fn a_missing_file_is_created_and_removing_from_it_is_nothing() {
      let action = plan(Launcher::Installed).thunar_action();
      let created = merge_uca(None, Some(&action)).unwrap().unwrap();
      assert!(created.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<actions>\n<action>"));
      assert!(roxmltree::Document::parse(&created).is_ok());
      assert_eq!(merge_uca(None, None).unwrap(), None);
    }

    #[test]
    fn the_users_actions_stay_byte_for_byte_when_ours_comes_and_goes() {
      let action = plan(Launcher::Installed).thunar_action();
      let added = merge_uca(Some(USERS), Some(&action)).unwrap().unwrap();
      assert!(added.starts_with(&USERS[..USERS.len() - "</actions>\n".len()]));
      assert!(roxmltree::Document::parse(&added).is_ok());
      assert_eq!(
        merge_uca(Some(&added), Some(&action)).unwrap(),
        None,
        "idempotent"
      );
      let removed = merge_uca(Some(&added), None).unwrap().unwrap();
      assert_eq!(removed, USERS);
    }

    #[test]
    fn a_changed_action_is_replaced_in_place() {
      let installed = plan(Launcher::Installed).thunar_action();
      let moved = plan(Launcher::Path(PathBuf::from("/opt/t"))).thunar_action();
      let added = merge_uca(Some(USERS), Some(&installed)).unwrap().unwrap();
      let replaced = merge_uca(Some(&added), Some(&moved)).unwrap().unwrap();
      assert_eq!(replaced.matches("<unique-id>de.zeitgleich").count(), 1);
      assert!(replaced.contains("<command>/opt/t --share -- %F</command>"));
    }

    #[test]
    fn a_file_that_cannot_be_merged_is_not_touched() {
      let action = plan(Launcher::Installed).thunar_action();
      assert!(merge_uca(Some("<actions><action>"), Some(&action)).is_err());
      assert!(merge_uca(Some("<other/>"), Some(&action)).is_err());
      let empty = merge_uca(Some("<actions/>"), Some(&action)).unwrap().unwrap();
      assert!(roxmltree::Document::parse(&empty).is_ok());
    }

    #[test]
    fn sync_merges_into_an_existing_file_and_off_restores_it() {
      let (directory, plan) = sandbox("thunar", &[FileManager::Thunar]);
      let path = directory.join("config/Thunar/uca.xml");
      fs::create_dir_all(path.parent().unwrap()).unwrap();
      fs::write(&path, USERS).unwrap();
      sync(&plan, true).unwrap();
      assert!(fs::read_to_string(&path).unwrap().contains(THUNAR_ACTION_ID));
      sync(&plan, false).unwrap();
      assert_eq!(fs::read_to_string(&path).unwrap(), USERS);
      fs::remove_dir_all(directory).unwrap();
    }
  }

  fn plan(launcher: Launcher) -> Plan {
    Plan {
      data_home: PathBuf::from("/data"),
      config_home: PathBuf::from("/config"),
      launcher,
      language: Language::En,
      installed: FileManager::ALL.to_vec(),
    }
  }

  fn temporary_directory(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("teamcraft-send-to-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).unwrap();
    directory
  }

  /// A plan writing below a fresh directory, for the given managers.
  fn sandbox(name: &str, installed: &[FileManager]) -> (PathBuf, Plan) {
    let directory = temporary_directory(name);
    let mut plan = plan(Launcher::Installed);
    plan.data_home = directory.join("data");
    plan.config_home = directory.join("config");
    plan.installed = installed.to_vec();
    (directory, plan)
  }
}
