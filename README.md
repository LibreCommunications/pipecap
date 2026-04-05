# pipecap

Native PipeWire audio capture for Electron on Linux. Captures system audio or per-app audio with dynamic source switching at runtime.

Built with Rust and [napi-rs](https://napi.rs). Designed to complement Chromium's built-in `getDisplayMedia` for screen sharing — Chromium handles video, pipecap handles audio.

## Why

Electron's `getDisplayMedia` on Linux/Wayland captures video natively via PipeWire, but has no per-app audio support. The common workaround (venmic) only captures system-wide audio. pipecap gives you:

- **System audio** — all desktop audio via PipeWire sink monitor
- **Per-app audio** — capture only a specific app's audio (Firefox, Chrome, games, etc.)
- **Dynamic switching** — change audio source at runtime without restarting capture
- **App discovery** — list all PipeWire apps for a user-facing dropdown

## Install

```bash
npm install @librecord/pipecap
```

Requires PipeWire running on the system (standard on modern Linux desktops).

## API

```typescript
import {
  startAudio,
  stopAudio,
  setAudioTarget,
  readAudio,
  listAudioApps,
  listAllApps,
  isCapturing,
} from '@librecord/pipecap';

// 1. Start system audio capture
startAudio();

// 2. Poll audio samples
setInterval(() => {
  const audio = readAudio();
  // audio: { channels: number, sampleRate: number, data: Buffer } | null
  if (audio) processAudio(audio);
}, 20);

// 3. Switch audio source at runtime
setAudioTarget('system');       // all desktop audio
setAudioTarget('Firefox');      // only Firefox's audio
setAudioTarget('none');         // stop audio capture

// 4. List apps for a dropdown UI
const playing = listAudioApps();  // apps currently producing audio
const all = listAllApps();        // all PipeWire apps (including silent ones)
// [{ name: 'Firefox', binary: 'firefox' }, ...]

// 5. Stop
stopAudio();
```

## Electron Integration

pipecap is audio-only. For screen sharing, use it alongside `getDisplayMedia`:

```typescript
// Video: Chromium's native PipeWire capture (one portal dialog)
await room.localParticipant.setScreenShareEnabled(true, {
  audio: false, // audio comes from pipecap
  resolution: { width: 1920, height: 1080 },
});

// Audio: pipecap captures system audio via PipeWire
const pipecap = require('@librecord/pipecap');
pipecap.startAudio();

// Build a MediaStreamTrack from pipecap's audio and publish to LiveKit
const audioCtx = new AudioContext({ sampleRate: 48000 });
const dest = audioCtx.createMediaStreamDestination();
// ... feed readAudio() samples into Web Audio ...
await room.localParticipant.publishTrack(dest.stream.getAudioTracks()[0], {
  source: Track.Source.ScreenShareAudio,
});
```

## Per-App Audio

When the user selects a specific app, pipecap watches the PipeWire registry for that app's audio output node. If the app isn't playing audio yet, pipecap waits — when audio starts, it connects automatically.

```typescript
// User picks "Firefox" from a dropdown
setAudioTarget('Firefox');
// Firefox starts playing a YouTube video 30 seconds later
// → pipecap automatically connects and audio flows
```

Apps that use PipeWire/PulseAudio for playback work with per-app capture:
Firefox, Chrome, Spotify, VLC, mpv, Steam/Proton games, and most native Linux apps.

## Building from source

```bash
# Prerequisites: Rust toolchain, PipeWire dev headers, Node.js 20+
sudo pacman -S pipewire libpipewire   # Arch
sudo apt install libpipewire-0.3-dev  # Debian/Ubuntu

npm install
npm run build
```

## Architecture

```
src/
  lib.rs          — napi exports (startAudio, setAudioTarget, listAllApps, ...)
  audio/
    mod.rs        — AudioCapturer, AudioTarget, shared helpers
    system.rs     — system audio (PipeWire sink monitor)
    app.rs        — per-app audio (registry watcher, fresh stream per node)
    resolve.rs    — app name matching, PipeWire graph queries
  pw_util.rs      — PipeWire utilities (pod serialization, roundtrip)
```

## Acknowledgements

- [**pipewire-rs**](https://gitlab.freedesktop.org/pipewire/pipewire-rs) — Rust PipeWire bindings
- [**napi-rs**](https://napi.rs) — Rust → Node.js native addon bridge
- [**obs-pipewire-audio-capture**](https://github.com/dimtpap/obs-pipewire-audio-capture) — reference for PipeWire audio capture patterns

## License

[AGPL-3.0](LICENSE)
