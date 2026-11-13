# Improvement Plan — audio-streamer / audio-streamer-server

Two repos:

- Client (capture): `/Users/aero/Documents/git/audio-streamer`
- Server (playback): `/Users/aero/Documents/git/audio-streamer-server`

Wire protocol today: UDP port 9032. Config packet `\x02{bits} {rate} {buffer}`;
sound packet `[1][LE samples]`; silence packet `[0][u16 LE byte count]`.
Bit depth and rate are taken from the config; the server is generic over
`i16` and `i32` and the clients send 16-bit natively when the source is 16-bit.

Priority overview:

| Phase | Scope | Priority | Wire change |
|---|---|---|---|
| 0 | Baseline build + commit | — | no |
| 1 | Receiver resilience (XRUN, idle, buffer bounds) | P0 | no |
| 2 | Protocol v2: sequence numbers, periodic config, MTU cap | P1 | yes |
| 3 | Jitter buffer + clock-drift control | P1 | no |
| 4 | Sender latency (macOS/Linux/Windows) | P2 | no |
| 5 | Optional Opus | P3 | yes |
| 6 | Tests, soak, CI | P1 | no |

---

## Phase 0 — Baseline

Both repos currently have uncommitted 16-bit work. Build on macOS, Linux and
Windows, commit the 16-bit change, then branch per repo.

Verify: `cargo build --release` on all three platforms; one stream per client OS.

---

## Phase 0.5 — Verified APIs (do not deviate)

### ALSA 0.8.1 (`/tmp/alsa-rs`, tag v0.8.1)

- `PCM::try_recover(&self, err: Error, silent: bool) -> Result<()>` — pcm.rs:172-178.
  Wraps `snd_pcm_recover`; handles EPIPE (prepare) and ESTRPIPE (resume).
- `PCM::prepare`, `resume`, `pause`, `drop`, `start` — pcm.rs:161-170.
- `IO::writei(&self, buf: &[S]) -> Result<usize>` — pcm.rs:388; no internal recovery.
- `alsa::Error::errno()` — error.rs:67. nix is pinned to 0.26.4 in the server
  `Cargo.lock`, so this is a positive `Errno`. Pass the error to `try_recover`.
- `PCM::avail_update()` pcm.rs:199, `PCM::delay()` pcm.rs:206, `PCM::state()` pcm.rs:183.
- `HwParams::set_period_size_near(v, dir)` pcm.rs:872, `set_buffer_size_near(v)`
  pcm.rs:912; `SwParams::set_avail_min` pcm.rs:1034, `set_start_threshold` pcm.rs:1048.

Anti-patterns: `PCM::set_nonblock` does not exist (nonblocking is `PCM::new(_, _, true)`).
Do not use the `std::io::Write` impl for `IO` when errno matters.

### cpal 0.15.3

- `BufferSize::{Default, Fixed(u32)}` — src/lib.rs:219-265.
- `SupportedStreamConfig::buffer_size() -> &SupportedBufferSize` (`Range { min, max } | Unknown`) — src/lib.rs:267-277, :391-413.
- `SupportedStreamConfig::config()` always returns `BufferSize::Default` — src/lib.rs:407-413. Set `Fixed` explicitly.
- macOS honours `Fixed` via `kAudioDevicePropertyBufferFrameSize` — src/host/coreaudio/macos/mod.rs:527-561.
- Out-of-range `Fixed` → `BuildStreamError::StreamConfigNotSupported` — mod.rs:553-555.

Anti-pattern: never pass a `Fixed` value outside `Range { min, max }`; treat `Unknown` as `Default`.

### PipeWire 0.7.2

- `pw::keys::NODE_LATENCY` — requested latency as a fraction, e.g. `128/48000` (pipewire/src/keys.rs:208).
- `properties!` accepts expressions as values (properties.rs:53-60); values are `Into<Vec<u8>>`.

### kanal 0.1.0-pre8

- `Sender::try_send(&self, T) -> Result<bool, SendError>`, `is_full`, `len`, `is_disconnected`.

### wasapi 0.14.0

- `get_mixformat`, `get_bitspersample`, `get_samplespersec`, `get_bufferframecount`,
  `get_current_padding`, `get_available_space_in_frames` — api.rs:409-696.
- `WasapiRes<T> = Result<T, Box<dyn std::error::Error>>` (not Send + Sync), so wasapi
  calls do not `?` into `anyhow::Result` directly; map the error to a string.

### Clock-drift correction — decision

Default is **drop/insert, no resampling**:

- Correct only in the native integer domain; no new dependency.
- Keep the queue at half the `--latency` buffer (default ~40 ms), with a ±10 ms dead band.
- Queue too deep: consume two frames and emit their blend, removing one frame.
- Queue too shallow: emit the blend of the held frame and the next queued frame,
  then emit that frame next — one extra frame, one-frame crossfade.
- Whole frames only, so L/R cannot swap. At most one correction per 100 ms.

Resampling is out of scope. rubato is a fallback only for mismatched nominal rates
(>1 % error), not for drift. Research notes: rubato 5.1.0 (MIT/Apache) has `Slip`
(zero delay, ~0.8 % capacity) but is f32/f64 only and uses an `audioadapter` API;
the `SincFixedIn`-style names only exist in rubato <= 0.16.2. `samplerate` allocates
per call and has no ratio setter. `soxr` is LGPL and needs system libsoxr.
`speexdsp-resampler` is unmaintained.

