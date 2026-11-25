package de.zeitgleich.teamcraft.calls

import android.content.Context
import android.content.Intent
import android.media.AudioFormat
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.ParcelFileDescriptor
import android.speech.RecognitionListener
import android.speech.RecognitionSupport
import android.speech.RecognitionSupportCallback
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import android.util.Log
import androidx.annotation.RequiresApi
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.UiThreadGuard
import java.io.FileDescriptor
import java.util.Locale
import java.util.concurrent.CompletableFuture
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException
import kotlin.coroutines.resume
import kotlinx.coroutines.suspendCancellableCoroutine
import uniffi.teamcraft_ffi.CaptionLineInfo
import uniffi.teamcraft_ffi.CaptionRecognizeException
import uniffi.teamcraft_ffi.CaptionRecognizer

/** The rate and format the core hands the recognizer (`call_captions.rs`). */
private const val CAPTION_RATE_HZ = 16_000

/**
 * How long one piece may take: the core hands at most 10 s of speech, and the system recognizer
 * reads it from a pipe. Beyond this the recognizer is taken as hung, and captions stop.
 */
private const val RECOGNIZE_TIMEOUT_MILLIS = 20_000L

/** While the pipe is full, the writer looks again this often. */
private const val PIPE_FULL_WAIT_MILLIS = 10L

/** How many finished lines the panel shows. */
private const val SHOWN_LINES = 3

/** A sentence still going on is lighter than finished ones. */
private const val PARTIAL_ALPHA = 0.7f

private const val TAG = "TeamCraftCaptions"

/**
 * Whether this phone can caption calls (#215): Android 13+ (the recognizer takes the call's
 * audio instead of the microphone) with a recognizer that runs **on the device**. Without one,
 * there is no captions button: recognition in the cloud is never used (e2ee.md §8.6).
 */
internal fun areCaptionsOffered(context: Context): Boolean =
  Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU && SpeechRecognizer.isOnDeviceRecognitionAvailable(context)

/** Whether the on-device recognizer knows the language captions use. */
internal enum class CaptionLanguageSupport {
  /** Installed: captions can start. */
  READY,

  /** Supported but not installed: Android was asked to download it. */
  DOWNLOADING,

  /** Not recognized on this device. */
  UNSUPPORTED,
}

/**
 * Asks the on-device recognizer about [locale] (Android 13+). A language it supports but hasn't
 * installed yet is downloaded by Android (its own speech pack, nothing of the call).
 */
@RequiresApi(Build.VERSION_CODES.TIRAMISU)
internal suspend fun checkCaptionLanguage(context: Context, locale: Locale): CaptionLanguageSupport {
  val intent = recognizeIntent(locale)
  val main = Handler(Looper.getMainLooper())
  val support = suspendCancellableCoroutine { continuation ->
    main.post {
      val recognizer = SpeechRecognizer.createOnDeviceSpeechRecognizer(context)
      recognizer.checkRecognitionSupport(
        intent,
        context.mainExecutor,
        object : RecognitionSupportCallback {
          override fun onSupportResult(support: RecognitionSupport) {
            recognizer.destroy()
            if (continuation.isActive) continuation.resume(languageSupport(support, locale))
          }

          override fun onError(error: Int) {
            recognizer.destroy()
            Log.w(TAG, "the on-device recognizer's languages are unknown: error $error")
            if (continuation.isActive) continuation.resume(CaptionLanguageSupport.UNSUPPORTED)
          }
        },
      )
    }
  }
  if (support == CaptionLanguageSupport.DOWNLOADING) {
    main.post {
      val recognizer = SpeechRecognizer.createOnDeviceSpeechRecognizer(context)
      recognizer.triggerModelDownload(intent)
      recognizer.destroy()
    }
  }
  return support
}

@RequiresApi(Build.VERSION_CODES.TIRAMISU)
private fun languageSupport(support: RecognitionSupport, locale: Locale): CaptionLanguageSupport {
  val matches = { tags: List<String> -> tags.any { Locale.forLanguageTag(it).language == locale.language } }
  return when {
    matches(support.installedOnDeviceLanguages) -> CaptionLanguageSupport.READY
    matches(support.pendingOnDeviceLanguages) || matches(support.supportedOnDeviceLanguages) -> CaptionLanguageSupport.DOWNLOADING
    else -> CaptionLanguageSupport.UNSUPPORTED
  }
}

