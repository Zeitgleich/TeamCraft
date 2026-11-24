//! Synthetic Slack and Mattermost exports, converted and read back as the import reads them:
//! channels, DMs, group DMs, threads, files (present, missing and hostile), reactions, edits,
//! bots, system messages and the mapping templates.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use teamcraft_import::export::{CHANNEL_DEFAULT, CHANNEL_DM, CHANNEL_GROUP, Export, POST_FILE};
use teamcraft_import::mapping::Mapping;
use teamcraft_import::plan::{Target, plan};
use teamcraft_import::sources::{ConvertError, Skipped, SourceKind, convert, slack};
use teamcraft_proto::UserId;

mod slack_export {
  use super::*;

  #[test]
  fn converts_channels_threads_files_and_reactions() {
    let dir = scratch("slack");
    let source = dir.join("export");
    write_slack(&source);
    let out = dir.join("converted");
    let summary = convert(SourceKind::Slack, &source, &out).unwrap();
    assert_eq!(
      summary.channels, 4,
      "general, the one without a safe folder, private, the group DM"
    );
    assert_eq!(summary.direct, 1);
    assert_eq!(summary.files, 2);
    assert_eq!(summary.missing_files, 1);
    assert_eq!(summary.skipped[&Skipped::SystemMessage], 1);
    assert_eq!(summary.skipped[&Skipped::Deleted], 1);
    assert_eq!(summary.skipped[&Skipped::UnsafePath], 1);
    assert_eq!(summary.skipped[&Skipped::ExternalFile], 1);
    assert_eq!(summary.skipped[&Skipped::NoFolder], 1);

    let export = Export::open(&out, None).unwrap();
    let general = channel_named(&export, "general");
    assert_eq!(general.kind, CHANNEL_DEFAULT);
    assert_eq!(general.purpose.as_deref(), Some("Company-wide"));
    assert_eq!(general.members().len(), 3);
    let posts = export.posts(general.id).unwrap();
    let texts: Vec<&str> = posts
      .iter()
      .map(|post| post.message.as_deref().unwrap())
      .collect();
    let anna = user_id(&export, "anna");
    assert_eq!(
      texts,
      [
        format!(
          "Hello @u:{} **all** <https://example.com/a?b=1&c=2|docs>",
          user_id(&export, "ben")
        )
        .as_str(),
        "a reply",
        "two files",
        "",
        "Build 7 failed",
      ]
    );
    let root = &posts[0];
    assert_eq!(root.user_id, anna);
    assert_eq!(root.maybe_root(), None);
    assert_eq!(posts[1].maybe_root(), Some(root.id));
    assert_eq!(posts[1].maybe_edited_at(), Some(1_700_000_100_000));
    assert_eq!(export.reactions[&root.id].len(), 2);
    assert!(
      export.reactions[&root.id]
        .iter()
        .all(|reaction| reaction.name == "thumbsup")
    );
    // Two files: one on the message, the second a message of its own a millisecond later.
    assert_eq!(posts[2].kind, POST_FILE);
    assert_eq!(posts[3].kind, POST_FILE);
    assert_eq!(posts[3].create_at, posts[2].create_at + 1);
    assert_eq!(
      export.upload_path(&posts[2]),
      source
        .join(slack::UPLOADS_DIR)
        .join("F1")
        .join("plan.pdf")
        .canonicalize()
        .unwrap()
    );
    assert!(posts[2].file_props.as_ref().unwrap().name.as_deref() == Some("plan.pdf"));
    assert!(export.bots.values().any(|name| name == "CI"));

    let dm = export
      .channels
      .values()
      .find(|row| row.kind == CHANNEL_DM)
      .unwrap();
    assert_eq!(export.posts(dm.id).unwrap().len(), 1);
    let group = channel_named(&export, "Anna A, Ben B, Carl C");
    assert_eq!(group.kind, CHANNEL_GROUP);

    // Converting again gives the same IDs, so a resumed import agrees with the first.
    let again = dir.join("again");
    convert(SourceKind::Slack, &source, &again).unwrap();
    let second = Export::open(&again, None).unwrap();
    assert_eq!(
      export.channels.keys().collect::<Vec<_>>(),
      second.channels.keys().collect::<Vec<_>>()
    );
    assert_eq!(
      posts.iter().map(|post| post.id).collect::<Vec<_>>(),
      second
        .posts(general.id)
        .unwrap()
        .iter()
        .map(|post| post.id)
        .collect::<Vec<_>>()
    );
    drop(std::fs::remove_dir_all(&dir));
  }

