//! Setup shared by the scenarios: a space with members, a channel with an encrypted group.

use teamcraft_proto::{ConversationId, SpaceId};

use crate::peer::Peer;
use crate::stack::Stack;

/// A space created through REST by `owner`, with `members` added under the given roles
/// (`admin`, `member`, `guest`). Returns the space ID.
pub async fn space_with(stack: &Stack, owner: &Peer, name: &str, members: &[(&Peer, &str)]) -> SpaceId {
  let space = stack.create_space(&owner.tokens, name, "invite_only").await;
  for (member, role) in members {
    stack
      .add_space_member(&owner.tokens, &space, &member.user_id().to_string(), role)
      .await;
  }
  space.parse().expect("space ID")
}

/// A channel in `space` created by `owner`; every member is added to its MLS group and has joined
/// (received the welcome) when this returns.
pub async fn channel_with(
  owner: &mut Peer,
  space: SpaceId,
  name: &str,
  members: &mut [&mut Peer],
) -> ConversationId {
  let channel = owner
    .client
    .create_channel(space, name)
    .await
    .expect("creating the channel");
  for member in members.iter() {
    owner
      .client
      .add_member(channel, member.user_id())
      .await
      .unwrap_or_else(|error| panic!("adding {} to {name}: {error}", member.name));
  }
  for member in members.iter_mut() {
    member.wait_joined(channel).await;
  }
  channel
}
