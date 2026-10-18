# LiveStage

**Live performance and show-control software, written entirely in Rust.**
One operator surface for audio playback with stems and click, video and projection mapping,
presentations (PDF / PowerPoint), show timers and teleprompter, DMX lighting, and external control
(MIDI/MSC, OSC, WebSocket, NDI) on macOS, Windows and Linux.

```bash
cargo run --release                        # opens ./shows/Demo Show.livestage (created on first run)
```

---

## Contents

1. [Feature highlights](#feature-highlights)
2. [Architecture](#architecture)
3. [Building](#building)
4. [UI and controls guide](#ui-and-controls-guide)
5. [Show files (`.livestage` bundles)](#show-files-livestage-bundles)
6. [Remote control API](#remote-control-api)
7. [Engine details](#engine-details)
8. [Testing](#testing)
9. [Known gaps](#known-gaps)

---

## Feature highlights

| Area | What it does |
|---|---|
| **1 kHz real-time show thread** | Cue lists tick on a dedicated thread at 1 kHz in a real-time scheduling class (Mach time-constraint on macOS, `SCHED_FIFO` on Linux, `TIME_CRITICAL` on Windows). The UI never blocks a cue; stacked timeline children start within ~0.5–1.6 ms of their mark. |
| **Lock-free audio engine** | `cpal` output with a real-time callback that never allocates, frees or locks. Commands cross a single-producer/single-consumer ring buffer. Any format Symphonia decodes (WAV, AIFF, FLAC, MP3, AAC/M4A, OGG, ALAC). Stems patched to physical outputs, sample-synchronous multitrack, click track with count-in. |
| **`wgpu` video pipeline** | Layer compositor → canvas → Bezier warp mesh with **edge blending**, **polygon masks** and brightness → each output. Movie files with hardware decode (VideoToolbox / D3D11VA / VAAPI) and YUV→RGB on the GPU, stills, image sequences, timers, text/teleprompter. |
| **Multi-display kiosk outputs** | Name logical outputs ("Main LED Wall", "Confidence Monitor"), map them to physical screens, route any content anywhere. Program windows are borderless fullscreen, cursorless, always-on-top and ignore OS close requests. They are rendered and presented on their own **output thread**, so they keep running when the operator window is covered, minimised or dragged. |
| **Presentations** | Import **PDF** (pure-Rust `hayro` rasteriser) and **PowerPoint** (`.pptx` built-in renderer, or LibreOffice for full fidelity and `.ppt` / `.key` / `.odp`). Slides render once, in the background, at the largest output's resolution; slide text and speaker notes are kept. Slide cues route to any output with Next / Previous / Go-to and cross-fades. |
| **Live scrubbing** | Click or drag the playhead of any running cue (cue grid, inspector, multiview) to seek **while it plays**: audio jumps in place with a 2 ms de-click dip, video re-times its decoder, timers and teleprompters jump, timelines re-chase their children. |
| **Multiview / control booth** | Configurable monitor wall (presets, custom rows × columns, a featured 2 × 2 cell). Any cell shows an output's live canvas, the program cue with clocks and a scrub bar, the next cue with a content preview (poster frame, image, first slide, text), the running presentation's current and next slide with notes, or a clock. |
| **NDI streaming** | Receive any NDI source as a layer; publish any output's program feed at its own resolution (GPU-scaled, triple-buffered async readback). The NDI runtime is loaded at run time, never linked. |
| **DMX** | Bit-exact Art-Net 4 and sACN (E1.31) output, fixture library with a validated patch (16-bit attributes), timed crossfades, LED pixel mapper fed from output canvases. |
| **Control** | MIDI notes/CC/program change, MIDI Show Control, OSC in/out, WebSocket JSON API with live status push, two-stage panic (soft fade, then a hard cut on the second press). |
| **Timecode (SMPTE / MTC / Art-Net)** | 24, 25, 29.97 drop-frame, 29.97 and 30 fps with exact frame ↔ time math. Chase incoming **MIDI Time Code** or **Art-Net timecode** through a lock with drift compensation, freewheel and jump detection, or run the **internal generator** on the audio sample clock. Timeline cues can be **timecode slaves**, and whole cue lists can fire cues on timecode triggers, chasing jumps and pausing when timecode stops. |
| **Editing safety** | Coalesced undo/redo that keeps running cues controllable, 30 s background autosave with crash recovery, multiple cue lists, drag-to-retime timeline editor. |
| **Operator comfort** | Settings window (⌘,) for audio device / buffer / sample rate, control ports, output behaviour, accent colour, UI size and tool paths; macOS-style context menus with shortcut hints on cues and multiview cells. |

---

## Architecture

LiveStage is a Cargo workspace of six crates. Only `livestage_core` is shared by everything, and it
has no hardware dependencies.

```
                          ┌──────────────────────┐
                          │   livestage_ui (bin) │  winit · egui · egui_dock
                          │  operator app + show │  show thread · kiosk outputs
                          └──────────┬───────────┘
          ┌──────────────┬───────────┼────────────┬────────────────┐
          ▼              ▼           ▼            ▼                ▼
 ┌────────────────┐ ┌──────────┐ ┌──────────┐ ┌────────────┐ ┌──────────────────┐
 │livestage_audio │ │ _video   │ │  _dmx    │ │ _control   │ │                  │
 │cpal · symphonia│ │wgpu · NDI│ │Art-Net / │ │MIDI · OSC  │ │                  │
 │SPSC ring buffer│ │ffmpeg opt│ │sACN · fx │ │WebSocket   │ │                  │
 └───────┬────────┘ └────┬─────┘ └────┬─────┘ └─────┬──────┘ │                  │
         └───────────────┴────────────┴─────────────┴────────▶  livestage_core  │
                                                             │ cues · timelines │
                                                             │ cue lists · model│
                                                             │ .livestage I/O   │
                                                             └──────────────────┘
```

| Crate | Responsibility |
|---|---|
| `livestage_core` | The `Cue` trait and its lifecycle state machine (`CueExt`, including in-place seek); cue types Audio, Video, Timer, Slides, DMX, Control, Timeline; multiview layout model; stacked timelines slaved to a master media clock; `CueList` runner (standby, auto-continue/follow, panic); routing model; projection-mapping math; `.livestage` bundle I/O with schema migration, autosave and undo support. |
| `livestage_audio` | Decoding, resampling, stem → output patch bay, click generator, real-time renderer and device host. |
| `livestage_video` | Compositor, per-output renderer (warp/blend/mask), GPU readback, video backends (builtin + optional FFmpeg with hardware decode), NDI send/receive, presentation import (PDF / PPTX → slides) and slide playback, thumbnails. |
| `livestage_dmx` | Protocol encoders, paced UDP sender, fixtures and patch, crossfade playback, pixel mapper. |
| `livestage_control` | OSC, MIDI/MSC, WebSocket remote API, `ControlHub` command funnel, panic escalation. |
| `livestage_ui` | The `livestage` binary: control window, dockable panels, output render thread, show thread, settings and preferences, undo, snapshot tool. |

### Data flow

```
 MIDI / MSC ─┐                         ┌─ AudioAction ──▶ AudioEngine ══SPSC══▶ audio callback (RT)
 OSC ────────┤                         │
 WebSocket ──┼─▶ ControlHub ─▶ ShowCommand ─▶ CueList ─EngineAction─┼─ VideoAction ──▶ output thread: Compositor ─▶ outputs / NDI
 Keyboard/UI ┘        ▲                │                            ├─ DmxAction ────▶ DmxEngine ─▶ Art-Net / sACN
                      │          show thread (1 kHz)                └─ ControlAction ─▶ OSC / MIDI out / re-queue
                      └──── cue-triggered commands      ▲
                                                        └── media clocks (audio sample counter) via CueContext
```

Cues never touch hardware. They emit `EngineAction`s through a `CueContext`, which also supplies
the engine clocks that timeline children follow. That keeps the cue engine deterministic and
unit-testable (see `MockCtx` in `crates/livestage_core/src/cue.rs`).

### Threads

| Thread | Owns | Notes |
|---|---|---|
| main | winit event loop, the operator window (egui), window creation | Locks the runtime only while laying out panels; its own vsync never delays outputs. |
| `livestage-output` | compositor, output canvases, program-window swapchains, NDI senders, pixel-map readback | Presents every program window independently of the operator window (paced by the outputs' vsync, 60 Hz when none is open). |
| `livestage-show` | show runtime: cue lists, audio/DMX/control engines | 1 kHz, real-time class; health shown in the status bar. |
| `livestage-audio-host` | the `cpal` stream (`!Send`) | Parks until shutdown. |
| audio callback | real-time renderer | No allocation, locking or I/O. |
| decoder / slide loader / thumbnail / import / NDI / DMX / OSC / WS / Art-Net timecode workers | their own I/O | Communicate through channels and atomics. Timecode samples are time-stamped where they arrive (MIDI callback, UDP thread). |

---

## Building

### Requirements

- **Rust** 1.92 or newer (stable).
- **macOS** 12+ (Metal): developed and tested here. **Windows** 10+ (DX12/Vulkan) and **Linux**
  (Vulkan, ALSA: `libasound2-dev`, `libudev-dev`) are supported by every dependency and have
  platform code paths, but are not yet tested.
- Optional: **FFmpeg 9.x** development libraries for movie playback; the **NDI runtime** for NDI;
  **LibreOffice** for full-fidelity PowerPoint import and for `.ppt` / `.key` / `.odp`.

### Commands

```bash
# Release build (recommended for shows: thin LTO, single codegen unit)
cargo build --release
./target/release/livestage                              # demo show
./target/release/livestage "path/My Show.livestage"     # open a show
./target/release/livestage --new ~/Shows "Tour 2027"    # create an empty show

# With movie-file playback (FFmpeg 9.x via pkg-config)
cargo run --release --features ffmpeg
# FFmpeg installed in a custom prefix:
PKG_CONFIG_PATH=$HOME/.local/ffmpeg-9/lib/pkgconfig cargo run --release --features ffmpeg
```

| Feature / env | Effect |
|---|---|
| `--features ffmpeg` | Adds the FFmpeg backend for `.mov .mp4 .m4v .mkv .webm .avi .mxf .mpg .ts .m2ts .wmv .flv .ogv .dv .nut`. Without it, video cues play stills and PNG/JPEG image sequences, and movie files show an on-screen notice. Requires FFmpeg **9.x** headers and libraries (`ffmpeg-next` 9). |
| `LIVESTAGE_HWDEC=0` | Forces software video decode (hardware decode is otherwise used when available; also a Settings switch). |
| `LIVESTAGE_SOFFICE=/path/soffice` | LibreOffice binary for presentation import (also a Settings field; otherwise `PATH` and standard install locations are searched). |
| `RUST_LOG=info` | Logging (`tracing`); e.g. `RUST_LOG=info,livestage_core=debug` to log every cue start. |

### NDI

No build step is needed. LiveStage looks for the NDI runtime at run time:
`NDI_RUNTIME_DIR_V6` / `_V5`, then the platform default (`/usr/local/lib/libndi.dylib` on macOS,
installed by NDI Tools; `Processing.NDI.Lib.x64.dll` on Windows; `libndi.so.6` on Linux). Without
it, NDI features show "NDI runtime not installed" and everything else works.

```bash
cargo run -p livestage_video --example ndi_probe            # list NDI sources on the network
cargo run -p livestage_video --example ndi_probe -- "Name"  # receive one and report a frame
```

### Presentations

```bash
cargo run -p livestage_video --example render_deck -- talk.pptx /tmp/slides 1920x1080   # preview an import
```

| Format | Renderer |
|---|---|
| `.pdf` | Built in (`hayro`, pure Rust): vector pages rasterised at output resolution, text extracted for previews. |
| `.pptx` / `.ppsx` / `.potx` | LibreOffice when found (full fidelity), else the built-in renderer: backgrounds, filled shapes, pictures, text with wrapping, bullets, alignment and autofit, master/layout placeholder styles, groups, theme colours. Speaker notes are always read from the file. Charts, SmartArt, tables, gradients and effects need LibreOffice. |
| `.ppt` / `.key` / `.odp` | LibreOffice only (or export to PDF first). |

---

## UI and controls guide

### Window layout

```
┌──────────────────────────────────────────────────────────────────────────────────────────────┐
│ LiveStage  File  Edit  View  Show │ Demo Show                                        menu bar│
├────────────┬──────────────────────────────┬────────┬─────────┬──────────────┬──────────┬───────┤
│            │ NEXT UP                      │        │         │ RUNNING·1    │ TIME OF  │OUTPUTS│
│     GO     │ 5  Teleprompter: speech      │ STOP   │  PANIC  │ -00:20.81    │ DAY      │ LIVE  │
│            │ [Prev][Next] [Pause][Resume] │ ALL    │         │ 4 Song 1     │ 12:29:14 │MASTER │
├────────────┴──────────────────────────────┴────────┴─────────┴───┬──────────┴──────────┴───────┤
│ Cue List                                                         │ Inspector                   │
│ [Main Cue List] [Cue List 2] [+ List]                            │ Timeline cue                │
│ [+ Add cue] [Move up] [Move down] [Delete]                       │ Number   4                  │
│ ┌──────────────────────────────────────────────────────────────┐ │ Name     Song 1 — Timeline  │
│ │ cue grid (see below)                                         │ │ Pre-wait 0.00 s   Follow …  │
│ └──────────────────────────────────────────────────────────────┘ │ type-specific settings …    │
├──────────────────────────────────────────────────────────────────┴─────────────────────────────┤
│ Timeline │ Multiview │ Display Routing │ Audio Patch │ DMX │ Control & Log          bottom tabs│
│                                                                                                │
├────────────────────────────────────────────────────────────────────────────────────────────────┤
│ Audio · 48000 Hz · 2 ch │ Show 1000 Hz · late ≤0.4 ms │ Outputs 60 fps │ DMX │ OSC · WS     │
└────────────────────────────────────────────────────────────────────────────────────────────────┘
```

- The **transport bar** is laid out in fixed regions. On narrow windows the clock and output regions
  drop out (they stay reachable from the *Show* menu) instead of overlapping.
- Panels are dockable: drag tabs to rearrange, and use **View ▸ Reset layout** to restore the default.
- Drop media files or presentations onto the window (or use **File ▸ Import…**, ⌘I). They are
  copied into the show and a matching cue is created; presentations render in the background with a
  progress indicator in the menu bar.
- **Settings** (⌘, / Ctrl+,, or **LiveStage ▸ Settings…**): *General* (accent colour, interface size,
  quit confirmation, autosave interval), *Audio* (device, sample rate, buffer size with latency, show
  outputs; saved in the show), *Control* (OSC / WebSocket ports, MIDI input, MSC device id; saved in
  the show), *Outputs & Video* (open outputs on launch, always-on-top, hide cursor, hardware decode,
  NDI runtime folder), *Files & Paths* (startup show, media folder, LibreOffice). Machine settings are
  stored in `~/Library/Application Support/LiveStage/preferences.json` (`%APPDATA%\LiveStage`,
  `~/.config/livestage` on other systems).

### Cue grid

```
 #      TYPE      NAME                      PRE-WAIT     ACTION                 POST-WAIT  FOLLOW    STATE
 1      Timer     Doors                                  05:00.00                                    
▌2      Video     Line-up test pattern                   00:04.21 / ∞                                PLAY
▌3      DMX       Preset: house half                     00:03.00                                    STANDBY
 4      Timeline  Song 1 — Timeline                      ███░░░ 00:03.19 / 00:24.00                  PLAY
   ↳ 4.0 Audio      Backing track           @ 00:00.00   ███░░░ 00:03.19 / 00:24.00                  PLAY
   ↳ 4.1 DMX        Verse look              @ 00:00.00   00:01.00 / 00:01.00                         DONE
   ↳ 4.2 Video      Lyric: verse 1          @ 00:04.00   00:06.00
 5      Video     Teleprompter: speech                   ∞                                 Continue
▌8      Slides    Keynote                                Slide 2 / 12                                PLAY
```

| Colour | Meaning |
|---|---|
| Amber `#FFC107` stripe and row | **Standby**: the next GO fires it |
| Leaf green `#4CAF50` | **Active / playing**, with a progress fill in the Action column |
| Emerald `#00C853` outline | Selected row (shown in the Inspector) |
| Crimson `#E53935` | **Stopped / panic** |

Timeline children are indented under their parent; their *Pre-wait* column shows the offset
(`@ 00:04.00`) on the parent's clock. Double-click a row to make it the next cue.

- **Scrub while playing**: the progress bar of a running cue is a scrub bar. Click or drag it to
  seek; hover shows the target time.
- **Right-click a row** for a context menu: GO now, set as next, pause / resume, restart from top,
  skip to the last 10 s, next / previous slide, stop, arm / disarm, duplicate (⌘D), move, copy cue
  number, delete (⌘⌫).

### Multiview (control-room monitor wall)

```
 Layout  1×1  1×2  2×2  2×3 [3×3] 4×4  Featured+5  Featured+7 │ Custom [3 rows][3 cols] □ featured │ Auto
┌──────────────────────────────┐ ┌──────────────────────────────┐ ┌──────────────────────────────┐
│      live output canvas      │ │      live output canvas      │ │      live output canvas      │
├──────────────────────────────┤ ├──────────────────────────────┤ ├──────────────────────────────┤
│ Main LED Wall    LIVE  NDI 1 │ │ Confidence Monitor  1920×1080│ │ Output 3            1920×1080│
└──────────────────────────────┘ └──────────────────────────────┘ └──────────────────────────────┘
┌──────────────────────────────┐ ┌──────────────────────────────┐ ┌──────────────────────────────┐
│ ┌─────┐ 8  Keynote           │ │ ┌─────┐ 2  Line-up test      │ │ ┌───┐  ┌──────────┐          │
│ │ pic │ 00:00:12.40   -01:05 │ │ │thumb│    pattern           │ │ │now│  │   next   │          │
│ └─────┘ ══════●═══════════   │ │ └─────┘  Test pattern        │ │ └───┘  └──────────┘          │
├──────────────────────────────┤ ├──────────────────────────────┤ │ NOW 2/12   NEXT 3/12         │
│▌PROGRAM        SLIDES  PLAY  │ │▌NEXT              VIDEO      │ │ next slide text · Notes: …   │
└──────────────────────────────┘ └──────────────────────────────┘ ├──────────────────────────────┤
                                                                  │▌SLIDES · Keynote   RUNNING   │
                                                                  └──────────────────────────────┘
```

- **Layouts**: presets, any custom *rows × columns* up to 6 × 6, and *featured* (the first cell spans
  2 × 2). *Auto* shows every output followed by Program, Next, Presentation and Clock. The layout is
  saved with the show.
- **Right-click a cell** to assign its feed: any output, **Program** (running cue with clocks and a
  scrub bar), **Next** (standby cue with a preview: movie poster frame, image, first slide, text or a
  summary), **Presentation** (current and upcoming slide, upcoming slide text and speaker notes), or
  **Clock**.
- The caption's left edge is a tally: crimson for live outputs, green for program, amber for next.

### Audio patch matrix

```
 Output meters   ▮▮ ▯▯ ▯▯ ▯▯ ▯▯ ▯▯
                 1  2  3  4  5  6  7  8

 Stems           Name             Ch   Gain            Mute  Bus shortcut
                 Backing Tracks   2    ───●── 0.0 dB   M     [Patch to bus…]
                 Click            1    ───●── 0.0 dB   M     [Patch to bus…]
                 In-Ear Mix       2    ───●── 0.0 dB   M     [Patch to bus…]

 Patch matrix              1    2    3    4    5    6    7    8      ← physical outputs
   Backing Tracks L       [■]  [ ]  [ ]  [ ]  [ ]  [ ]  [ ]  [ ]
   Backing Tracks R       [ ]  [■]  [ ]  [ ]  [ ]  [ ]  [ ]  [ ]
   Click                  [ ]  [ ]  [■]  [■]  [ ]  [ ]  [ ]  [ ]
   In-Ear Mix L           [ ]  [ ]  [ ]  [ ]  [■]  [ ]  [■]  [ ]
   In-Ear Mix R           [ ]  [ ]  [ ]  [ ]  [ ]  [■]  [ ]  [■]
```

Each cell is a crosspoint (stem source channel → physical output), shown green when patched.
Outputs the current device doesn't have are dimmed; crosspoints to them are ignored and listed once
in the log. The default patch is *FOH Main* on 1-2, *Click* on 3-4 and *In-Ears* on 5-8. Changes
apply live, without dropouts.

### Other panels

- **Inspector**: context-aware settings for the selected cue: audio tracks/stems/fades/click, video
  source and output, timer mode and colours, text and teleprompter (including scroll speed and
  mirroring), DMX channels and fade, OSC/MIDI actions, timeline items. Running cues get a live
  playhead you can drag to seek. Slide cues show a clickable thumbnail strip (jump while running, or
  pick the first slide), Previous / Next, transition time and the speaker notes.
- **Timeline**: lanes for the master clock and every stacked item. Drag to re-time, with snapping to
  0.1 s or to the click track's beats, and a live playhead.
- **Display Routing**: name outputs, map them to screens, set canvas size, kiosk mode, NDI output and
  its resolution, test pattern, and projection mapping (draggable Bezier warp grid, edge blend per
  side, polygon masks, brightness).
- **DMX**: universes (sACN / Art-Net / off), fixture patch with overlap checks, live universe view.
- **Control & Log**: OSC/WebSocket/MIDI status, MIDI port selection, and the event log.

### Timecode

The **TIMECODE** box in the transport shows the show timecode, its source (INT / MTC / ART) and the
lock state (green LOCKED, amber LOCKING / FREEWHEEL). Click it to open **Settings → Timecode**:

- **Source**: *Internal* (generator with Start / Stop / Reset / Locate, a frame rate, a start
  timecode, and a clock: the audio sample clock (default) or the system clock), *MIDI Time Code*
  (choose the MIDI input), or *Art-Net Timecode* (UDP port, shared with lighting software on the same
  machine). External sources show measured drift (ppm) and jitter.
- **Freewheel / jump threshold**: how long to roll through dropouts, and how big an error counts as a
  jump.
- **Cue lists in timecode mode**: tick a list, then give cues a **Timecode** trigger in the
  Inspector. They fire when timecode reaches the trigger. A jump into the middle of a cue's fade or
  media starts it part-way through. What happens when timecode stops is set per list (pause, stop,
  or keep playing). Triggers appear in the cue grid's pre-wait column.
- **Timeline slaves**: in the Inspector, tick **Timecode slave** on a timeline cue and set **Starts
  at**. The timeline runs whenever timecode is inside it, follows jumps and locates, re-syncs beyond
  the tolerance, and pauses, stops or keeps playing when timecode stops.

Labels are compared at the incoming rate, so a trigger at `01:00:00:00` works with any frame rate.

### Keyboard

| Key | Action |
|---|---|
| **Space** | GO (also works when an output window has focus) |
| **Esc** | PANIC: soft fade; a second press within 1.5 s is a hard cut plus DMX blackout |
| **↑ / ↓** | Previous / next cue to standby |
| **→ / ←**, **PgDn / PgUp** | Next / previous slide of the running presentation (presentation clickers send these) |
| **P / R** | Pause all / resume all |
| **⌘/Ctrl S** | Save |
| **⌘/Ctrl I** | Import media or presentation |
| **⌘/Ctrl D** | Duplicate the selected cue |
| **⌘/Ctrl ⌫** | Delete the selected cue |
| **⌘/Ctrl Z**, **⇧⌘/Ctrl Z** | Undo / redo |
| **⌘/Ctrl ,** | Settings |

---

## Show files (`.livestage` bundles)

```
My Show.livestage/
├── project.json              full show: cue lists, displays, audio patch, DMX, control, multiview (schema v2)
├── project.json.bak          previous manifest, rotated on every atomic save
├── project.autosave.json     crash-recovery copy (only while there are unsaved changes)
└── assets/
    ├── audio/   video/   images/   data/     imported media, copied in (originals never moved)
    ├── documents/                            imported PDF / PowerPoint originals
    └── slides/<deck>/001.png …               slides rendered from them at import
```

- Saves are atomic (temp file, fsync, rename). Older schemas are migrated on open.
- Asset paths are bundle-relative and checked against path traversal. Missing assets are reported on
  open and shown on-screen instead of stopping the show.
- This repository tracks show **manifests** and the asset folder layout, but not media (see
  `.gitignore`). The demo show's backing track is generated; delete `shows/Demo Show.livestage` to
  regenerate the demo with its audio.

---

## Remote control API

**OSC** (UDP 53100; QLab-style unprefixed aliases accepted):

| Address | Args | Action |
|---|---|---|
| `/livestage/go` | | GO |
| `/livestage/go/{n}`, `/livestage/cue/{n}/go` | | GO cue *n* |
| `/livestage/stop` | `[fade s]` | Stop all |
| `/livestage/panic` | `[1 = hard]` | Panic |
| `/livestage/pause`, `/livestage/resume` | | Pause / resume all |
| `/livestage/standby/next` · `previous` · `{n}` | | Move standby |
| `/livestage/master` | `dB` | Master level |
| `/livestage/cue/{n}/seek`, `/livestage/seek/{n}` | `seconds` | Seek cue *n* (in place if it is playing) |
| `/livestage/slide/next` · `previous` | | Step the running presentation |
| `/livestage/slide` | `slide number` | Go to a slide (1-based) |
| `/livestage/outputs` | `[1 / 0]` | Open / close program outputs |
| `/livestage/timecode/start` · `stop` | | Internal timecode generator transport |
| `/livestage/timecode/locate` | `"HH:MM:SS:FF"` | Move the internal generator |

**WebSocket** (port 8765): send JSON commands such as `{"cmd":"go"}`, `{"cmd":"go_cue","number":"12"}`
`{"cmd":"panic","hard":false}`, `{"cmd":"seek_cue","number":"7","position":95000000}` (µs) or
`{"cmd":"next_slide"}`, `{"cmd":"timecode_locate","timecode":"01:00:00:00@25"}`. Each one is answered with `{"type":"ack"}`, and connected clients
receive a status document (standby cue, running cues with elapsed/remaining, timecode position and
lock state) at up to 10 Hz.

**MIDI**: note 60/61/62 on channel 1 = GO / STOP / PANIC by default. MIDI Show Control GO (with cue
number), STOP, RESUME and ALL_OFF are supported. MIDI Time Code is read from its own port (see
[Timecode](#timecode)).

---

## Engine details

- **Audio real-time rules.** The audio callback never allocates, frees, locks or blocks. Voices, clips
  (`Arc`) and routing tables are built on the control thread and moved in through an SPSC ring buffer.
  Finished objects travel back through a second ring buffer and are freed off the audio thread. The
  sample clock is interpolated between device callbacks, so timeline children are not quantised to the
  buffer size.
- **Timelines.** A master cue (usually audio) acts as the clock. Starting mid-way *chases*: media
  starts part-way through, past DMX looks are re-applied, and past triggers are not re-fired.
- **Video.** Hardware-decoded frames arrive as NV12 and are converted by the layer shader (BT.601/709,
  limited/full range from the stream's tags). Streams the hardware can't decode fall back to software
  automatically.
- **Seeking.** `CueExt::seek` moves a running or paused cue in place through `Cue::on_seek`; cues
  that cannot seek in place (DMX fades) restart from the new position. The audio renderer ramps to
  silence over 96 frames, moves the read position (re-phasing the click to the song grid) and ramps
  back, and the cue's sample clock follows immediately.
- **Outputs.** Displays are matched by name, position and size, so mappings survive reboots. Kiosk
  windows are placed in the target monitor's own logical coordinates (correct on mixed-DPI setups)
  and verified on that screen before going fullscreen; they re-hide the cursor and stay on top when
  focus or the cursor returns. Swapchains are configured on the main thread and presented from the
  output thread.
- **Presentations.** Slides decode on a worker thread with the current slide's neighbours
  prefetched; a slide change cross-fades from a snapshot of the previous slide.
- **Timecode lock.** Samples are stamped on arrival (in the MIDI / UDP thread) and fed to a
  phase + frequency loop on the show thread. Small errors are absorbed gradually (no audible or
  visible steps) while the loop measures the sender's real speed, so positions between samples stay
  accurate even when the sender's clock is ±0.1 % off. Errors beyond the jump threshold (default
  4 frames) re-locate at once. After a dropout the lock freewheels (default 200 ms) and then reports
  a stop. MTC delivers a sample every quarter frame (≈100/s at 25 fps); Art-Net one per frame.
- **Timecode slaving.** Slaves compare their own clock with timecode every tick. Beyond the tolerance
  (default 80 ms), media timelines re-sync with an in-place seek (de-clicked audio), followed by a
  250 ms hold-off so an engine settling after a jump is not chased again. Timelines without a media
  clock are corrected by re-timing their clock alone, so DMX looks are not re-fired.
- **Undo.** Snapshot-based and coalesced (a fader drag is one step). Restoring a snapshot re-attaches
  live cue state by cue id, so running cues stay stoppable, and the live master level is never reverted.

---

## Testing

```bash
cargo test --workspace                                                     # 114 tests
PKG_CONFIG_PATH=$HOME/.local/ffmpeg-9/lib/pkgconfig \
  cargo test --workspace --features livestage_ui/ffmpeg,livestage_video/ffmpeg  # 117 tests
cargo clippy --workspace --all-targets
```

Tests include headless GPU checks (warp/blend/mask, NV12 conversion, default-projection visibility,
stream readback), a hardware-decode check on macOS, a live NDI send → discover → receive loopback
(skipped when the NDI runtime is absent), bit-exact Art-Net/sACN packets, the cue/timeline state
machine (including in-place seek), a click-free live audio seek, PDF and PowerPoint import (generated
test documents), background slide decoding, multiview layouts, and timecode: drop-frame math over a
whole day, exact frame ↔ time round trips at every rate, MTC / Art-Net encode-decode, lock-in,
drift compensation against a jittery sender running off-speed, freewheel / stop / jump / locate,
midnight wrap, and slave behaviour (roll-in, chase, pause / stop / continue, re-sync with hold-off).

Live timecode check with a virtual MIDI source (macOS / Linux):

```bash
cargo run -p livestage_control --example mtc_send -- 00:59:58:00 10 25   # creates "LiveStage MTC Test"
```

To review UI changes without a display:

```bash
cargo run --release -- --ui-snapshot ui.png --go 4 --select 4 --tab multiview --size 1600x1000
cargo run --release -- --ui-snapshot ui.png --import talk.pdf --go 8 --next-slide 1 --tab multiview
cargo run --release -- --ui-snapshot ui.png --settings audio
```

This renders the real UI offscreen (with live output canvases) to a PNG. `--go` may repeat; note
that GO on audio cues plays through the default audio device.

---

## Known gaps

- **Timecode** is input-only: LiveStage chases MTC and Art-Net timecode and generates timecode
  internally, but does not yet transmit MTC / Art-Net timecode or read LTC from an audio input
  (`mtc_send` is a test tool). Slaved media is kept in sync by de-clicked seeks beyond the
  tolerance, not by varispeed resampling.

- **Zero-copy video**: hardware frames are copied GPU→CPU→GPU as NV12. True zero-copy (IOSurface /
  D3D11 shared textures into `wgpu`) needs wgpu-hal interop. Audio inside movie files is ignored (use
  an audio cue).
- **NDI**: video only (no NDI audio or tally). The send path still copies from GPU to CPU, because the
  NDI SDK takes CPU buffers.
- **PowerPoint without LibreOffice** draws the common building blocks only (see
  [Presentations](#presentations)); embedded fonts are replaced by the bundled UI font.
- **Kiosk outputs** have been exercised as windowed outputs on a single-display Mac; multi-monitor
  kiosk placement on mixed-DPI setups follows the logic above but still needs a test on real hardware.
- `.bson` manifests are not implemented (JSON only).
- The dependency set is pinned to one compatible generation (wgpu 24, egui 0.31, winit 0.30,
  cpal 0.15, symphonia 0.5). Upgrade the wgpu / egui / winit trio together.

---

Licensed under MIT OR Apache-2.0, as declared in the workspace `Cargo.toml`.