  #[test]
  fn templates_drive_the_plan() {
    let dir = scratch("slack-plan");
    let source = dir.join("export");
    write_slack(&source);
    let out = dir.join("converted");
    convert(SourceKind::Slack, &source, &out).unwrap();
    let export = Export::open(&out, None).unwrap();
    let channels = std::fs::read_to_string(out.join("channels.csv"))
      .unwrap()
      .replace(",public,,", ",public,Space A,")
      .replace(",private,,", ",private,Space A,");
    std::fs::write(out.join("channels.csv"), channels).unwrap();
    let users = std::fs::read_to_string(out.join("users.csv")).unwrap();
    let users: String = users
      .lines()
      .map(|line| match line.split(',').nth(1) {
        Some("U1") => format!("{line}anna\n"),
        Some("U2") => format!("{line}ben\n"),
        _ => format!("{line}\n"),
      })
      .collect();
    std::fs::write(out.join("users.csv"), users).unwrap();
    let mapping = Mapping::read(&out.join("channels.csv"), &out.join("users.csv")).unwrap();
    assert_eq!(mapping.users.len(), 2);
    let accounts: HashMap<_, _> = mapping
      .users
      .keys()
      .zip(1_u64..)
      .map(|(source, n)| (*source, n.to_string().parse::<UserId>().unwrap()))
      .collect();
    let planned = plan(&export, &mapping, &accounts);
    assert!(planned.items.iter().any(|item| matches!(
      &item.target,
      Target::NewChannel { space, name, is_public: true, .. } if space == "Space A" && name == "general"
    )));
    assert!(
      planned
        .items
        .iter()
        .any(|item| matches!(item.target, Target::Dm { .. }))
    );
    drop(std::fs::remove_dir_all(&dir));
  }

  #[test]
  fn hostile_exports_are_refused_or_contained() {
    let dir = scratch("slack-hostile");
    let source = dir.join("export");
    write_slack(&source);
    std::fs::write(source.join("general").join("2024-01-03.json"), "[{\"ts\": {}}]").unwrap();
    let Err(ConvertError::Json { path, .. }) = convert(SourceKind::Slack, &source, &dir.join("out")) else {
      panic!("a broken day file is an error naming it");
    };
    assert!(path.ends_with("2024-01-03.json"));

    let missing = dir.join("missing");
    std::fs::create_dir_all(&missing).unwrap();
    assert!(matches!(
      convert(SourceKind::Slack, &missing, &dir.join("out2")),
      Err(ConvertError::Io { .. })
    ));
    drop(std::fs::remove_dir_all(&dir));
  }
}

mod mattermost_export {
  use super::*;

  #[test]
  fn converts_teams_channels_dms_threads_and_files() {
    let dir = scratch("mattermost");
    let source = dir.join("export");
    write_mattermost(&source);
    let out = dir.join("converted");
    let summary = convert(SourceKind::Mattermost, &source.join("import.jsonl"), &out).unwrap();
    assert_eq!(
      summary.channels, 3,
      "town square, a private channel and the group DM"
    );
    assert_eq!(summary.direct, 1);
    assert_eq!(summary.files, 1);
    assert_eq!(summary.missing_files, 0);
    assert_eq!(summary.skipped[&Skipped::UnsafePath], 1);
    assert_eq!(summary.skipped[&Skipped::UnknownChannel], 1);
    assert_eq!(summary.skipped[&Skipped::Deleted], 1);
    assert_eq!(summary.skipped[&Skipped::UnknownLine], 1);

    let export = Export::open(&out, None).unwrap();
    let town = channel_named(&export, "town-square");
    assert_eq!(town.kind, CHANNEL_DEFAULT);
    assert_eq!(town.members().len(), 2);
    let private = channel_named(&export, "secret");
    assert_eq!(private.kind, CHANNEL_GROUP);
    let posts = export.posts(town.id).unwrap();
    let anna = user_id(&export, "anna");
    let ben = user_id(&export, "ben");
    assert_eq!(
      posts[0].message.as_deref(),
      Some(format!("hi @u:{ben}, see `@anna`").as_str())
    );
    assert_eq!(posts[0].user_id, anna);
    assert_eq!(posts[1].maybe_root(), Some(posts[0].id));
    assert_eq!(posts[1].user_id, ben);
    assert_eq!(posts[1].maybe_edited_at(), Some(1_700_000_090_000));
    assert_eq!(export.reactions[&posts[0].id][0].name, "+1");
    assert_eq!(posts[2].kind, POST_FILE);
    assert_eq!(
      posts[2].file_props.as_ref().unwrap().name.as_deref(),
      Some("photo.png")
    );
    assert!(posts[2].file_props.as_ref().unwrap().is_image);
    assert!(export.bots.values().any(|name| name == "Deploy bot"));
    assert_eq!(posts.len(), 5, "the post with only a hostile file keeps its text");

    let dm = export
      .channels
      .values()
      .find(|row| row.kind == CHANNEL_DM)
      .unwrap();
    assert_eq!(export.posts(dm.id).unwrap().len(), 2);
    assert!(export.favorites.contains(&(town.id, anna)));
    // Somebody who only wrote, without a line of their own, is a deleted person to map.
    assert!(
      export
        .users
        .values()
        .any(|user| user.username.as_deref() == Some("gone") && user.deleted)
    );
    let users = std::fs::read_to_string(out.join("users.csv")).unwrap();
    assert!(users.contains("anna@example.com"));
    let channels = std::fs::read_to_string(out.join("channels.csv")).unwrap();
    assert!(channels.contains("Team A"));
    drop(std::fs::remove_dir_all(&dir));
  }

