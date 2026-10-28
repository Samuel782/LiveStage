# LiveStage — Architectural & Feature Status Report

*Snapshot of `main` at commit `f7c8967` (26 Oct 2026). This report was written by reading the code; no source files were changed for it.*

---

## 1. Executive Overview

**What it is.** LiveStage is a desktop show-control application for live events. It runs a theatre or concert show from one operator workstation. Cue lists fire audio, video, lighting (DMX) and control messages. Cues can be triggered from the keyboard, MIDI/MSC, OSC, a WebSocket remote, or chased from SMPTE timecode. Program video goes to projectors and LED walls through multi-display kiosk windows with projection mapping.

**Target use case.** Small-to-medium productions (theatre, corporate, touring music) that would otherwise combine QLab-style playback, a media server, and a basic lighting console. The operator works in a single egui window. The show runs on a dedicated real-time thread, so a busy or frozen UI never delays a cue.

**Size and shape.** Six Rust crates, about 34 000 lines, 194 unit/integration tests (4 ignored on purpose), 17 commits.

| Crate | Lines | Tests | Role |
|---|---:|---:|---|
| `livestage_core` | 7 830 | 86 | Show model, cue state machines, timecode maths, bundle I/O |
| `livestage_ui` | 12 346 | 14 (+2 ignored) | Application: runtime, threads, egui UI, outputs, watchdog |
| `livestage_video` | 5 946 | 30 | wgpu compositor, projection mapping, decoders, NDI, slides |
| `livestage_audio` | 3 418 | 19 (+1 ignored) | Real-time mixer, disk streaming, LTC generator/reader |
| `livestage_dmx` | 3 373 | 29 | Art-Net/sACN, fixtures, console, sACN input, GDTF/OFL import |
| `livestage_control` | 1 406 | 16 | MIDI/MSC, OSC, WebSocket, MTC/Art-Net timecode |

**Technical stack.** The dependencies are pinned as one mutually compatible set. Upgrade wgpu, egui and winit together.

| Layer | Technology |
|---|---|
| Language / build | Rust 2021, stable ≥ 1.92, Cargo workspace |
| Graphics | `wgpu` 24 (Metal / DX12 / Vulkan), WGSL shaders, `winit` 0.30 windows |
| UI | `egui` 0.31 + `egui-wgpu` + `egui_dock` (dockable workspace) |
| Audio | `cpal` 0.15 (CoreAudio / WASAPI / ALSA), `symphonia` 0.5 decoding, `ringbuf` lock-free queues |
| Video decode | Optional `ffmpeg-next` 9 (system FFmpeg; VideoToolbox / D3D11VA / VAAPI hardware decode) |
| Presentations | `hayro` (pure-Rust PDF), built-in PPTX renderer (`zip` + `roxmltree` + `kurbo` + `ab_glyph`), optional LibreOffice |
| Networking | std UDP + `socket2` (shared ports, multicast), `tokio-tungstenite` (WebSocket), `rosc` (OSC), `midir` (MIDI) |
| NDI | NDI SDK loaded at runtime with `libloading` (no link-time dependency) |
| Show files | Folder bundle (`*.livestage/`) with a JSON manifest (schema v2 with migration) and copied assets |

---

## 2. Subsystem Architecture & Implementation Details

### 2.1 `livestage_core` — show model and cue logic

**Main responsibility.** The engine-independent model of a show:

- what a cue is and how it behaves over time;
- how timecode maps to time;
- how a show is stored on disk.

It has no I/O threads and no device code. Everything here is deterministic and unit-testable.

**Key features**

- **Cues** (`cue.rs`, 7 `AnyCue` variants dispatched through a `delegate!` macro):
  - Audio, Video, Timer, DMX, Control, Timeline and Slides cues.
  - Each has pre-wait, fades, hold, end behaviour, and notes.
  - An optional timecode trigger is available on every cue.
- **Cue lists** (`cuelist.rs`): standby/GO, auto-continue and auto-follow, panic (soft/hard), multiple lists running in parallel.
- **Timelines** (`timeline.rs`): stacked cues on a timeline, slaved to a master media clock (usually audio). Starting a timeline mid-way *chases*: media starts part-way through, past DMX looks are re-applied, and past triggers are not re-fired. Seeking in place uses `CueExt::seek` / `Cue::on_seek`.
- **Timecode** (`timecode/`):
  - Exact frame ↔ time maths for 24, 25, 29.97 DF, 29.97 and 30 fps.
  - `TimecodeLock`: a phase/frequency loop with drift compensation, freewheel and jump detection.
  - `InternalClock`: runs on the audio sample clock or the wall clock.
  - `TimecodeEngine`: selects the source (Internal / MTC / Art-Net / LTC).
  - `TimecodeSlave` / `ListTimecode`: chasing for timelines and lists.
  - `ltc.rs`: a complete SMPTE 12M encoder/decoder.
