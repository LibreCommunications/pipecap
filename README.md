# pipecap

Native PipeWire screen capture for Electron on Linux. Shows the system's xdg-desktop-portal picker (KDE, GNOME, Sway, etc.) and returns raw video frames + system audio from the selected source.

Built with Rust and [napi-rs](https://napi.rs). Respects Wayland's security model — all capture requires explicit user consent through the portal.

## Why

Electron's `desktopCapturer.getSources()` cannot see native Wayland windows. This module bypasses that limitation by calling xdg-desktop-portal directly via D-Bus, then consuming the PipeWire video/audio streams natively.

## Install

```bash
npm install @librecord/pipecap
```

Requires PipeWire and xdg-desktop-portal running on the system (standard on modern Linux desktops).

## Electron Integration

pipecap ships with helpers for Electron's main process, preload, and renderer.

### Main process

```typescript
import { ipcMain } from 'electron';
import { setupPipecap } from '@librecord/pipecap/electron/main';

app.whenReady().then(() => {
  setupPipecap(ipcMain, () => mainWindow);
});
```

### Preload

```typescript
import { contextBridge, ipcRenderer } from 'electron';
import { exposePipecap } from '@librecord/pipecap/electron/preload';

exposePipecap(contextBridge, ipcRenderer);
```

### Renderer

```typescript
import { createScreenShareStream } from '@librecord/pipecap/electron/renderer';

// 1. Show native picker
const streams = await window.pipecap.showPicker(3); // 1=monitors, 2=windows, 3=both
if (!streams) return; // User cancelled

// 2. Start capture
await window.pipecap.startCapture({
  nodeId: streams[0].nodeId,
  width: streams[0].width,
  height: streams[0].height,
  fps: 30,
  audio: true,
  excludePid: myPid, // Prevent feedback
});

// 3. Get a MediaStream for LiveKit / WebRTC / <video>
const { stream, stop } = createScreenShareStream({ fps: 30, audio: true });

// 4. Pass to LiveKit
await room.localParticipant.publishTrack(stream.getVideoTracks()[0], {
  source: Track.Source.ScreenShare,
});

// 5. When done
stop();
```

## Low-Level API

If you don't use the Electron helpers:

```typescript
import { showPicker, startCapture, readFrame, readAudio, stopCapture, isCapturing } from '@librecord/pipecap';

const streams = await showPicker(3);
if (!streams) process.exit(0);

startCapture({
  nodeId: streams[0].nodeId,
  width: 1920,
  height: 1080,
  fps: 30,
  audio: true,
  excludePid: process.pid,
});

setInterval(() => {
  const frame = readFrame();  // { width, height, data: Buffer<RGBA> }
  const audio = readAudio();  // { channels, sampleRate, data: Buffer<f32 PCM> } | null
}, 33);

stopCapture();
```

## Building from source

```bash
# Prerequisites: Rust toolchain, PipeWire dev headers, Node.js 20+
sudo pacman -S pipewire libpipewire   # Arch
sudo apt install libpipewire-0.3-dev  # Debian/Ubuntu

npm install
npm run build
```

## Acknowledgements

This project is built on the work of:

- [**ashpd**](https://github.com/bilelmoussaoui/ashpd) by Bilal Elmoussaoui — Rust wrapper for xdg-desktop-portal that makes the portal API accessible and safe. This is what powers the native screen picker.
- [**pipewire-rs**](https://gitlab.freedesktop.org/pipewire/pipewire-rs) by Tom Wagner and Guillaume Desmottes — Rust bindings for PipeWire that make it possible to consume video streams natively.
- [**napi-rs**](https://napi.rs) by LongYinan — the bridge that turns Rust into a Node.js native addon.

## License

[AGPL-3.0](LICENSE)