  #[test]
  fn a_broken_line_names_its_number() {
    let dir = scratch("mattermost-broken");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
      dir.join("import.jsonl"),
      "{\"type\":\"version\",\"version\":1}\n\n{\"type\":\"post\",\"post\":\"x\"}\n",
    )
    .unwrap();
    let Err(ConvertError::Json { maybe_line, .. }) = convert(
      SourceKind::Mattermost,
      &dir.join("import.jsonl"),
      &dir.join("out"),
    ) else {
      panic!("expected a JSON error");
    };
    assert_eq!(maybe_line, Some(3));
    drop(std::fs::remove_dir_all(&dir));
  }
}

fn write_slack(source: &Path) {
  std::fs::create_dir_all(source.join("general")).unwrap();
  std::fs::create_dir_all(source.join("private")).unwrap();
  std::fs::create_dir_all(source.join("D1")).unwrap();
  std::fs::create_dir_all(source.join("mpdm-anna--ben--carl-1")).unwrap();
  std::fs::create_dir_all(source.join(slack::UPLOADS_DIR).join("F1")).unwrap();
  std::fs::write(source.join(slack::UPLOADS_DIR).join("F1").join("plan.pdf"), "pdf").unwrap();
  std::fs::write(
    source.join("users.json"),
    r#"[
      {"id":"U1","name":"anna","real_name":"Anna A","profile":{"email":"anna@example.com","display_name":"Anna A"}},
      {"id":"U2","name":"ben","profile":{"real_name":"Ben B","email":"ben@example.com"}},
      {"id":"U3","name":"carl","deleted":true,"profile":{"real_name":"Carl C"}},
      {"id":"B9","name":"helper","is_bot":true,"profile":{"real_name":"Helper"}}
    ]"#,
  )
  .unwrap();
  std::fs::write(
    source.join("channels.json"),
    r#"[
      {"id":"C1","name":"general","created":1700000000,"creator":"U1","members":["U1","U2","U3"],"purpose":{"value":"Company-wide"}},
      {"id":"C2","name":"../escape","created":1700000000,"members":["U1"]}
    ]"#,
  )
  .unwrap();
  std::fs::write(
    source.join("groups.json"),
    r#"[{"id":"G1","name":"private","created":1700000000,"is_archived":true,"members":["U1","U2"]}]"#,
  )
  .unwrap();
  std::fs::write(
    source.join("mpims.json"),
    r#"[{"id":"G2","name":"mpdm-anna--ben--carl-1","created":1700000000,"members":["U1","U2","U3"]}]"#,
  )
  .unwrap();
  std::fs::write(
    source.join("dms.json"),
    r#"[{"id":"D1","created":1700000000,"members":["U1","U2"]}]"#,
  )
  .unwrap();
  std::fs::write(
    source.join("general").join("2024-01-01.json"),
    r#"[
      {"type":"message","subtype":"channel_join","user":"U2","text":"<@U2> has joined the channel","ts":"1699999999.000100"},
      {"type":"message","user":"U1","text":"Hello <@U2> *all* <https://example.com/a?b=1&amp;c=2|docs>","ts":"1700000000.000100","thread_ts":"1700000000.000100",
       "reactions":[{"name":"thumbsup","users":["U2"],"count":1},{"name":"thumbsup::skin-tone-2","users":["U3"],"count":1}]},
      {"type":"message","user":"U2","text":"a reply","ts":"1700000001.000200","thread_ts":"1700000000.000100","edited":{"user":"U2","ts":"1700000100.000000"}},
      {"type":"message","subtype":"tombstone","user":"U1","text":"This message was deleted.","ts":"1700000002.000000"}
    ]"#,
  )
  .unwrap();
  std::fs::write(
    source.join("general").join("2024-01-02.json"),
    r#"[
      {"type":"message","user":"U1","text":"two files","ts":"1700086400.000000","files":[
        {"id":"F1","name":"plan.pdf","mimetype":"application/pdf","filetype":"pdf","size":3},
        {"id":"F2","name":"photo.png","mimetype":"image/png","filetype":"png","size":9},
        {"id":"F3","name":"../../etc/passwd"},
        {"id":"F4","name":"linked.doc","is_external":true}
      ]},
      {"type":"message","subtype":"bot_message","bot_id":"B1","username":"CI","text":"","ts":"1700086500.000000",
       "attachments":[{"fallback":"Build 7 failed"}]}
    ]"#,
  )
  .unwrap();
  std::fs::write(
    source.join("D1").join("2024-01-01.json"),
    r#"[{"type":"message","user":"U2","text":"just us","ts":"1700000000.000000"}]"#,
  )
  .unwrap();
  std::fs::write(source.join("private").join("notes.txt"), "not a day").unwrap();
}