- **Show data**:
  - Routing: audio stems → outputs, logical displays → physical screens.
  - Projection model: Bezier warp, edge blend, masks.
  - Lighting: palettes, effects, submasters, stage plot.
  - Multiview layouts, cue carts (soundboard).
  - `edits.rs`: coalesced undo/redo support.
- **Bundle I/O** (`bundle.rs`):
  - Atomic, fsynced manifest saves with `.bak` rotation, and fallback to `.bak` when the manifest is corrupt.
  - Autosave file, and a session marker for detecting a crashed previous run.
  - Asset import that copies into `assets/<kind>/`.
  - Path-escape protection (`AssetRef::is_safe`).

**Technical implementation**

- **Cue trait:** cues implement a `Cue` trait with lifecycle hooks. They talk to engines only through a `CueContext`: `now()`, `dispatch(EngineAction)`, `media_position(cue)` and `media_finished(cue)`. That keeps cue logic free of engine types; the UI crate supplies the context.
- **Engine actions:** cues dispatch `EngineAction::{Audio, Video, Dmx, Control}`, the single command vocabulary engines consume. `ShowCommand` (27 variants) is the external command vocabulary shared by every control surface.
- **Time:** `ShowTime` is a microsecond-precision time type used everywhere.
- **Serialisation:** serde throughout. New fields use `#[serde(default)]`, so older show files keep opening.

### 2.2 `livestage_audio` — real-time audio engine

**Main responsibility.** Sample-accurate playback of audio cues, carts and click tracks to a multichannel interface. It also supplies the audio sample clock that the rest of the show can lock to.

**Key features**

- **Mixing:** multi-track cues (stems) started on the same sample frame. Per-cue gain fades, pause/resume, de-clicked in-place seeking (96-frame ramps), loops and start/end regions, and a routing matrix from stems to physical outputs (up to 64 channels).
- **Metronome:** a click track / count-in, phase-aligned to the song grid.
- **Disk streaming:**
  - Files over 15 s keep only a 3 s resident pre-roll; the rest plays from a 4 s ring per track, refilled by a streamer thread.
  - Output is sample-identical to full decode, including seeks into MP3/AAC.
  - An underrun plays silence but keeps the track aligned, and is reported in the UI.
- **Non-blocking GO:** cues whose files are still decoding or priming start later, without blocking the show thread.
- **Sample clock:** `sample_clock()` is interpolated between device callbacks, plus per-voice `VoiceClock`s for timelines and A/V lock.
- **LTC:**
  - Generator mixed into a chosen output channel *after* the master fader, so panics and fades never mute it. Anchored to the device sample clock and aligned to the timecode frame grid.
  - Reader on any input device/channel (`LtcInput`).
- **Health counters:** underruns, rejected commands, leaked garbage, DSP load, device errors, and time since the last callback (`callback_age`).

**Technical implementation**

- **Real-time rules:** the cpal callback (`Renderer::render`) never allocates, frees, locks or blocks.
  - **Commands in:** `RtCommand`s arrive through a `ringbuf` SPSC queue: Start, Stop, Seek, SeekStreams, SetGain, SetRouting, SetLtc, and others.
  - **Objects out:** finished objects go back through a second SPSC queue (`Garbage`) and are dropped on the control side. If that queue is full, they are leaked and counted, never freed on the RT thread.
- **Device ownership:** `cpal::Stream` is `!Send`, so it lives on a parked `livestage-audio-host` thread. The `AudioEngine` handle is `Send` and owned by the show runtime.
- **Clip cache:** `ClipCache` runs on a `livestage-audio-loader` thread. It loads clips, or picks streaming via `StreamedClip::open`. Decoder panics are contained per file.
- **Streaming threads:** a `Streamer` thread (`livestage-audio-stream`) services every `StreamJob`, unprimed rings first. `StreamReader` on the RT side reads the pre-roll, then the ring. A per-stream decoder panic ends only that stream.
- **Offline engine:** `AudioEngine::offline()` returns an engine plus a manually driven `Renderer`. It is used by the tests, so the real mixer is exercised without a device.

