//! Identity provider groups to spaces and public channels, in the admin console
//! (docs/provisioning.md §5).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{ConversationId, SpaceId};

/// The most rules an instance keeps.
pub const MAX_GROUP_MAPPINGS: usize = 100;
/// The most spaces one rule names.
pub const MAX_MAPPED_SPACES: usize = 20;
/// The most channels one rule names.
pub const MAX_MAPPED_CHANNELS: usize = 100;
/// The longest group path, in characters.
pub const MAX_GROUP_PATH_CHARS: usize = 255;

/// One rule: the provider's members of `group_path` join `space_ids` and, of those spaces, the public
/// channels in `channel_ids`. Additions only; leaving the group removes nobody.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GroupMappingRule {
  /// The provider's group path, starting with `/`, e.g. `/engineering`.
  pub group_path: String,
  pub space_ids: Vec<SpaceId>,
  #[serde(default)]
  pub channel_ids: Vec<ConversationId>,
}

/// `GET` and the answer of `PUT /api/v1/admin/identity-provider/group-mappings`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GroupMappingsResponse {
  /// Ordered by path.
  pub mappings: Vec<GroupMappingRule>,
}

/// `PUT /api/v1/admin/identity-provider/group-mappings`: the whole list, replacing the stored one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PutGroupMappingsRequest {
  pub mappings: Vec<GroupMappingRule>,
}

impl GroupMappingRule {
  /// The path as the provider spells it: trimmed, with one leading `/`, no trailing one.
  pub fn normalized_path(&self) -> String {
    let trimmed = self.group_path.trim().trim_matches('/');
    format!("/{trimmed}")
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn paths_are_normalized_and_channels_default_to_none() {
    let rule: GroupMappingRule =
      serde_json::from_str(r#"{"group_path":" engineering/ ","space_ids":["5"]}"#).unwrap();
    assert_eq!(rule.normalized_path(), "/engineering");
    assert!(rule.channel_ids.is_empty());
    let nested = GroupMappingRule {
      group_path: "/staff/engineering".into(),
      ..rule
    };
    assert_eq!(nested.normalized_path(), "/staff/engineering");
  }
}