/**
 * The system's speech recognizer in on-device mode (`createOnDeviceSpeechRecognizer`: it never
 * sends audio off the phone), fed the call's decrypted audio through a pipe instead of the
 * microphone. The core calls [recognize] on its captions thread, one piece at a time; the
 * recognizer itself lives on the main thread, as Android requires.
 */
@RequiresApi(Build.VERSION_CODES.TIRAMISU)
internal class OnDeviceCaptionRecognizer(private val context: Context, private val locale: Locale) : CaptionRecognizer {
  private val main = Handler(Looper.getMainLooper())
  private val writer = Executors.newSingleThreadExecutor { runnable -> Thread(runnable, "captions-pipe") }

  /** Created on first use, on the main thread; only touched there. */
  private var maybeRecognizer: SpeechRecognizer? = null

  @Volatile private var isClosed = false

  override fun recognize(pcm16: ByteArray): String {
    UiThreadGuard.shared.check("OnDeviceCaptionRecognizer.recognize")
    if (isClosed) throw CaptionRecognizeException.Failed("closed")
    val (source, sink) = ParcelFileDescriptor.createPipe()
    val result = CompletableFuture<String>()
    try {
      main.post { start(source, result) }
      writer.execute { writeAll(sink, pcm16, result) }
      // blocking-ok: the core's captions thread (UiThreadGuard above), never the UI thread.
      return result.get(RECOGNIZE_TIMEOUT_MILLIS, TimeUnit.MILLISECONDS)
    } catch (_: TimeoutException) {
      main.post { maybeRecognizer?.cancel() }
      throw CaptionRecognizeException.Failed("timed out")
    } catch (error: java.util.concurrent.ExecutionException) {
      throw CaptionRecognizeException.Failed(error.cause?.message ?: "failed")
    } finally {
      // The writer stops at its next try once the result is in (see writeAll).
      result.complete("")
      runCatching { source.close() }
    }
  }

  /** Stops the recognizer; a recognition under way fails. Captions are off. */
  fun close() {
    isClosed = true
    writer.shutdown()
    main.post {
      maybeRecognizer?.destroy()
      maybeRecognizer = null
    }
  }

  private fun start(source: ParcelFileDescriptor, result: CompletableFuture<String>) {
    if (isClosed) {
      result.completeExceptionally(IllegalStateException("closed"))
      return
    }
    val recognizer = maybeRecognizer ?: SpeechRecognizer.createOnDeviceSpeechRecognizer(context).also { maybeRecognizer = it }
    recognizer.setRecognitionListener(Listener(result))
    val intent = recognizeIntent(locale).apply {
      putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE, source)
      putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_CHANNEL_COUNT, 1)
      putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_ENCODING, AudioFormat.ENCODING_PCM_16BIT)
      putExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_SAMPLING_RATE, CAPTION_RATE_HZ)
    }
    recognizer.startListening(intent)
  }

  /**
   * Writes the piece into the pipe and closes it (the end of the audio). Non-blocking, so a
   * recognizer that stopped reading can't hold this thread: it gives up once the result is in.
   */
  private fun writeAll(sink: ParcelFileDescriptor, pcm16: ByteArray, result: CompletableFuture<String>) {
    try {
      val fd: FileDescriptor = sink.fileDescriptor
      Os.fcntlInt(fd, OsConstants.F_SETFL, OsConstants.O_NONBLOCK)
      var offset = 0
      while (offset < pcm16.size && !result.isDone) {
        try {
          offset += Os.write(fd, pcm16, offset, pcm16.size - offset)
        } catch (error: ErrnoException) {
          if (error.errno != OsConstants.EAGAIN) throw error
          // blocking-ok: the captions' own pipe thread, never the UI thread.
          Thread.sleep(PIPE_FULL_WAIT_MILLIS)
        }
      }
    } catch (error: ErrnoException) {
      // The recognizer closed its end (it ended or failed): its listener says which.
      Log.d(TAG, "the recognizer stopped reading: errno ${error.errno}")
    } finally {
      runCatching { sink.close() }
    }
  }

  /** The recognizer's answer for one piece. Never logs the text. */
  private class Listener(private val result: CompletableFuture<String>) : RecognitionListener {
    override fun onResults(results: Bundle) {
      val text = results.getStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION)?.firstOrNull().orEmpty()
      result.complete(text)
    }

    override fun onError(error: Int) {
      when (error) {
        // Nothing understood in the piece: no words, not a failure.
        SpeechRecognizer.ERROR_NO_MATCH, SpeechRecognizer.ERROR_SPEECH_TIMEOUT -> result.complete("")
        else -> result.completeExceptionally(IllegalStateException("recognizer error $error"))
      }
    }

    override fun onReadyForSpeech(params: Bundle?) = Unit

    override fun onBeginningOfSpeech() = Unit

    override fun onRmsChanged(rmsdB: Float) = Unit

    override fun onBufferReceived(buffer: ByteArray?) = Unit

    override fun onEndOfSpeech() = Unit

    override fun onPartialResults(partialResults: Bundle?) = Unit

    override fun onEvent(eventType: Int, params: Bundle?) = Unit
  }
}

