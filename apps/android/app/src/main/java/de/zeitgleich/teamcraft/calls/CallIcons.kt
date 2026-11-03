package de.zeitgleich.teamcraft.calls

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.addPathNodes
import androidx.compose.ui.unit.dp

/**
 * The call controls' icons, from the Material symbol set. `material-icons-core` has no microphone,
 * camera or speaker, and the extended set is large; these few are drawn from their path data.
 */
internal object CallIcons {
  val Mic = icon(
    "Mic",
    "M12,14c1.66,0 2.99,-1.34 2.99,-3L15,5c0,-1.66 -1.34,-3 -3,-3S9,3.34 9,5v6c0,1.66 1.34,3 3,3z" +
      "M17.3,11c0,3 -2.54,5.1 -5.3,5.1S6.7,14 6.7,11L5,11c0,3.41 2.72,6.23 6,6.72L11,21h2v-3.28" +
      "c3.28,-0.48 6,-3.3 6,-6.72h-1.7z",
  )
  val MicOff = icon(
    "MicOff",
    "M19,11h-1.7c0,0.74 -0.16,1.43 -0.43,2.05l1.23,1.23c0.56,-0.98 0.9,-2.09 0.9,-3.28z" +
      "M14.98,11.17c0,-0.06 0.02,-0.11 0.02,-0.17L15,5c0,-1.66 -1.34,-3 -3,-3S9,3.34 9,5v0.18l5.98,5.99z" +
      "M4.27,3L3,4.27l6.01,6.01v0.72c0,1.66 1.33,3 2.99,3 0.22,0 0.44,-0.03 0.65,-0.08l1.66,1.66" +
      "c-0.71,0.33 -1.5,0.52 -2.31,0.52 -2.76,0 -5.3,-2.1 -5.3,-5.1L5,11c0,3.41 2.72,6.23 6,6.72L11,21h2" +
      "v-3.28c0.91,-0.13 1.77,-0.45 2.54,-0.9L19.73,21 21,19.73 4.27,3z",
  )
  val Videocam = icon(
    "Videocam",
    "M17,10.5V7c0,-0.55 -0.45,-1 -1,-1H4c-0.55,0 -1,0.45 -1,1v10c0,0.55 0.45,1 1,1h12" +
      "c0.55,0 1,-0.45 1,-1v-3.5l4,4v-11l-4,4z",
  )
  val VideocamOff = icon(
    "VideocamOff",
    "M21,6.5l-4,4V7c0,-0.55 -0.45,-1 -1,-1H9.82L21,17.18V6.5z" +
      "M3.27,2L2,3.27 4.73,6H4c-0.55,0 -1,0.45 -1,1v10c0,0.55 0.45,1 1,1h12c0.21,0 0.39,-0.08 0.54,-0.18" +
      "L19.73,21 21,19.73 3.27,2z",
  )
  val VolumeUp = icon(
    "VolumeUp",
    "M3,9v6h4l5,5V4L7,9H3z" +
      "M16.5,12c0,-1.77 -1.02,-3.29 -2.5,-4.03v8.05c1.48,-0.73 2.5,-2.25 2.5,-4.02z" +
      "M14,3.23v2.06c2.89,0.86 5,3.54 5,6.71s-2.11,5.85 -5,6.71v2.06c4.01,-0.91 7,-4.49 7,-8.77" +
      "s-2.99,-7.86 -7,-8.77z",
  )
  val CallEnd = icon(
    "CallEnd",
    "M12,9c-1.6,0 -3.15,0.25 -4.6,0.72v3.1c0,0.39 -0.23,0.74 -0.56,0.9 -0.98,0.49 -1.87,1.12 -2.66,1.85" +
      "-0.18,0.18 -0.43,0.28 -0.7,0.28 -0.28,0 -0.53,-0.11 -0.71,-0.29L0.29,13.08c-0.18,-0.17 -0.29,-0.42 -0.29,-0.7" +
      " 0,-0.28 0.11,-0.53 0.29,-0.71C3.34,8.78 7.46,7 12,7s8.66,1.78 11.71,4.67c0.18,0.18 0.29,0.43 0.29,0.71" +
      " 0,0.28 -0.11,0.53 -0.29,0.71l-2.48,2.48c-0.18,0.18 -0.43,0.29 -0.71,0.29 -0.27,0 -0.52,-0.11 -0.7,-0.28" +
      "-0.79,-0.74 -1.69,-1.36 -2.67,-1.85 -0.33,-0.16 -0.56,-0.5 -0.56,-0.9v-3.1C15.15,9.25 13.6,9 12,9z",
  )

  private fun icon(name: String, pathData: String): ImageVector =
    ImageVector.Builder(name, 24.dp, 24.dp, 24f, 24f)
      .addPath(addPathNodes(pathData), fill = SolidColor(Color.Black))
      .build()
}