fn write_mattermost(source: &Path) {
  std::fs::create_dir_all(source.join("data").join("2024")).unwrap();
  std::fs::write(source.join("data").join("2024").join("photo.png"), "png").unwrap();
  let lines = [
    r#"{"type":"version","version":1,"info":{"generator":"mmctl"}}"#,
    r#"{"type":"team","team":{"name":"team-a","display_name":"Team A","type":"O"}}"#,
    r#"{"type":"channel","channel":{"team":"team-a","name":"town-square","display_name":"Town Square","type":"O","purpose":"Everyone"}}"#,
    r#"{"type":"channel","channel":{"team":"team-a","name":"secret","type":"P"}}"#,
    r#"{"type":"user","user":{"username":"anna","email":"anna@example.com","first_name":"Anna","last_name":"A","teams":[{"name":"team-a","channels":[{"name":"town-square","favorite":true},{"name":"secret"}]}]}}"#,
    r#"{"type":"user","user":{"username":"ben","email":"ben@example.com","teams":[{"name":"team-a","channels":[{"name":"town-square"}]}]}}"#,
    r#"{"type":"user","user":{"username":"carl","delete_at":5}}"#,
    r#"{"type":"post","post":{"team":"team-a","channel":"town-square","user":"anna","message":"hi @ben, see `@anna`","create_at":1700000000000,
      "reactions":[{"user":"ben","emoji_name":"+1","create_at":1700000001000}],
      "replies":[{"user":"ben","message":"thanks","create_at":1700000060000,"edit_at":1700000090000}]}}"#,
    r#"{"type":"post","post":{"team":"team-a","channel":"town-square","user":"ben","message":"","create_at":1700000200000,"attachments":[{"path":"data/2024/photo.png"}]}}"#,
    r#"{"type":"post","post":{"team":"team-a","channel":"town-square","user":"ben","message":"sneaky","create_at":1700000300000,"attachments":[{"path":"../../etc/passwd"}]}}"#,
    r#"{"type":"post","post":{"team":"team-a","channel":"town-square","user":"deploy","message":"","create_at":1700000400000,"props":{"from_webhook":"true","override_username":"Deploy bot","attachments":[{"fallback":"Deployed"}]}}}"#,
    r#"{"type":"post","post":{"team":"team-a","channel":"gone-channel","user":"anna","message":"lost","create_at":1700000500000}}"#,
    r#"{"type":"post","post":{"team":"team-a","channel":"secret","user":"anna","message":"removed","create_at":1700000600000,"delete_at":1700000700000}}"#,
    r#"{"type":"direct_channel","direct_channel":{"members":["anna","ben"],"favorited_by":["ben"]}}"#,
    r#"{"type":"direct_post","direct_post":{"channel_members":["ben","anna"],"user":"ben","message":"just us @anna.","create_at":1700000000000}}"#,
    r#"{"type":"direct_post","direct_post":{"channel_members":["ben","anna"],"user":"ben","message":"same millisecond","create_at":1700000000000}}"#,
    r#"{"type":"direct_post","direct_post":{"channel_members":["anna","ben","gone"],"user":"gone","message":"three of us","create_at":1700000000000}}"#,
    r#"{"type":"emoji","emoji":{"name":"party","image":"data/emoji/party.png"}}"#,
  ];
  // One object per line, as the export writes them (the literals above wrap for reading).
  let lines: Vec<String> = lines.iter().map(|line| line.replace('\n', "")).collect();
  std::fs::write(source.join("import.jsonl"), lines.join("\n")).unwrap();
}

fn channel_named<'a>(export: &'a Export, name: &str) -> &'a teamcraft_import::export::Channel {
  export
    .channels
    .values()
    .find(|channel| channel.name.as_deref() == Some(name))
    .unwrap_or_else(|| panic!("no channel {name}"))
}

fn user_id(export: &Export, username: &str) -> i64 {
  export
    .users
    .values()
    .find(|user| user.username.as_deref() == Some(username))
    .unwrap()
    .id
}

fn scratch(name: &str) -> PathBuf {
  let dir = std::env::temp_dir().join(format!("teamcraft-import-convert-{name}-{}", std::process::id()));
  drop(std::fs::remove_dir_all(&dir));
  std::fs::create_dir_all(&dir).unwrap();
  dir
}
