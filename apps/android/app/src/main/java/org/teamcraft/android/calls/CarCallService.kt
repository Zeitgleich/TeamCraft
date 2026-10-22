package org.teamcraft.android.calls

import android.content.Intent
import android.content.pm.ApplicationInfo
import androidx.car.app.CarAppService
import androidx.car.app.CarContext
import androidx.car.app.Screen
import androidx.car.app.Session
import androidx.car.app.model.Action
import androidx.car.app.model.Header
import androidx.car.app.model.ItemList
import androidx.car.app.model.ListTemplate
import androidx.car.app.model.MessageTemplate
import androidx.car.app.model.Row
import androidx.car.app.model.Template
import androidx.car.app.validation.HostValidator
import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.launch
import org.teamcraft.android.R
import org.teamcraft.android.TeamCraftApplication
import uniffi.teamcraft_ffi.CallPhase
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind

/**
 * Android Auto calling (category CALLING): the car lists direct and group conversations and
 * starts their calls. The call itself is a Telecom call ([CallController]), so the car's own
 * in-call screen, steering-wheel buttons and audio routing handle it.
 */
class CarCallService : CarAppService() {
  override fun createHostValidator(): HostValidator {
    val isDebuggable = applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE != 0
    return if (isDebuggable) {
      HostValidator.ALLOW_ALL_HOSTS_VALIDATOR
    } else {
      HostValidator.Builder(this).addAllowedHosts(androidx.car.app.R.array.hosts_allowlist_sample).build()
    }
  }

  override fun onCreateSession(): Session = object : Session() {
    override fun onCreateScreen(intent: Intent): Screen = ContactsScreen(carContext)
  }
}

/** Conversations to call, or the running call with a hang-up action. */
private class ContactsScreen(carContext: CarContext) : Screen(carContext) {
  private val graph = TeamCraftApplication.graph(carContext)
  private var maybeConversations: List<ConversationItem>? = null

  init {
    lifecycleScope.launch {
      maybeConversations = runCatching { graph.core.core().conversations() }.getOrDefault(emptyList())
        .filter { it.kind == ConversationKind.DIRECT || it.kind == ConversationKind.GROUP }
      invalidate()
    }
    lifecycleScope.launch {
      graph.calls.callState.collect { invalidate() }
    }
  }

  override fun onGetTemplate(): Template {
    val maybeCall = graph.calls.callState.value?.takeIf { it.phase != CallPhase.ENDED }
    if (maybeCall != null) {
      return MessageTemplate.Builder(carContext.getString(R.string.call_ongoing))
        .setHeader(header(R.string.app_name))
        .addAction(
          Action.Builder()
            .setTitle(carContext.getString(R.string.call_hang_up))
            .setOnClickListener { graph.calls.hangUp() }
            .build(),
        )
        .build()
    }
    val builder = ListTemplate.Builder().setHeader(header(R.string.call_car_title))
    val conversations = maybeConversations ?: return builder.setLoading(true).build()
    val list = ItemList.Builder().setNoItemsMessage(carContext.getString(R.string.no_conversations))
    for (conversation in conversations.take(MAX_ROWS)) {
      list.addItem(
        Row.Builder()
          .setTitle(conversation.name)
          .setOnClickListener { graph.calls.startCall(conversation.id, conversation.name) }
          .build(),
      )
    }
    return builder.setSingleList(list.build()).build()
  }

  private fun header(title: Int): Header =
    Header.Builder().setTitle(carContext.getString(title)).setStartHeaderAction(Action.APP_ICON).build()

  private companion object {
    /** Hosts limit list length (ConstraintManager); six rows fit every host. */
    const val MAX_ROWS = 6
  }
}
