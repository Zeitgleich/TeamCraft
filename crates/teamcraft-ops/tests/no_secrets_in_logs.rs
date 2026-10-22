//! Static check (security.md: "Never log plaintext messages, keys, tokens, passwords"): no
//! `tracing` macro in the production code of any crate names a secret-carrying value. String
//! literals are ignored (the message text may say "token"), the arguments are not.
//!
//! The runtime counterpart, which captures the logs of a server handling real credentials, is in
//! `teamcraft-server/tests/log_hygiene.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

/// Identifiers that carry or hold secrets or plaintext.
const FORBIDDEN: &[&str] = &[
  "token",
  "tokens",
  "access_token",
  "bot_token",
  "secret",
  "client_secret",
  "password",
  "passphrase",
  "private_key",
  "ciphertext",
  "plaintext",
  "authorization",
  "credentials",
  "push_key",
  "content_key",
  "key_bag",
  "invite_token",
  "guest_token",
  "signing_key",
];

/// Reviewed uses: (file suffix, identifier, reason). None so far.
const ALLOWED: &[(&str, &str, &str)] = &[];

#[test]
fn tracing_macros_never_mention_secret_values() {
  let root = Path::new(env!("CARGO_MANIFEST_DIR"))
    .join("../..")
    .canonicalize()
    .unwrap();
  let mut files = Vec::new();
  for krate in std::fs::read_dir(root.join("crates")).unwrap() {
    let src = krate.unwrap().path().join("src");
    if src.is_dir() {
      collect(&src, &mut files);
    }
  }
  assert!(files.len() > 100, "found only {} source files", files.len());

  let mut findings = Vec::new();
  let mut macros_seen = 0;
  for file in &files {
    let text = std::fs::read_to_string(file).unwrap();
    // Tests may log what they like.
    let production = text.split("#[cfg(test)]").next().unwrap();
    for (line, call) in tracing_calls(production) {
      macros_seen += 1;
      let code = strip_strings(&call);
      for word in identifiers(&code) {
        let relative = file.strip_prefix(&root).unwrap().display().to_string();
        let is_allowed = ALLOWED
          .iter()
          .any(|(suffix, allowed, _)| relative.ends_with(suffix) && *allowed == word);
        if FORBIDDEN.contains(&word.as_str()) && !is_allowed {
          findings.push(format!(
            "{relative}:{line}: `{word}` in {}",
            call.lines().next().unwrap_or("")
          ));
        }
      }
    }
  }
  assert!(
    macros_seen > 50,
    "found only {macros_seen} tracing calls; the scan is broken"
  );
  assert!(
    findings.is_empty(),
    "secret-like values in log calls:\n{}",
    findings.join("\n")
  );
}

#[test]
fn the_scan_finds_secrets_in_arguments_but_not_in_message_text() {
  let code = r#"fn f() { warn!(token = %bot_token, "the token was rejected"); info!("password reset"); error!(%error, "x"); }"#;
  let calls = tracing_calls(code);
  assert_eq!(calls.len(), 3);
  let flagged: Vec<bool> = calls
    .iter()
    .map(|(_, call)| {
      identifiers(&strip_strings(call))
        .iter()
        .any(|word| FORBIDDEN.contains(&word.as_str()))
    })
    .collect();
  assert_eq!(flagged.iter().filter(|flagged| **flagged).count(), 1);
  let (_, call) = &calls[flagged.iter().position(|flagged| *flagged).unwrap()];
  assert!(call.contains("bot_token"));
}

fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
  for entry in std::fs::read_dir(dir).unwrap() {
    let path = entry.unwrap().path();
    if path.is_dir() {
      collect(&path, files);
    } else if path.extension().is_some_and(|extension| extension == "rs") {
      files.push(path);
    }
  }
}

/// (line number, text) of every `trace!`/`debug!`/`info!`/`warn!`/`error!` invocation.
fn tracing_calls(text: &str) -> Vec<(usize, String)> {
  let mut calls = Vec::new();
  for name in ["trace!(", "debug!(", "info!(", "warn!(", "error!("] {
    let mut from = 0;
    while let Some(found) = text[from..].find(name) {
      let start = from + found;
      from = start + name.len();
      let before = text[..start].chars().next_back();
      if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
        continue;
      }
      let mut depth = 1;
      let mut end = from;
      let mut in_string = false;
      let mut previous = ' ';
      for (offset, c) in text[from..].char_indices() {
        match c {
          '"' if previous != '\\' => in_string = !in_string,
          '(' if !in_string => depth += 1,
          ')' if !in_string => {
            depth -= 1;
            if depth == 0 {
              end = from + offset;
              break;
            }
          },
          _ => {},
        }
        previous = c;
      }
      let line = text[..start].matches('\n').count() + 1;
      calls.push((line, text[start..end].to_owned()));
    }
  }
  calls
}

/// Replaces the contents of string literals, so message text is not scanned.
fn strip_strings(code: &str) -> String {
  let mut out = String::new();
  let mut in_string = false;
  let mut previous = ' ';
  for c in code.chars() {
    if c == '"' && previous != '\\' {
      in_string = !in_string;
      out.push('"');
    } else if !in_string {
      out.push(c);
    }
    previous = c;
  }
  out
}

fn identifiers(code: &str) -> Vec<String> {
  code
    .split(|c: char| !(c.is_alphanumeric() || c == '_'))
    .filter(|word| !word.is_empty())
    .map(str::to_owned)
    .collect()
}