---

## Phase 1 — Receiver resilience (P0)

### 1.1 Idle must not tear down playback

`server/src/main.rs` — a 1 s `recv` timeout (WouldBlock/TimedOut) currently returns an
error, ending `handle`, dropping the playback thread and re-opening ALSA. Treat both
kinds as `continue`; exit only when `sender.is_disconnected()`.

Verify: kill the sender for 15 s and resume — no `handler error`, one ALSA open.

### 1.2 ALSA XRUN recovery

`server/src/playback/alsa.rs` — wrap `io.writei`; on error call
`pcm.try_recover(e, true)?`, reset `started = false`, `written = 0`, `filled = false`,
then `continue` so the stream re-fills to `start_threshold`.

Verify: pause the sender longer than the ALSA buffer; log `XRUN recovered`; audio resumes.

### 1.3 Silence-guard units

`server/src/main.rs` compares `sender.len()` (samples) with `buffer` (frames).
Compare with `buffer * CHANNELS`.

### 1.4 WASAPI server buffer bound

`server/src/playback/wasapi.rs` slices a fixed 16384-byte buffer with a size derived
from the device buffer. Allocate the buffer from
`get_bufferframecount() * blockalign` after init and clamp `size`.

Verify: output device configured with a >=100 ms buffer; stream survives.

### 1.5 Remove recoverable unwraps from the WASAPI playback loop

The release profile uses `panic = "abort"`, so any panic in the playback thread kills
the server. Map `WasapiRes` errors into `anyhow` instead of unwrapping.

---

## Phase 2 — Protocol v2 (P1)

1. **Versioned config, backward compatible**: `\x02{version} {bits} {rate} {buffer} {channels}`;
   parse positionally so 3-field v1 packets still work (version 1, channels 2).
2. **Sequence numbers**: every packet carries `u32 LE` after the tag:
   `[1][seq][samples]`, `[0][seq][count]`. Track the expected sequence and count
   gaps; concealment is handled by the Phase 3 jitter buffer.
   Stats (gaps, drops, queue depth) print every 5 s when non-zero.
3. **MTU cap**: split capture buffers into payloads <= 1400 bytes. Windows currently
   sends 8193 bytes; macOS sends the whole callback. Shared splitter in `client/src/capture.rs`.
4. **Periodic config**: resend every ~2 s so a restarted server self-heals; keep the
   server's cached-config fallback.
5. **Non-blocking enqueue**: use `try_send` on the server so a full queue cannot stall
   UDP receives; count drops.

Rollout: server first (parses both versions), then clients.

Verify: restart the server mid-stream; `tc netem loss 1%` yields counted gaps.

---

## Phase 3 — Jitter buffer and clock drift (P1, depends on Phase 2)

Implemented as `Jitter` in `server/src/playback.rs`:

1. `Jitter::new(rate, latency_ms)` sets the target depth from `--latency`
   (default 40 ms), clamped to half the queue capacity, with a ±10 ms band.
2. `next_frame` pulls one interleaved frame per call. Too deep: consume two frames
   and emit their blend (removes one). Too shallow: emit a blend of the held frame
   and the next queued frame, then emit that frame next (adds one). Both use a
   one-frame crossfade and are rate-limited to once per 100 ms.
3. Before playback starts it blocks for data; after starting, an empty queue
   yields whole frames of silence.
4. Both ALSA and WASAPI playback use it; the old pair-toggling fill logic is gone.
5. Ingress uses `try_send` (Phase 2.5); no resampling.
6. Unit tests cover insert, drop, in-band, and pre-start behaviour.

Verify: 60-minute soak with a deliberately skewed sender clock; bounded queue, no clicks.

---

## Phase 4 — Sender latency (P2)

1. **macOS**: add `--frames N` (default 256); read `supported.buffer_size()`, clamp,
   set `config.buffer_size = BufferSize::Fixed(n)`; fall back to `Default`.
   Log the first callback's `data.len()`.
2. **Packet size follows the callback** (Phase 2.3) so a loss costs <= ~8 ms.
3. **PipeWire quantum**: `*pw::keys::NODE_LATENCY => format!("{}/{}", frames, rate)`.
4. **Send errors must not exit**: `pipewire.rs` currently calls `std::process::exit(1)`;
   log once per second, count, continue.
5. Remove unused `ringbuf` and `coreaudio-rs` dependencies.

---

## Phase 5 — Optional Opus (P3)

Only if bandwidth/loss is the bottleneck. Add `codec` to the config, 20 ms frames,
`opus` crate behind a feature flag. ~10-20 ms extra latency. Not before Phase 1-3.

---

## Phase 6 — Verification

- Unit tests (none today): packet splitter, sequence-gap arithmetic, silence expansion
  per bit depth, config parsing for v1/v2.
- Integration: test sender with a known ramp; assert sample count and ordering.
- Network: `tc qdisc add dev lo root netem loss 1% reorder 25% 50% delay 20ms`.
- Soak: 60 min per client OS; track queue depth, underruns, drops, gaps.
- CI: GitHub Actions matrix (`ubuntu-latest`, `windows-latest`, `macos-14`) with
  `cargo check` + tests for both repos.

Acceptance: no handler errors, no process exits, no unbounded queue growth, no IP
fragmentation, latency within per-phase budget.
