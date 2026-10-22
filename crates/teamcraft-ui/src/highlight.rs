//! Syntax highlighting for fenced code blocks: a small tokenizer for the languages people paste
//! into chat. It only classifies text (keyword, string, comment, number); the UI turns the spans
//! into elements, so no markup is ever built from message text.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
  Plain,
  Keyword,
  Str,
  Comment,
  Number,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
  pub kind: TokenKind,
  pub text: String,
}

struct Syntax {
  keywords: &'static [&'static str],
  /// Line comment introducers.
  line_comments: &'static [&'static str],
  has_block_comments: bool,
}

const RUST: Syntax = Syntax {
  keywords: &[
    "as", "async", "await", "break", "const", "continue", "crate", "else", "enum", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "self", "Self",
    "static", "struct", "trait", "true", "type", "unsafe", "use", "where", "while",
  ],
  line_comments: &["//"],
  has_block_comments: true,
};
const JAVASCRIPT: Syntax = Syntax {
  keywords: &[
    "async",
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "default",
    "else",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "from",
    "function",
    "if",
    "import",
    "in",
    "interface",
    "let",
    "new",
    "null",
    "of",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "type",
    "typeof",
    "undefined",
    "var",
    "void",
    "while",
    "yield",
  ],
  line_comments: &["//"],
  has_block_comments: true,
};
const PYTHON: Syntax = Syntax {
  keywords: &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif", "else",
    "except", "False", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "None",
    "nonlocal", "not", "or", "pass", "raise", "return", "True", "try", "while", "with", "yield",
  ],
  line_comments: &["#"],
  has_block_comments: false,
};
const SHELL: Syntax = Syntax {
  keywords: &[
    "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if", "in", "local",
    "return", "then", "while",
  ],
  line_comments: &["#"],
  has_block_comments: false,
};
const SQL: Syntax = Syntax {
  keywords: &[
    "ALTER", "AND", "AS", "BY", "CREATE", "DELETE", "DROP", "FROM", "GROUP", "INNER", "INSERT", "INTO",
    "JOIN", "LEFT", "LIMIT", "NOT", "NULL", "ON", "OR", "ORDER", "SELECT", "SET", "TABLE", "UPDATE",
    "VALUES", "WHERE", "alter", "and", "as", "by", "create", "delete", "drop", "from", "group", "inner",
    "insert", "into", "join", "left", "limit", "not", "null", "on", "or", "order", "select", "set", "table",
    "update", "values", "where",
  ],
  line_comments: &["--"],
  has_block_comments: true,
};
const JSON: Syntax = Syntax {
  keywords: &["true", "false", "null"],
  line_comments: &[],
  has_block_comments: false,
};

fn syntax_for(language: &str) -> Option<&'static Syntax> {
  Some(match language.trim().to_lowercase().as_str() {
    "rust" | "rs" => &RUST,
    "js" | "javascript" | "ts" | "typescript" | "jsx" | "tsx" => &JAVASCRIPT,
    "py" | "python" => &PYTHON,
    "sh" | "bash" | "zsh" | "shell" | "console" => &SHELL,
    "sql" => &SQL,
    "json" => &JSON,
    _ => return None,
  })
}