### 2.3 `livestage_video` — GPU compositor and media

**Main responsibility.** It turns `VideoAction`s into pixels on every program output. It covers compositing, projection mapping, decoding, NDI in/out, presentations, and the canvas readback that drives the LED pixel mapper.

**Key features**

- **Compositor** (`compositor.rs`): per logical output, z-ordered layers with fades.
  - **Content:** movie, still image, image sequence, text, countdown/timer overlays, solid colour, test pattern, NDI source, slides.
  - **Operations:** pause/resume/seek and slide changes with cross-fades.
  - **Blackout:** an all-outputs blackout.
- **Two-pass rendering** (`OutputRenderer`):
  - Pass A composites layers into a canvas texture at canvas resolution.
  - Pass B maps the canvas through a Bezier warp mesh, with edge blend, polygon masks and brightness, onto the window surface.
- **Video decode:**
  - Built-in: PNG/JPEG stills, image sequences, colours, a calibration pattern.
  - Movies: the optional FFmpeg backend with hardware decode. Frames arrive as NV12 and are converted in the layer shader (BT.601/709, limited/full range). Software fallback is automatic.
  - `capabilities.rs` reports movie support at startup and from `--check-media`.
- **A/V lock** (`sync.rs`): timeline video layers follow their audio master's sample clock.
  - Inside 1 ms of the master, the layer simply plays at 1×.
  - Beyond that, its speed is slewed by up to ±5 %.
  - More than 80 ms late, it snaps forward; up to 0.5 s early, it holds its frame until the audio catches up.
  - It pins to the master when the master is held, and free-runs when the master is gone.
- **NDI** (`ndi.rs`): source discovery, receiving NDI as layer content, and publishing an output as an NDI source. The SDK is loaded at runtime from the platform path.
- **Slides** (`slides/`): PDF via `hayro`. PPTX through a built-in renderer (shapes, text, images), or LibreOffice when installed (also `.ppt`/`.key`/`.odp`). Slides decode on a worker thread with neighbour prefetch.
- **Thumbnails** for the media browser.

**Technical implementation**

- **GPU context:** `GpuContext` holds one `wgpu` instance, adapter, device and queue, shared by the control window and all program windows. `Pipelines` holds the WGSL pipelines and bind-group layouts. `CANVAS_FORMAT` is non-sRGB `Rgba8Unorm`, so egui overlays and gamma-encoded video blend the same way.
- **Pluggable decoding:** through `VideoBackend` / `VideoSource` traits and a `BackendRegistry`. The FFmpeg backend runs one `livestage-ffmpeg` thread per movie, with look-ahead and seek generations. Image sequences and slides have their own worker threads.
- **Readback:** an asynchronous staging ring (`map_async`) for the pixel mapper (`ReadbackKind::PixelMap`) and NDI send (`ReadbackKind::Stream`), so the render loop never stalls on the GPU.

### 2.4 `livestage_dmx` — lighting

**Main responsibility.** Everything that ends up on a DMX universe:

- the fixture library and patch;
- cue playback with crossfades;
- the lighting console layers;
- the LED pixel mapper;
- the network protocols in and out.

**Key features**

- **Output:** bit-exact Art-Net 4 ArtDmx and E1.31 sACN data packets, sent by a paced sender (`livestage-dmx-out`, 1–44 Hz). Each universe is Art-Net, sACN multicast/unicast with priority, or disabled.
- **Fixtures:**
  - The built-in library holds four generic types: dimmer, RGB par, RGBW par, moving head.
  - **GDTF** (`.gdtf` → `description.xml`) and **Open Fixture Library** (`.json`) imports become serialisable `FixtureProfile`s stored in the show.
  - Imported attributes take LiveStage's canonical names (`intensity`, `red`, `pan`, …), so palettes and effects work on them; repeated attributes become `red 2`, `red 3`.
  - The patch is validated: overlaps, out-of-range addresses, unknown types and modes. 16-bit attributes are supported.
- **Playback:** LTP crossfades per slot; cue release and blackout.
- **Console** (`console.rs`), as layers over playback:
  - programmer;
  - palettes (intensity / colour / position / beam);
  - effects (circles, chases, dimmer waves, with rate, size and spread);
  - submasters (level, flash, inhibit);
  - grand master and blackout.

  Fixtures without an intensity channel get a virtual dimmer through their emitters.
