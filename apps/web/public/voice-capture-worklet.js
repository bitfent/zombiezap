// Mic capture worklet for proximity voice chat. It just batches the raw mono
// mic samples (at the context's native rate) and posts them to the main thread,
// which downsamples to 16 kHz and frames them. Batching to ~2048 samples keeps
// postMessage traffic low (~20-25 msgs/sec) without adding noticeable latency.
class VoiceCaptureProcessor extends AudioWorkletProcessor {
  constructor() {
    super();
    this.buf = new Float32Array(2048);
    this.n = 0;
  }

  process(inputs) {
    const ch = inputs[0] && inputs[0][0];
    if (ch) {
      for (let i = 0; i < ch.length; i++) {
        this.buf[this.n++] = ch[i];
        if (this.n === this.buf.length) {
          this.port.postMessage(this.buf.slice(0));
          this.n = 0;
        }
      }
    }
    return true; // keep the processor alive
  }
}

registerProcessor("voice-capture", VoiceCaptureProcessor);
