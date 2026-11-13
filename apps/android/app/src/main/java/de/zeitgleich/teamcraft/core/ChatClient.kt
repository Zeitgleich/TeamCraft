package de.zeitgleich.teamcraft.core

import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import uniffi.teamcraft_ffi.AddPeopleOutcome
import uniffi.teamcraft_ffi.GroupMentionReachItem
import uniffi.teamcraft_ffi.MentionableGroupItem
import uniffi.teamcraft_ffi.ChannelMembersInfo
import uniffi.teamcraft_ffi.ChannelRolesReviewItem
import uniffi.teamcraft_ffi.CreateOutcome
import uniffi.teamcraft_ffi.DirectoryChannelsPage
import uniffi.teamcraft_ffi.DirectoryFilterChoices
import uniffi.teamcraft_ffi.DirectoryInvitationsPage
import uniffi.teamcraft_ffi.DirectoryPeopleKind
import uniffi.teamcraft_ffi.DirectoryPeoplePage
import uniffi.teamcraft_ffi.DirectoryPeopleRequest
import uniffi.teamcraft_ffi.InvitationOutcome
import uniffi.teamcraft_ffi.InviteOutcome
import uniffi.teamcraft_ffi.InvitePeopleInput
import uniffi.teamcraft_ffi.LeaveChoice
import uniffi.teamcraft_ffi.MeetingCancelScope
import uniffi.teamcraft_ffi.MeetingEditForm
import uniffi.teamcraft_ffi.MeetingForm
import uniffi.teamcraft_ffi.MeetingSave
import uniffi.teamcraft_ffi.SavedMeeting
import uniffi.teamcraft_ffi.NewChannelInput
import uniffi.teamcraft_ffi.NewSpaceInput
import uniffi.teamcraft_ffi.ReviewedRolesOutcome
import uniffi.teamcraft_ffi.SpaceMenuItem
import uniffi.teamcraft_ffi.TeamCraft

/**
 * The chat features of the core, decoded: reads go through the JSON bridge's snapshot, window and
 * queries, changes through actions ([ChatActions]). Nothing here interprets the data; only the
 * snapshot's deleted accounts get [deletedUserName] when one is given (issue #751).
 */