- **Pixel mapper:** samples a region of an output canvas onto an LED matrix (RGB/GRB/BRG/RGBW; row, serpentine and column wiring).
- **sACN input** (`sacn_in.rs`):
  - A receiver thread joins the multicast group of each patched universe.
  - Sources are merged the E1.31 way: highest priority wins, HTP between equal priorities.
  - A source is dropped after 2.5 s of silence or when it sends stream-terminated.
  - Out-of-order and preview packets are discarded, and so is LiveStage's own CID.
  - Channel-range input patches map onto output universes, by HTP or override. The merge applies to what is sent, not to the engine state.

**Technical implementation**

- **Engine:** `DmxEngine::tick` runs on the show thread. It renders at a bounded `render_hz` (default 100 Hz, never below the transmit rate), and at once after a cue action or an input change.
- **Allocation-free render:** layer planes are reused, and attribute slots are resolved once when the patch is built.
- **Hand-off to the sender:** a `SharedFrame` (`Arc<Mutex<BTreeMap<u16, Universe>>>`), updated only for universes that changed.
- **Fixture definitions:** `&'static str` names keep the render path free of allocation. Names of imported profiles are interned once.

### 2.5 `livestage_control` — external control and timecode I/O

**Main responsibility.** Every way the outside world talks to LiveStage, and how LiveStage sends control and timecode out.

**Key features**

- **MIDI** (`midir`):
  - Input with learnable bindings (notes/CC/program change → `ShowCommand`); output for control cues.
  - MIDI Show Control: GO, STOP, RESUME, TIMED_GO, ALL_OFF, RESET, filtered by device ID.
- **OSC:**
  - Server for the `/livestage/...` address space (GO, cues, slides, carts, lighting, timecode).
  - Fire-and-forget sender for control cues.
- **WebSocket:** a JSON command API, plus a status broadcast every 100 ms (standby, running cues, timecode).
- **Timecode in:**
  - MTC decoder (quarter frames and full-frame locates, reverse detection).
  - Art-Net `OpTimeCode` listener on a shared port.
  - Every sample is stamped with its arrival time.
- **Timecode out:** `MtcGenerator` / `MtcOutput` send a full frame on every locate, start and stop, and each quarter frame at its boundary while rolling. LTC out lives in the audio crate.
- **Panic controller and control hub:** the `ControlHub` queue merges every source into `ControlEvent { source, command }`.

**Technical implementation**

- **Threads:** each listener owns a thread (`livestage-osc`, `livestage-ws` with a tokio runtime, `livestage-artnet-tc`) or a midir callback. Each forwards events over crossbeam channels and never touches the runtime directly.
- **Untrusted input:** OSC decoding runs inside `catch_unwind`. A randomised robustness test feeds 20 000 malformed packets to the OSC, MIDI, MTC and Art-Net parsers.

### 2.6 `livestage_ui` — the application

**Main responsibility.** The `livestage` binary. It owns the runtime, the threads, the operator interface, the program windows, and the reliability infrastructure.

**Key features**

- **Runtime** (`runtime.rs`, `ShowRuntime`): owns all engines (audio, DMX, control servers, timecode), executes `ShowCommand`s, ticks lists and carts, flushes `EngineAction`s, and keeps the operator log.
- **Operator UI** (`panels/`, egui + egui_dock):
  - Transport/command deck with standby, running, timecode and system modules.
  - Cue grid; inspector; timeline editor (drag to retime).
  - Soundboard carts with MIDI/key learn.
  - Lighting console with stage plot and tiles; DMX patch, import and input.
  - Display routing with warp/blend/mask editing; multiview.
  - Audio routing and meters; settings; control/log.
  - Undo/redo, theming, preferences, a demo-show generator and a demo rig.
- **Outputs** (`display.rs`): kiosk program windows matched to monitors by name, position and size. They are borderless, always on top, cursorless and ignore OS close requests, and are presented from a dedicated output thread.
- **Reliability** (`health.rs`, `logging.rs`):
  - Panics are contained per show tick and per output frame, and reported once per code location with a backtrace to `crash.log`.
  - A watchdog thread watches show (100 ms), output (500 ms) and UI (2 s) heartbeats and the audio callback (1 s).
  - Autosave is written from the watchdog's own thread with fsync.
  - A session marker detects unclean exits.
  - Logs rotate at 16 MB × 5, with a health summary every 10 minutes (rates, worst gaps, RSS, threads, fds).
  - SIGINT/SIGTERM quit cleanly.
