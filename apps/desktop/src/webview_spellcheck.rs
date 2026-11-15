//! Spell checking in the composer on Linux (#2064). The composer sets `spellcheck="true"`, but
//! WebKitGTK has spell checking switched off in its web context until the app switches it on
//! (`webkit_web_context_set_spell_checking_enabled`), and it checks nothing without languages.
//! macOS and Windows webviews check with the system's own languages and need nothing from the shell.
//!
//! The languages are the user's choice in Settings (`ShellPrefs::spell_check_language`), or the
//! system's: `LANGUAGE`, then `LC_ALL`, `LC_MESSAGES`, `LANG`. WebKit uses the dictionaries that
//! enchant finds (hunspell, aspell, …); a language without a dictionary installed checks nothing.

/// The language WebKit checks in when the system names none.
const FALLBACK_LANGUAGE: &str = "en_US";

/// Environment variables that name the system's languages, in the order of the C library.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const LOCALE_VARIABLES: [&str; 4] = ["LANGUAGE", "LC_ALL", "LC_MESSAGES", "LANG"];

/// The languages to check in: the user's `chosen` one (a locale such as `de_DE`; empty: the
/// system's) or those of the environment, `lookup` being how a variable is read.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn languages(chosen: &str, lookup: impl Fn(&str) -> Option<String>) -> Vec<String> {
  let chosen = chosen.trim();
  if !chosen.is_empty() {
    return vec![chosen.to_owned()];
  }
  let from_system = LOCALE_VARIABLES
    .iter()
    .filter_map(|name| lookup(name))
    .find(|value| !value.trim().is_empty())
    .map(|value| {
      let mut found: Vec<String> = Vec::new();
      for part in value.split(':').filter_map(locale_language) {
        if !found.contains(&part) {
          found.push(part);
        }
      }
      found
    })
    .unwrap_or_default();
  if from_system.is_empty() {
    vec![FALLBACK_LANGUAGE.to_owned()]
  } else {
    from_system
  }
}

/// `de_DE.UTF-8@euro` as `de_DE`; `None` for the "no language" locales (`C`, `POSIX`).
fn locale_language(locale: &str) -> Option<String> {
  let name = locale.split(['.', '@']).next().unwrap_or_default().trim();
  match name {
    "" | "C" | "POSIX" => None,
    other => Some(other.replace('-', "_")),
  }
}

/// Switches spell checking on in `webview`'s web context, in the chosen or the system's languages.
#[cfg(target_os = "linux")]
pub fn configure(webview: &webkit2gtk::WebView, chosen: &str) {
  use webkit2gtk::{WebContextExt, WebViewExt};

  let Some(context) = webview.context() else {
    return;
  };
  let languages = languages(chosen, |name| std::env::var(name).ok());
  let names: Vec<&str> = languages.iter().map(String::as_str).collect();
  context.set_spell_checking_enabled(true);
  context.set_spell_checking_languages(&names);
  tracing::info!(languages = ?names, is_enabled = context.is_spell_checking_enabled(), "spell checking");
}

#[cfg(test)]
mod tests {
  use super::*;

  fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |name| {
      pairs
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, value)| (*value).to_owned())
    }
  }

  #[test]
  fn the_chosen_language_wins_over_the_system() {
    assert_eq!(languages("fr_FR", env(&[("LANG", "de_DE.UTF-8")])), ["fr_FR"]);
  }

  #[test]
  fn the_system_languages_come_from_the_first_variable_that_names_one() {
    assert_eq!(languages("", env(&[("LANG", "de_DE.UTF-8")])), ["de_DE"]);
    assert_eq!(
      languages(
        "",
        env(&[("LANGUAGE", "de_AT:en_GB:de_AT"), ("LANG", "fr_FR.UTF-8")])
      ),
      ["de_AT", "en_GB"]
    );
    assert_eq!(
      languages("", env(&[("LC_ALL", ""), ("LANG", "sv_SE@euro")])),
      ["sv_SE"]
    );
    assert_eq!(languages("", env(&[("LANG", "de-DE")])), ["de_DE"]);
  }

  #[test]
  fn no_language_falls_back_to_english() {
    assert_eq!(languages("  ", env(&[])), ["en_US"]);
    assert_eq!(
      languages("", env(&[("LANG", "C"), ("LC_ALL", "POSIX")])),
      ["en_US"]
    );
  }
}
