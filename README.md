# pipecap

Native PipeWire screen capture for Electron on Linux. Shows the system's xdg-desktop-portal picker (KDE, GNOME, Sway, etc.) and delivers raw video frames via shared memory + audio capture with per-app isolation.

Built with Rust and [napi-rs](https://napi.rs). Respects Wayland's security model — all capture requires explicit user consent through the portal.

## Features

- **Video capture** via shared memory (`/dev/shm`) — zero-copy from PipeWire to your renderer
- **System audio** — captures all desktop audio via sink monitor
- **Per-app audio** — auto-detects the captured app and isolates its audio stream
- **Dynamic audio switching** — change audio source at runtime without restarting video
- **App discovery** — list audio-producing applications for user selection
- **Wayland-native** — uses xdg-desktop-portal, works on KDE, GNOME, Sway, etc.

## Install

```bash
npm install @librecord/pipecap
```

Requires PipeWire and xdg-desktop-portal running on the system (standard on modern Linux desktops).

## API

```typescript
import {
  showPicker,
  startCapture,
  readAudio,
  stopCapture,
  setAudioTarget,
  listAudioApps,
  isCapturing,
} from '@librecord/pipecap';

// 1. Show native picker
const result = await showPicker(3); // 1=monitors, 2=windows, 3=both
if (!result) process.exit(0);

const stream = result.streams[0];

// 2. Start capture — returns detected app name for per-app audio
const info = startCapture({
  nodeId: stream.nodeId,
  pipewireFd: result.pipewireFd,
  fps: 30,
  audio: true,
  sourceType: stream.sourceType, // 1=monitor→system audio, 2=window→per-app
});
// info.detectedApp is the auto-detected app name, or null

// 3. Read video from shared memory (info.shmPath)
// Layout: [32-byte header][frame slot 0][frame slot 1]
// Header: seq(u64) width(u32) height(u32) stride(u32) data_offset(u32) data_size(u32)

// 4. Poll audio
setInterval(() => {
  const audio = readAudio(); // { channels, sampleRate, data: Buffer } | null
  if (audio) processAudio(audio);
}, 20);

// 5. Switch audio source at runtime
setAudioTarget('system');       // all desktop audio
setAudioTarget('Firefox');      // specific app
setAudioTarget('none');         // disable audio

// 6. List audio-producing apps (for dropdown UI)
const apps = listAudioApps();   // [{ name: 'Firefox', binary: 'firefox' }, ...]

// 7. Stop
stopCapture();
```

## Audio Modes

| Scenario | Audio mode | How it works |
|----------|-----------|--------------|
| Monitor capture | System | Captures from default sink monitor |
| Window capture (app detected) | Per-app | Auto-detected from KWin's `media.name` property |
| Window capture (app unknown) | Fallback to system | Wine/Proton games, Electron apps with WebRTC |
| User switches via UI | Dynamic | `setAudioTarget()` recreates the audio pipeline |

Per-app detection works for native Linux apps (Firefox, Chrome, Spotify, VLC, games). Apps that bypass PipeWire (Discord's internal WebRTC) fall back to system audio.

## Electron Integration

pipecap ships with helpers for Electron's main process, preload, and renderer.

### Main process

```js
const { setupPipecap } = require('@librecord/pipecap/electron/main');
setupPipecap(ipcMain, () => mainWindow);
```

### Preload

```js
const { exposePipecap } = require('@librecord/pipecap/electron/preload');
exposePipecap(contextBridge, ipcRenderer);
```

### Renderer

```js
const { createScreenShareStream } = require('@librecord/pipecap/electron/renderer');
const { stream, stop } = createScreenShareStream({ fps: 30, audio: true });
// stream is a MediaStream — pass to LiveKit, WebRTC, or <video>
```

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
  lib.rs          — napi exports (showPicker, startCapture, setAudioTarget, ...)
  capture.rs      — PipeWire video stream → shared memory
  audio/
    mod.rs        — AudioCapturer, AudioTarget, shared helpers
    system.rs     — system audio (sink monitor)
    app.rs        — per-app audio (registry watcher, fresh stream per node)
    resolve.rs    — app identity resolution, audio app discovery
  portal.rs       — xdg-desktop-portal ScreenCast client
  shm.rs          — double-buffered shared memory frame buffer
  pw_util.rs      — PipeWire utilities (pod serialization, roundtrip)
```

## Acknowledgements

- [**ashpd**](https://github.com/bilelmoussaoui/ashpd) — Rust xdg-desktop-portal bindings
- [**pipewire-rs**](https://gitlab.freedesktop.org/pipewire/pipewire-rs) — Rust PipeWire bindings
- [**napi-rs**](https://napi.rs) — Rust → Node.js native addon bridge
- [**obs-pipewire-audio-capture**](https://github.com/dimtpap/obs-pipewire-audio-capture) — reference for the virtual sink + port linking approach

## License

[AGPL-3.0](LICENSE)