class ChatClient(
  /** The app's "Deleted user" in the user's language; `null` for readers that show no old messages. */
  private val deletedUserName: (() -> String)? = null,
  /**
   * The store's change counter ([CoreRepository.changeCount]) and the account's generation
   * ([CoreRepository.accountGeneration]), when the reader has them: the snapshot is then read and
   * decoded once per change and shared by every screen that asks, never across accounts.
   */
  private val maybeChanges: (() -> Long)? = null,
  private val generation: () -> Long = { 0L },
  private val core: suspend () -> TeamCraft,
) {
  private val snapshotLock = Mutex()
  private val kept = KeptValue<ChatSnapshot>()

  /**
   * The whole workspace (users, spaces, conversations). Decoding it takes hundreds of milliseconds
   * on a slow phone with a large workspace, so it is kept until the next change.
   */
  suspend fun snapshot(): ChatSnapshot {
    val changes = maybeChanges ?: return readSnapshot()
    return snapshotLock.withLock {
      // Read before the snapshot: a change while it is read makes the next caller read again.
      val account = generation()
      val version = changes()
      kept.at(account, version) ?: readSnapshot().also { kept.keep(account, version, it) }
    }
  }

  /**
   * The snapshot read last, maybe from before the newest change, or `null` before the first read: a
   * screen shows names and which messages are the user's own with it at once, then reads afresh.
   */
  fun maybeKeptSnapshot(): ChatSnapshot? = kept.latest(generation())

  /** Drops the kept snapshot: the session ended, its account's data leaves memory. */
  fun forget() = kept.clear()

  private suspend fun readSnapshot(): ChatSnapshot {
    val snapshot = WireJson.decodeFromString(ChatSnapshot.serializer(), core().snapshot())
    return deletedUserName?.let { snapshot.namingDeletedUsers(it()) } ?: snapshot
  }

  /** The newest `limit` messages of a conversation, oldest first. */
  suspend fun latest(conversation: String, limit: Int): MessageWindow {
    val probe = window(conversation, 0, 0)
    val start = (probe.total - limit).coerceAtLeast(0)
    return window(conversation, start, limit)
  }

  suspend fun window(conversation: String, start: Int, len: Int): MessageWindow =
    WireJson.decodeFromString(
      MessageWindow.serializer(),
      core().messageWindow(conversation, start.toUInt(), len.toUInt()),
    )

  /**
   * The messages of a conversation from [maybeOldest] (the oldest one a feed shows) to the newest,
   * and [older] messages before it; without [maybeOldest] the newest [older] (issue #1058).
   */
  suspend fun tail(conversation: String, maybeOldest: Message?, older: Int): MessageTail =
    WireJson.decodeFromString(
      MessageTail.serializer(),
      core().messageTail(conversation, maybeOldest?.id, maybeOldest?.sentAt ?: 0, older.toUInt()),
    )

  /** [MessageTail.authorsSnapshot], with deleted accounts named like the snapshot's. */
  fun authorsSnapshot(tail: MessageTail): ChatSnapshot? {
    val snapshot = tail.authorsSnapshot() ?: return null
    return deletedUserName?.let { snapshot.namingDeletedUsers(it()) } ?: snapshot
  }

  /**
   * The line under each of [conversations] in the chat list, in one local read for the whole list
   * (#1876). It doesn't move the catch-up focus: listing a conversation isn't opening it.
   */
  suspend fun chatPreviews(conversations: List<String>): ChatPreviews =
    WireJson.decodeFromString(ChatPreviews.serializer(), core().chatPreviews(conversations))

  /** The hits for [query] (`from:`, `in:` and the other filters), with their authors. */
  suspend fun search(query: String): NamedSearchResults =
    WireJson.decodeFromString(NamedSearchResults.serializer(), core().search(query))

  /** The newest [len] messages, read ahead without moving the catch-up focus (the user isn't there yet). */
  suspend fun tailAhead(conversation: String, len: Int): MessageTail =
    WireJson.decodeFromString(MessageTail.serializer(), core().messageTailAhead(conversation, len.toUInt()))

  suspend fun query(query: String): ChatAnswer = decodeAnswer(core().query(query))

  /** A query whose answer goes back to the core undecoded (the image viewer's pages). */
  suspend fun rawQuery(query: String): String = core().query(query)

  suspend fun act(action: String) = core().act(action)

  /**
   * People whose name or username matches [query], at most [limit], without the user and bots. The
   * server's directory answers later as a change; read again then (#1534).
   */
  suspend fun searchPeople(query: String, limit: Int): List<User> =
    decodeAnswer(core().searchPeople(query, limit.toUInt())).people

  /** Whether "New space" shows: the workspace role allows creating spaces. */
  suspend fun canCreateSpaces(): Boolean = core().canCreateSpaces()

  /** Opens, or finds, the DM (one person) or group DM (several) with exactly these people. */
  suspend fun openDirect(users: List<String>): CreateOutcome = core().openDirect(users)

  suspend fun createChannel(input: NewChannelInput): CreateOutcome = core().createChannel(input)

  suspend fun createSpace(input: NewSpaceInput): CreateOutcome = core().createSpace(input)

  /** Adds people to a channel, to its space first where the user may (#1534). */
  suspend fun addPeopleToChannel(conversation: String, users: List<String>): AddPeopleOutcome =
    core().addPeopleToChannel(conversation, users)

  /** One page of the directory's people (#1337), from the server. */
  suspend fun directoryPeople(request: DirectoryPeopleRequest): DirectoryPeoplePage = core().directoryPeople(request)

  /** The values the people filters offer, for [kind] and a space (empty for all). */
  suspend fun directoryFilters(kind: DirectoryPeopleKind, spaceId: String): DirectoryFilterChoices =
    core().directoryFilters(kind, spaceId)

  suspend fun directoryChannels(query: String, spaceId: String, after: String): DirectoryChannelsPage =
    core().directoryChannels(query, spaceId, after)

  suspend fun directoryInvitations(query: String, spaceId: String, after: String): DirectoryInvitationsPage =
    core().directoryInvitations(query, spaceId, after)

  suspend fun invitePeople(input: InvitePeopleInput): InviteOutcome = core().invitePeople(input)

  suspend fun resendInvitation(spaceId: String, invitationId: String): InvitationOutcome =
    core().resendInvitation(spaceId, invitationId)

  suspend fun revokeInvitation(spaceId: String, invitationId: String): InvitationOutcome =
    core().revokeInvitation(spaceId, invitationId)

  /** The files of a draft (#1220), in the order they were attached. */
  suspend fun draftFiles(conversation: String, maybeThread: String?): List<DraftFileItem> =
    decodeDraftFiles(core().query(ChatQueries.draftFiles(conversation, maybeThread)))

  suspend fun extras(conversation: String, messages: List<String>): Map<String, MessageExtras> =
    if (messages.isEmpty()) {
      emptyMap()
    } else {
      query(ChatQueries.messageExtras(conversation, messages)).extras.associateBy { it.message }
    }

  /** A plain server endpoint (notification settings, meetings): answers its JSON. */
  suspend fun api(method: String, path: String, maybeBody: String? = null): String =
    core().apiRequest(method, path, maybeBody)

  suspend fun ownProfile(): OwnProfile = OwnProfile.parse(api("GET", "/me/profile"))

  /** Saves [maybeName] as the member's name, or with `null` goes back to the sign-in name. */
  suspend fun setDisplayName(profile: OwnProfile, maybeName: String?) {
    api("PUT", "/me/profile", profile.updateBody(maybeName))
  }

  suspend fun notificationSettings(): NotificationSettings =
    WireJson.decodeFromString(NotificationSettings.serializer(), api("GET", "/me/notification-settings"))

  suspend fun setNotificationSettings(settings: NotificationSettings) {
    api("PUT", "/me/notification-settings", WireJson.encodeToString(NotificationSettings.serializer(), settings))
  }

  suspend fun conversationNotifications(conversation: String): ConversationNotificationSettings =
    WireJson.decodeFromString(
      ConversationNotificationSettings.serializer(),
      api("GET", "/conversations/$conversation/notification-settings"),
    )

  suspend fun setConversationNotifications(conversation: String, settings: ConversationNotificationSettings) {
    api(
      "PUT",
      "/conversations/$conversation/notification-settings",
      WireJson.encodeToString(ConversationNotificationSettings.serializer(), settings),
    )
  }

  /** The user groups the composer offers after `@` here; none without `use_group_mentions` (#206). */
  suspend fun mentionableGroups(conversation: String): List<MentionableGroupItem> = core().mentionableGroups(conversation)

  /** Who the group mentions in [body] would notify here, for the hint and the confirmation. */
  suspend fun groupMentionReach(conversation: String, body: String): GroupMentionReachItem =
    core().groupMentionReach(conversation, body)

  /** The core's answer to whether the conversation's menu offers "Leave" (#1105). */
  suspend fun leaveChoice(conversation: String): LeaveChoice = core().conversationLeaveChoice(conversation)

  /** `{origin}/c/{id}`, or `null` where the server has no web address. */
  fun conversationLink(origin: String, conversation: String): String? =
    uniffi.teamcraft_ffi.conversationLink(origin, conversation)

  suspend fun spaceMenu(space: String): SpaceMenuItem? = core().spaceMenu(space)

  /** The channel's members section: members with roles, the alarm line, a workspace admin's look (#1262). */
  suspend fun channelMembers(conversation: String): ChannelMembersInfo = core().channelMembers(conversation)

  /** Reads the channel's members and signed roles afresh; the change arrives as a store update. */
  suspend fun refreshChannelMembers(conversation: String) = core().refreshChannelMembers(conversation)

  /** Confirms exactly the roles of [review] as the admin opened it; nothing is signed if the log moved. */
  suspend fun signReviewedChannelRoles(conversation: String, review: ChannelRolesReviewItem): ReviewedRolesOutcome =
    core().signReviewedChannelRoles(conversation, review.rows, review.logCursor)

  /** How many conversations the server just claimed deleted at once, if suspect; told once. */
  suspend fun takeSuspectDeletions(): UInt = core().takeSuspectDeletions()

  /** The space's mute time (`muted_until`), if it is muted ahead. */
  suspend fun spaceMutedUntil(space: String, nowMs: Long): Long? =
    spaceSettings(space)["muted_until"]?.jsonPrimitive?.longOrNull?.takeIf { it > nowMs }

  /** Mutes the space until `maybeUntil` (`null` unmutes); the space's level stays as it is. */
  suspend fun setSpaceMutedUntil(space: String, maybeUntil: Long?) {
    val body = JsonObject(
      spaceSettings(space).filterKeys { it != "muted_until" } +
        (if (maybeUntil == null) emptyMap() else mapOf("muted_until" to JsonPrimitive(maybeUntil))),
    )
    api("PUT", "/spaces/$space/notification-settings", body.toString())
  }

  private suspend fun spaceSettings(space: String): JsonObject =
    WireJson.parseToJsonElement(api("GET", "/spaces/$space/notification-settings")).jsonObject

  /** Quick DND (`manual = "dnd"`, until a time or open-ended) or back to automatic. */
  suspend fun setManualPresence(presence: ManualPresence) {
    api("PUT", "/me/presence", WireJson.encodeToString(ManualPresence.serializer(), presence))
  }

  /** What others see of `userId` now (`online`, `away`, `dnd`, `offline`); `null` if unknown. */
  suspend fun presenceState(userId: String): String? =
    WireJson.decodeFromString(PresenceList.serializer(), api("GET", "/presence?user_ids=$userId"))
      .presences.firstOrNull { it.userId == userId }?.state

  /** Whose public holidays the member's repeating meetings skip (#1450). */
  suspend fun holidayRegion(): HolidayRegion =
    WireJson.decodeFromString(HolidayRegion.serializer(), api("GET", "/me/holiday-region"))

  /** Picks [maybeRegion] (`null`: the workspace's); answers what is stored now. */
  suspend fun setHolidayRegion(maybeRegion: String?): HolidayRegion =
    WireJson.decodeFromString(
      HolidayRegion.serializer(),
      api("PUT", "/me/holiday-region", WireJson.encodeToString(SetHolidayRegion.serializer(), SetHolidayRegion(maybeRegion))),
    )

  /**
   * The user's meetings, read through the core's upkeep (#1841): the organizer's signed admitters
   * of meetings that run or start within a day stay current, and edits whose chat change is still
   * owed are finished.
   */
  suspend fun meetings(): List<Meeting> =
    WireJson.decodeFromString(MeetingList.serializer(), core().meetingUpkeep()).meetings

  /** Schedules [form]; the core sets up the meeting chat's encryption with the members invited here. */
  suspend fun scheduleMeeting(form: MeetingForm, maybeSpaceId: String?): MeetingSave =
    core().scheduleMeeting(form, maybeSpaceId)

  /** "Meet now": [requestKey] is made once per tap, so a retry returns the same meeting. */
  suspend fun startMeetingNow(maybeSpaceId: String?, requestKey: String): SavedMeeting =
    core().startMeetingNow(null, maybeSpaceId, requestKey)

  /** The edit form of a meeting the user organizes, as the server has it now. */
  suspend fun meetingEditForm(meetingId: String): MeetingEditForm = core().meetingEditForm(meetingId)

  suspend fun saveMeetingEdit(meetingId: String, form: MeetingForm): MeetingSave = core().saveMeetingEdit(meetingId, form)

  suspend fun cancelMeeting(meetingId: String, scope: MeetingCancelScope, occurrenceStartsAt: Long) =
    core().cancelMeeting(meetingId, scope, occurrenceStartsAt)

  /** Before joining a meeting's call: the organizer's device sets up a chat that has no group yet. */
  suspend fun prepareMeetingJoin(meetingId: String) = core().prepareMeetingJoin(meetingId)
}
