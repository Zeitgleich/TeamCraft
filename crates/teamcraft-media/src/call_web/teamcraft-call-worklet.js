// The AudioWorklet module of browser calls (teamcraft-media, call_web.rs). Pages load it from their
// own origin (the web client serves this file) or, without a Content-Security-Policy that forbids
// it, from a blob URL of the same text (web_api.rs, WORKLET_SOURCE).
class TeamcraftCapture extends AudioWorkletProcessor {
  process(inputs) {
    const channel = inputs[0] && inputs[0][0];
    if (channel) this.port.postMessage(channel.slice(0));
    return true;
  }
}

class TeamcraftPlayback extends AudioWorkletProcessor {
  constructor(options) {
    super();
    this.chunks = [];
    this.offset = 0;
    this.available = 0;
    this.maxSamples = options.processorOptions.maxSamples;
    this.port.onmessage = (event) => {
      this.chunks.push(event.data);
      this.available += event.data.length;
      while (this.available > this.maxSamples && this.chunks.length > 1) {
        this.available -= this.chunks[0].length - this.offset;
        this.chunks.shift();
        this.offset = 0;
      }
    };
  }

  process(_inputs, outputs) {
    const output = outputs[0];
    const out = output[0];
    let written = 0;
    while (written < out.length && this.chunks.length > 0) {
      const chunk = this.chunks[0];
      const count = Math.min(out.length - written, chunk.length - this.offset);
      out.set(chunk.subarray(this.offset, this.offset + count), written);
      written += count;
      this.offset += count;
      this.available -= count;
      if (this.offset === chunk.length) {
        this.chunks.shift();
        this.offset = 0;
      }
    }
    out.fill(0, written);
    for (let channel = 1; channel < output.length; channel++) output[channel].set(out);
    return true;
  }
}

// The call's clock (call_web/cadence.rs): a message every `everyFrames` frames of audio. The audio
// thread runs while the call's audio does, also in a hidden tab, where the page's timers are
// throttled. The message carries nothing; the page reads its own clock.
class TeamcraftClock extends AudioWorkletProcessor {
  constructor(options) {
    super();
    this.everyFrames = options.processorOptions.everyFrames;
    this.nextFrame = 0;
  }

  process() {
    if (currentFrame >= this.nextFrame) {
      this.port.postMessage(null);
      this.nextFrame = currentFrame + this.everyFrames;
    }
    return true;
  }
}

registerProcessor("teamcraft-capture", TeamcraftCapture);
registerProcessor("teamcraft-playback", TeamcraftPlayback);
registerProcessor("teamcraft-clock", TeamcraftClock);