/** Free speech in [locale], on the device only. */
private fun recognizeIntent(locale: Locale): Intent =
  Intent(RecognizerIntent.ACTION_RECOGNIZE_SPEECH).apply {
    putExtra(RecognizerIntent.EXTRA_LANGUAGE_MODEL, RecognizerIntent.LANGUAGE_MODEL_FREE_FORM)
    putExtra(RecognizerIntent.EXTRA_LANGUAGE, locale.toLanguageTag())
    putExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE, true)
    putExtra(RecognizerIntent.EXTRA_PARTIAL_RESULTS, false)
  }

/**
 * The captions over the stage's lower edge (#215): the last lines with the speaker's name, a
 * sentence still going on lighter. The header says the captions stay on this device. Finished
 * sentences are said to TalkBack politely, parts so far are not.
 */
@Composable
internal fun CaptionsPanel(lines: List<CaptionLineInfo>, names: Map<String, String>) {
  val someone = stringResource(R.string.call_captions_someone)
  val nameOf = { id: String -> names[id]?.takeIf { it.isNotEmpty() } ?: someone }
  CaptionAnnouncer(lines, nameOf)
  Surface(
    color = MaterialTheme.colorScheme.inverseSurface.copy(alpha = 0.9f),
    contentColor = MaterialTheme.colorScheme.inverseOnSurface,
    shape = RoundedCornerShape(16.dp),
    modifier = Modifier.padding(bottom = 12.dp).widthIn(max = 560.dp).fillMaxWidth(),
  ) {
    Column(
      Modifier.padding(horizontal = 16.dp, vertical = 10.dp),
      verticalArrangement = Arrangement.spacedBy(4.dp),
    ) {
      Text(
        stringResource(R.string.call_captions_only_here),
        style = MaterialTheme.typography.labelMedium,
        color = MaterialTheme.colorScheme.inverseOnSurface.copy(alpha = PARTIAL_ALPHA),
      )
      val shown = lines.takeLast(SHOWN_LINES)
      if (shown.isEmpty()) {
        Text(stringResource(R.string.call_captions_listening), style = MaterialTheme.typography.bodyMedium)
      }
      // Lines speak through the announcer, once each; read as a whole they'd repeat.
      Column(Modifier.clearAndSetSemantics {}, verticalArrangement = Arrangement.spacedBy(4.dp)) {
        shown.forEach { line ->
          Text(
            buildAnnotatedString {
              withStyle(SpanStyle(fontWeight = FontWeight.SemiBold)) { append(nameOf(line.userId)) }
              append("  ")
              append(line.text)
            },
            style = MaterialTheme.typography.bodyLarge,
            color = if (line.isFinal) {
              MaterialTheme.colorScheme.inverseOnSurface
            } else {
              MaterialTheme.colorScheme.inverseOnSurface.copy(alpha = PARTIAL_ALPHA)
            },
          )
        }
      }
    }
  }
}

/** Says each finished sentence once, politely; the lines on screen are hidden from TalkBack. */
@Composable
private fun CaptionAnnouncer(lines: List<CaptionLineInfo>, nameOf: (String) -> String) {
  val resources = LocalContext.current.resources
  var lastSaid by remember { mutableStateOf<ULong?>(null) }
  var message by remember { mutableStateOf("") }
  var flip by remember { mutableStateOf(false) }
  val currentNameOf by rememberUpdatedState(nameOf)
  LaunchedEffect(lines) {
    val said = lastSaid
    val finals = lines.filter { it.isFinal && (said == null || it.id > said) }
    if (finals.isEmpty()) return@LaunchedEffect
    lastSaid = finals.maxOf { it.id }
    val text = finals.joinToString(". ") { resources.getString(R.string.call_captions_announce, currentNameOf(it.userId), it.text) }
    // The same words twice in a row still change the region.
    flip = !flip
    message = if (flip) "$text​" else text
  }
  Box(
    Modifier.size(1.dp).semantics {
      liveRegion = LiveRegionMode.Polite
      contentDescription = message
    },
  )
}