- **Tooling:**
  - `snapshot.rs`: an offscreen UI snapshot harness.
  - `--check-media` and `--new` command-line modes.
  - An ignored 1 kHz isolation benchmark and an ignored soak test (`LIVESTAGE_SOAK_SECS`).

**Technical implementation**

- **Real-time priority** (`priority.rs`): the show thread asks for a real-time class (Mach time-constraint on macOS, `SCHED_FIFO` on Linux, `TIME_CRITICAL` on Windows).
- **Frame copy** (`view.rs`, `UiRuntime`): each frame, the UI copies what it shows out of the runtime. The copy is skipped when nothing changed, using `edit_generation`, `displays_rev` and `log_seq`. The UI lays out against the copy, then folds `ProjectEdits` back in under a short lock.

---

## 3. Threading, Concurrency & Real-Time Sync

### 3.1 Thread map

```
 main (winit event loop, egui UI) ──copy-in / edits-out──┐
                                                         ▼
 livestage-show (1 kHz, RT class) ── locks ──▶ Arc<parking_lot::Mutex<ShowRuntime>> ◀── livestage-watchdog (try_lock only)
     │  owns AudioEngine, DmxEngine, TimecodeEngine, control servers         ▲
     │                                                                       │ short lock per frame
     ├─ SPSC ringbuf ─▶ cpal RT callback (Renderer) ─ SPSC garbage ─┐   livestage-output (renders + presents all program windows)
     │                    ▲ streams: livestage-audio-stream          │        ▲ crossbeam: (ShowTime, VideoAction)
     │                    ▲ clips:   livestage-audio-loader          │        │
     ├─ crossbeam ─────────────────────────────────────────────────────────────┘
     ├─ SharedFrame (Mutex) ─▶ livestage-dmx-out ─▶ Art-Net / sACN
     ├─ input universes (Mutex) ◀─ livestage-sacn-in
     ◀─ ControlHub channel ◀─ livestage-osc · livestage-ws · midir callbacks
     ◀─ timecode samples  ◀─ MTC callback · livestage-artnet-tc · LTC input callback (ringbuf)
```

Supporting threads:

- **Video:** `livestage-ffmpeg` (one per movie), `livestage-imgseq`, `livestage-slides`, NDI find/receive.
- **UI side:** `livestage-thumbs-N`, `livestage-import`.
- **Reliability:** `livestage-autosave`.
- **Audio devices:** `livestage-audio-host` and `livestage-ltc-in`, which park while owning the `!Send` cpal streams.

### 3.2 Show thread (1 kHz)

**Each tick**, under the runtime lock:

1. Drain control events and execute the commands.
2. Flush engine actions, so a seek reaches the audio engine before a timeline reads its clock.
3. Feed timestamped timecode samples (MTC, Art-Net, LTC) into the lock.
4. Advance the timecode engine on the audio sample clock.
5. Send MTC out, and refresh the LTC generator anchor.
6. Let lists chase timecode, then tick lists and carts.
7. Flush actions again.
8. Run audio maintenance.
9. Tick DMX.
10. Publish WebSocket status (≤ 10 Hz).

**Scheduling:**

- Pacing is absolute (`next += period`). The thread resynchronises instead of bursting after more than 50 missed periods, for example after machine sleep.
- `ShowStats` records the worst lateness, lock wait and tick time.
- Each tick runs inside `contain()`: a panic loses one tick, not the show.

### 3.3 Audio ⇄ show

**From show to audio:** the show thread never blocks the device, and the device never waits for the show.

- Commands are pushed to the SPSC queue. If the queue is full, the command is dropped and logged.
- Large objects (voice groups, routing tables, primed streams) are built off the RT thread and moved in whole.

**From audio to show:**

- `EngineShared` atomics carry frames rendered, callback timestamps, peaks and load.
- Per-voice `VoiceShared` atomics carry position, state and seek holds.

**The clock:** `sample_clock()` interpolates between callbacks, so timelines are not quantised to the buffer size. This clock is also the internal timecode base and the master for A/V lock.

### 3.4 Video ⇄ show