/// Splits `code` into classified spans. Unknown languages come back as one plain token.
pub fn highlight(language: Option<&str>, code: &str) -> Vec<Token> {
  let Some(syntax) = language.and_then(syntax_for) else {
    return vec![Token {
      kind: TokenKind::Plain,
      text: code.to_owned(),
    }];
  };
  let characters: Vec<char> = code.chars().collect();
  let mut tokens: Vec<Token> = Vec::new();
  let mut plain = String::new();
  let mut at = 0;
  let push = |kind: TokenKind, text: String, plain: &mut String, tokens: &mut Vec<Token>| {
    if kind == TokenKind::Plain {
      plain.push_str(&text);
      return;
    }
    if !plain.is_empty() {
      tokens.push(Token {
        kind: TokenKind::Plain,
        text: std::mem::take(plain),
      });
    }
    tokens.push(Token { kind, text });
  };
  while at < characters.len() {
    let rest = &characters[at..];
    if let Some(length) = comment_length(syntax, rest) {
      push(
        TokenKind::Comment,
        rest[..length].iter().collect(),
        &mut plain,
        &mut tokens,
      );
      at += length;
    } else if matches!(rest[0], '"' | '\'' | '`') {
      let length = string_length(rest);
      push(
        TokenKind::Str,
        rest[..length].iter().collect(),
        &mut plain,
        &mut tokens,
      );
      at += length;
    } else if rest[0].is_ascii_digit() && !previous_is_word(&characters, at) {
      let length = rest
        .iter()
        .take_while(|character| character.is_ascii_alphanumeric() || **character == '.' || **character == '_')
        .count();
      push(
        TokenKind::Number,
        rest[..length].iter().collect(),
        &mut plain,
        &mut tokens,
      );
      at += length;
    } else if rest[0].is_alphabetic() || rest[0] == '_' {
      let length = rest
        .iter()
        .take_while(|character| character.is_alphanumeric() || **character == '_')
        .count();
      let word: String = rest[..length].iter().collect();
      let kind = if syntax.keywords.contains(&word.as_str()) {
        TokenKind::Keyword
      } else {
        TokenKind::Plain
      };
      push(kind, word, &mut plain, &mut tokens);
      at += length;
    } else {
      push(TokenKind::Plain, rest[0].to_string(), &mut plain, &mut tokens);
      at += 1;
    }
  }
  if !plain.is_empty() {
    tokens.push(Token {
      kind: TokenKind::Plain,
      text: plain,
    });
  }
  tokens
}

fn previous_is_word(characters: &[char], at: usize) -> bool {
  at > 0 && (characters[at - 1].is_alphanumeric() || characters[at - 1] == '_')
}

/// Characters of the comment that starts `rest`, if one does.
fn comment_length(syntax: &Syntax, rest: &[char]) -> Option<usize> {
  let starts = |marker: &str| {
    marker
      .chars()
      .enumerate()
      .all(|(index, character)| rest.get(index) == Some(&character))
  };
  if syntax.line_comments.iter().any(|marker| starts(marker)) {
    return Some(rest.iter().take_while(|character| **character != '\n').count());
  }
  if syntax.has_block_comments && starts("/*") {
    let end = rest.windows(2).skip(2).position(|pair| pair == ['*', '/']);
    return Some(end.map_or(rest.len(), |position| position + 4));
  }
  None
}

/// Characters of the string literal that starts `rest` (up to and including the closing quote).
fn string_length(rest: &[char]) -> usize {
  let quote = rest[0];
  let mut at = 1;
  while at < rest.len() {
    match rest[at] {
      '\\' => at += 2,
      character if character == quote => return at + 1,
      '\n' if quote != '`' => return at,
      _ => at += 1,
    }
  }
  rest.len()
}

#[cfg(test)]
mod tests {
  use super::*;

  fn kinds(language: &str, code: &str) -> Vec<(TokenKind, String)> {
    highlight(Some(language), code)
      .into_iter()
      .filter(|token| token.kind != TokenKind::Plain)
      .map(|token| (token.kind, token.text))
      .collect()
  }

  #[test]
  fn rust_keywords_strings_comments_and_numbers() {
    assert_eq!(
      kinds("rust", "let x = \"a\\\"b\"; // note\nfn f() -> u32 { 42 }"),
      [
        (TokenKind::Keyword, "let".into()),
        (TokenKind::Str, "\"a\\\"b\"".into()),
        (TokenKind::Comment, "// note".into()),
        (TokenKind::Keyword, "fn".into()),
        (TokenKind::Number, "42".into()),
      ]
    );
  }

  #[test]
  fn unknown_languages_and_text_are_kept_whole() {
    let tokens = highlight(Some("brainfuck"), "+++[>++<-]");
    assert_eq!(tokens.len(), 1);
    let rebuilt: String = highlight(Some("python"), "def f(x):\n  return x # c")
      .into_iter()
      .map(|t| t.text)
      .collect();
    assert_eq!(rebuilt, "def f(x):\n  return x # c");
  }

  #[test]
  fn digits_inside_names_are_not_numbers_and_block_comments_end() {
    assert!(kinds("js", "const a1 = 1;").contains(&(TokenKind::Number, "1".into())));
    assert!(
      !kinds("js", "const a1 = x;")
        .iter()
        .any(|(kind, _)| *kind == TokenKind::Number)
    );
    assert_eq!(
      kinds("sql", "/* hi */ select"),
      [
        (TokenKind::Comment, "/* hi */".into()),
        (TokenKind::Keyword, "select".into())
      ]
    );
  }
}
