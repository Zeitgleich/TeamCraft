use crate::wire_enum::wire_enum;

wire_enum! {
  /// What a conversation is (ARCHITECTURE.md §4.1).
  pub enum ConversationKind {
    Dm => "dm",
    Group => "group",
    Channel => "channel",
    /// The chat of a meeting; created with the meeting, outside spaces.
    Meeting => "meeting",
  }
}

wire_enum! {
  /// Who can find and join a channel.
  pub enum ConversationVisibility {
    Public => "public",
    Private => "private",
  }
}

wire_enum! {
  /// Which history a newly added member can read (e2ee.md).
  pub enum HistoryPolicy {
    /// Only messages from the moment of joining.
    FromJoin => "from_join",
    /// The complete history, shared with the new member's devices.
    Full => "full",
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn kind_wire_format_is_stable() {
    assert_eq!(serde_json::to_string(&ConversationKind::Dm).unwrap(), "\"dm\"");
    assert_eq!(
      serde_json::to_string(&ConversationKind::Group).unwrap(),
      "\"group\""
    );
    assert_eq!(
      serde_json::to_string(&ConversationKind::Channel).unwrap(),
      "\"channel\""
    );
  }

  #[test]
  fn history_policy_wire_format_is_stable() {
    assert_eq!(
      serde_json::to_string(&HistoryPolicy::FromJoin).unwrap(),
      "\"from_join\""
    );
    assert_eq!(
      serde_json::from_str::<HistoryPolicy>("\"full\"").unwrap(),
      HistoryPolicy::Full
    );
  }

  #[test]
  fn unknown_variants_are_preserved() {
    let kind: ConversationKind = serde_json::from_str("\"broadcast\"").unwrap();
    assert_eq!(kind, ConversationKind::Unknown("broadcast".into()));
    assert_eq!(serde_json::to_string(&kind).unwrap(), "\"broadcast\"");
    let policy: HistoryPolicy = serde_json::from_str("\"last_30_days\"").unwrap();
    assert_eq!(policy.as_str(), "last_30_days");
  }

  #[test]
  fn non_string_input_is_rejected() {
    assert!(serde_json::from_str::<ConversationVisibility>("5").is_err());
  }
}