- **Actions:** `VideoAction`s travel over a crossbeam channel, stamped with the show time they were dispatched at, and are applied at that time on the output thread.
- **Per frame:** the output thread takes the runtime lock briefly. It copies displays only when `displays_rev` changed, reads the pixel-map list, and looks up the master `VoiceClock`s.
- **Rendering:** it renders and presents every program window, then feeds pixel-map readbacks back into the DMX engine under a second short lock.
- **Timing:** when presenting, frames are paced by vsync; otherwise by a 60 Hz idle loop.
- **Finished media:** reported back through an `Arc<Mutex<HashSet<CueId>>>`.

### 3.5 UI ⇄ show

- **Never under the lock:** the UI never runs egui layout, tessellation or presentation while holding the lock.
- **Each frame:**
  - it copies runtime state in (`sync`);
  - lays out against the copy;
  - applies the collected edits back (`apply`);
  - runs deferred operations (`defer`) inside that short critical section.
- **Measured isolation:** an ignored benchmark measured 1 000-cue shows with an 80 ms layout. The show thread's worst lock wait stayed under 5 ms, compared with more than 15 ms for the old layout-under-lock design.

### 3.6 Watchdog

The `livestage-watchdog` thread polls every 10 ms. It never waits on the runtime lock (`try_lock_for(2 ms)`).

- **Heartbeats:** it watches the show, output and UI heartbeats.
- **Audio:** every 250 ms it checks audio callback age and device errors.
- **Autosave:** it snapshots dirty shows for autosave, written by the `livestage-autosave` thread.
- **Operator log:** stall, recovery, fault and device events are queued and delivered when the lock is free.

---

## 4. Current Limitations & Known Technical Debt

### 4.1 Corrections to assumptions in the brief

| Assumption | Actual state |
|---|---|
| *"Input-only timecode"* | Fixed in `f7c8967`: MTC out (show thread) and LTC out (audio callback) send the show timecode, so LiveStage can be the master clock. **Art-Net timecode output is still missing** (an encoder exists, `encode_art_timecode`, but there is no sender). |
| *"Single vs multi-display kiosk"* | Multi-display kiosk exists: every enabled logical display opens its own fullscreen window on its mapped monitor. The real limit is that **one output thread renders and presents every window in turn**: see 4.2. |
| *"Missing fixture libraries"* | Partly fixed. GDTF and OFL import exist, but **no fixture library is bundled**: the built-in set is four generic types, and each real fixture has to be imported from a file. |

### 4.2 Video and GPU

- **No zero-copy video.**
  - **Decode:** hardware-decoded frames are copied to the CPU (`av_hwframe_transfer_data`), then uploaded with `queue.write_texture` every frame. This costs roughly 3 MB per 1080p NV12 frame per layer, plus a CPU copy. A VideoToolbox/IOSurface → Metal (or D3D11 / DMA-BUF) interop path is not implemented.
  - **NDI send:** each NDI frame is a GPU → CPU readback.
- **Serial presentation.**
  - The output thread loops over all windows on one thread with `AutoVsync`, so several windows can serialise on each other's vsync. Above about 2–3 heads at 60 Hz, or with monitors at different refresh rates, frame pacing is not guaranteed.
  - There is no genlock/framelock and no per-window present thread.
- **Movies are optional and silent.**
  - Movie playback needs the optional `ffmpeg` feature and a system FFmpeg 9.x. A default build plays stills, sequences, NDI and slides only.
  - The FFmpeg backend decodes **video only**: movie sound tracks are not played. Sound for a movie must be a separate audio cue, which a timeline then locks the video to.
- **NDI:** no NDI audio and no tally.
- **Video output devices:** there is no DeckLink/SDI or Syphon/Spout output.

### 4.3 Audio

