/**
 * pipecap Electron renderer helper.
 *
 * Creates a MediaStream from pipecap's raw frame/audio data so you can
 * pass it directly to LiveKit, WebRTC, or a <video> element.
 *
 * Usage in your renderer:
 *
 *   const { createScreenShareStream } = require('@librecord/pipecap/electron/renderer');
 *
 *   // Show picker and start capture
 *   const streams = await window.pipecap.showPicker(3);
 *   if (!streams) return;
 *
 *   await window.pipecap.startCapture({
 *     nodeId: streams[0].nodeId,
 *     width: streams[0].width,
 *     height: streams[0].height,
 *     fps: 30,
 *     audio: true,
 *     excludePid: myPid,
 *   });
 *
 *   const { stream, stop } = createScreenShareStream({ fps: 30, audio: true });
 *   // stream is a MediaStream — pass to LiveKit
 *   // call stop() when done
 */

/**
 * Create a MediaStream from pipecap frame/audio data.
 * Must be called AFTER window.pipecap.startCapture().
 *
 * @param {{ fps?: number, audio?: boolean }} options
 * @returns {{ stream: MediaStream, stop: () => void }}
 */
function createScreenShareStream(options = {}) {
  const fps = options.fps || 30;
  const includeAudio = options.audio || false;

  // Video: canvas → captureStream
  const canvas = document.createElement('canvas');
  canvas.width = 1920;
  canvas.height = 1080;
  const ctx = canvas.getContext('2d');
  const videoStream = canvas.captureStream(fps);

  const cleanups = [];

  // Listen for video frames
  const unsubFrame = window.pipecap.onFrame((frame) => {
    if (canvas.width !== frame.width || canvas.height !== frame.height) {
      canvas.width = frame.width;
      canvas.height = frame.height;
    }
    // frame.data is RGBA pixels
    const imageData = new ImageData(
      new Uint8ClampedArray(frame.data),
      frame.width,
      frame.height,
    );
    ctx.putImageData(imageData, 0, 0);
  });
  cleanups.push(unsubFrame);

  // Combined stream
  const stream = new MediaStream();
  videoStream.getVideoTracks().forEach((t) => stream.addTrack(t));

  // Audio: ScriptProcessorNode (deprecated but universally supported)
  // or AudioWorklet for better performance
  if (includeAudio) {
    const audioCtx = new AudioContext({ sampleRate: 48000 });
    const dest = audioCtx.createMediaStreamDestination();

    // Buffer incoming audio chunks
    let pendingSamples = new Float32Array(0);

    const unsubAudio = window.pipecap.onAudio((chunk) => {
      // chunk.data is f32 LE bytes
      const f32 = new Float32Array(
        chunk.data.buffer,
        chunk.data.byteOffset,
        chunk.data.byteLength / 4,
      );
      // Append to pending buffer
      const combined = new Float32Array(pendingSamples.length + f32.length);
      combined.set(pendingSamples);
      combined.set(f32, pendingSamples.length);
      pendingSamples = combined;
    });
    cleanups.push(unsubAudio);

    // Feed audio to the destination via a ScriptProcessorNode
    const channels = 2;
    const bufferSize = 4096;
    const processor = audioCtx.createScriptProcessor(bufferSize, 0, channels);
    processor.onaudioprocess = (e) => {
      const needed = bufferSize * channels;
      if (pendingSamples.length >= needed) {
        // Deinterleave into output channels
        for (let ch = 0; ch < channels; ch++) {
          const output = e.outputBuffer.getChannelData(ch);
          for (let i = 0; i < bufferSize; i++) {
            output[i] = pendingSamples[i * channels + ch] || 0;
          }
        }
        pendingSamples = pendingSamples.slice(needed);
      } else {
        // Not enough data — output silence
        for (let ch = 0; ch < channels; ch++) {
          e.outputBuffer.getChannelData(ch).fill(0);
        }
      }
    };
    processor.connect(dest);

    // Kick-start the audio context (Chrome autoplay policy)
    audioCtx.resume();

    dest.stream.getAudioTracks().forEach((t) => stream.addTrack(t));

    cleanups.push(() => {
      processor.disconnect();
      audioCtx.close();
    });
  }

  function stop() {
    cleanups.forEach((fn) => fn());
    stream.getTracks().forEach((t) => t.stop());
    window.pipecap.stopCapture();
  }

  return { stream, stop };
}

module.exports = { createScreenShareStream };