- **Output:** one output device per show. There is no per-cue effects chain (EQ, compression), no VST/AU plugins, and no explicit ASIO selection on Windows (it uses cpal's default host).
- **Input:** the only audio input is LTC. There is no live-input routing or monitoring.
- **No automatic device recovery:** the watchdog detects a stalled or erroring device and raises an alarm, but the operator must press *Restart audio*.

### 4.4 Lighting

- **Protocols:**
  - No RDM, no Art-Net input, and no ArtPoll / sACN universe discovery.
  - No sACN synchronisation packets, so sources on several universes are not frame-synchronised.
- **Import limits:**
  - GDTF: channels on a second DMX break are dropped, and channel functions/sets (wheel slot names, ranges) are not used.
  - OFL: matrix modes are skipped.
- **Console depth:** the console is an operator-friendly subset. There are no fixture groups as objects, no presets per-fixture-type beyond the palette model, no tracking cue stack, and no MA/Eos-style command line.
- **Monitoring:** the universe view shows the engine output *before* the sACN input merge.

### 4.5 Control and networking

- **Network control is locked down** (added after this report's first version; it replaces an earlier note about an unmerged lock-down branch, which in fact held no such work).
  - **Binding:** the OSC and WebSocket servers listen on 127.0.0.1 unless the show enables external access.
  - **WebSocket:** needs an HMAC-SHA256 session token in its handshake.
  - **OSC:** from the network it is accepted only from an IP allowlist, and it never runs sensitive commands (panic, stop all, masters, blackout, submasters, output and timecode control).
  - **Remaining:** tokens travel in clear over `ws://`, so TLS and a VPN are the operator's job, and the WebSocket API has no per-command permissions: any valid token can run everything.
- **Missing protocols:** no OSC query/feedback beyond the WebSocket status, and no PSN/RTTrP tracking.

### 4.6 Platform and process

- **Platforms:**
  - Only macOS (Metal, CoreAudio) is developed and tested.
  - Windows and Linux have code paths (real-time priority, NDI paths, ALSA/WASAPI through cpal) but **have never been run**.
  - There is no installer or code-signed bundle; FFmpeg bundling has a script, but it is not part of a release pipeline.
- **Tests:**
  - Test density is uneven: `livestage_ui` has about 12 k lines and only 14 tests. Panels are checked by an offscreen snapshot harness, not assertions.
  - There is no CI configuration in the repository.
- **Not yet validated:**
  - **Soak:** the 8-hour soak test exists but has not been run to completion. The one long run was stopped after about 250 s; RSS, threads and fds were flat until then.
  - **Hardware:** LTC in/out and MTC out are verified in code (encoder ↔ decoder and generator ↔ decoder round-trips) but **not yet against real hardware**.
- **Dependencies:** pinned to an older, mutually compatible set (wgpu 24, egui 0.31, winit 0.30, cpal 0.15, symphonia 0.5). Upgrades must move wgpu, egui and winit together. A transitive crate (`block` 0.1.6) is flagged for future-Rust incompatibility.
- **Repository hygiene:** `shows/Demo Show.livestage/project.json` is tracked while the `shows/` folder is git-ignored, and it currently carries uncommitted local edits.

---

## 5. Project Maturity Rating

| Area | Rating | Notes |
|---|---|---|
| Core show logic (cues, lists, timelines, undo) | ●●●●○ | Well-modelled, heavily unit-tested, deterministic |
| Real-time architecture (show thread, audio RT rules, UI isolation) | ●●●●○ | Sound design, measured; RT discipline is strict and tested |
| Audio playback | ●●●●○ | Sample-accurate, streaming, seek-exact; lacks device auto-recovery and FX |
| Video / projection | ●●●○○ | Feature-rich, but no zero-copy, serial multi-head presentation, and silent movies |
| Lighting | ●●●○○ | Solid engine and console layers; protocol coverage and library depth are behind a real console |
| Timecode | ●●●●○ | Complete in and out; hardware validation pending |
| Control and remote | ●●●○○ | Broad protocol coverage; loopback by default, token-authenticated WebSocket, OSC allowlist and command filter; no TLS |
| Reliability (watchdog, crash recovery, logging) | ●●●●○ | Unusually thorough for the project's age; long soak not yet completed |
| Platform reach and release engineering | ●○○○○ | macOS only in practice; no CI, installer or signing |

**Overall: advanced beta (pre-production).**

The architecture is production-grade in its essentials:

- a real-time show thread isolated from the UI;
- lock-free audio with strict real-time rules;
- sample-clock A/V lock;
- contained faults, watchdogs and crash recovery.

It is also well tested at the core. LiveStage is **fit for rehearsals, installations and low-risk shows on a tested macOS machine**, with a show file that has been run end to end.

Before high-stakes production use, close these gaps in this order:

1. **Validate the run:** complete the 8-hour soak and a hardware run-through (LTC/MTC with a real console or DAW, multi-head output on the target GPU).
2. **Movie audio:** add a movie-audio path, or document the separate-audio-cue workflow prominently.
3. **Multi-head output:** move to per-window presentation, or validate it on the actual number of heads.
4. **Process:** add CI and a signed release build. Treat Windows and Linux as unsupported until they have been run.
