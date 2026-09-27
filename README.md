# RageGuard

RageGuard is a Discord moderation bot that listens to **one selected member** in a voice channel,
classifies the emotion of their speech with a speech-emotion model, and applies a Discord
**timeout** when it detects repeated, high-confidence angry speech.

- **Rust bot** (`bot/`): Discord API ([serenity]), voice ([songbird]), audio buffering and
  resampling, detection rules, moderation, configuration and structured logging.
- **Python AI service** (`ai-service/`): FastAPI + PyTorch + Hugging Face Transformers running
  [`Dpngtm/wav2vec2-emotion-recognition`][model].

> [!WARNING]
> **Voice emotion classification is probabilistic and often wrong.** The model was trained on
> acted English speech, and it confuses loud, fast or synthetic voices with anger. In testing, a
> calmly read sentence produced two segments scored *Angry* at 93% and 99% (see
> [Accuracy](#accuracy-and-limitations)). RageGuard therefore never acts on a single prediction,
> and it starts in **monitor-only mode**, which logs what it *would* do without timing anyone out.
> Keep a human in the loop.

**First time?** Follow [setup.md](setup.md) for a step-by-step walkthrough: creating the bot,
getting its token, inviting it to your server and running a first safe test.

---

## Contents

1. [Architecture](#architecture)
2. [Prerequisites](#prerequisites)
3. [Discord Developer Portal setup](#discord-developer-portal-setup)
4. [Bot permissions](#bot-permissions)
5. [Configuration](#configuration)
6. [Python setup](#python-setup)
7. [Rust setup](#rust-setup)
8. [Starting the AI service](#starting-the-ai-service)
9. [Starting RageGuard](#starting-rageguard)
10. [Slash commands](#slash-commands)
11. [How detection and moderation work](#how-detection-and-moderation-work)
12. [Testing](#testing)
13. [Docker](#docker)
14. [Troubleshooting](#troubleshooting)
15. [Privacy considerations](#privacy-considerations)
16. [Accuracy and limitations](#accuracy-and-limitations)
17. [Project structure](#project-structure)

---

## Architecture

```text
Discord
   │
   │ Voice (Opus, DAVE end-to-end encrypted)
   ▼
RageGuard Rust Bot
   │
   ├── Voice Receiver      only the monitored user's packets are kept
   ├── Audio Buffer        3 s segments (flushed early after 800 ms of silence)
   ├── 16 kHz Resampler    48 kHz stereo → 16 kHz mono WAV, on a blocking thread
   │
   ▼  POST /analyze (multipart, binary WAV)
Python FastAPI
   │
   ▼
Wav2Vec2 Emotion Model
   │
   ▼
Angry + Confidence
   │
   ▼
RageGuard Detection Engine   (sliding window)
   │
   ├── Below threshold → continue
   │
   └── Threshold reached
            │
            ▼
       Pre-checks → Discord Timeout (or MONITOR ONLY log) → moderation channel notice
```

### Runtime design

Any number of members can be monitored per server. Two kinds of Tokio task do the work:

| Task | Responsibility |
| --- | --- |
| **Voice supervisor** (`monitor/supervisor.rs`), one per server | A bot can only be in one voice channel per server, so this keeps it in the channel with the most monitored members (ties go to the member monitored longest). Every event (a monitored member moved or left, bot disconnected, driver failure, a 30 s safety tick) triggers a *reconcile* that compares where the members are with where the bot is and fixes any difference, with backoff on failure. |
| **Analysis worker** (`monitor/worker.rs`), one per monitored member | Takes that member's finished segments from a bounded queue, converts them to 16 kHz WAV in `spawn_blocking`, calls the AI service, feeds that member's detection engine and runs moderation. Segments are processed in order; if analysis falls behind, new segments are dropped instead of queueing without bound. |

The songbird event handler (`voice/receiver.rs`) never blocks: it copies each monitored member's
20 ms frames into that member's own buffer and hands full segments over with `try_send`.

Shared state is thread-safe and contains no globals:

- `MonitorRegistry` (`tokio::sync::RwLock<HashMap<GuildId, HashMap<UserId, Arc<MonitorSession>>>>`)
  holds one session per monitored member, inserted atomically so that two moderators starting
  monitoring of the same member at the same moment cannot both succeed.
- Per-session state (detector, last result, counters) sits behind a short-lived `parking_lot::Mutex`
  that is never held across an `.await`.
- A per-guild async lock serialises voice joins and leaves, so a session being stopped cannot
  disconnect a session that was just started.

---

## Prerequisites

| | Version | Notes |
| --- | --- | --- |
| Rust | stable **1.88+** | `rustup update stable` |
| C toolchain + **CMake** | any recent | Needed to compile the Opus codec used by songbird. **Windows:** Visual Studio Build Tools with *Desktop development with C++*, plus CMake (`winget install Kitware.CMake`). **Debian/Ubuntu:** `sudo apt install build-essential cmake pkg-config`. **macOS:** `xcode-select --install && brew install cmake`. |
| Python | **3.11+** | For the AI service. Tested with 3.12 (Docker) and 3.14 (Windows). |
| Docker | optional | Easiest way to run the AI service: `docker compose up -d`. |
| Disk | ~2 GB | PyTorch (CPU) plus the ~380 MB model. |

A GPU is not required: CPU inference takes roughly 100–250 ms per 3 s segment.

---

## Discord Developer Portal setup

1. Go to <https://discord.com/developers/applications> and click **New Application**. Name it
   *RageGuard*.
2. **General Information:** note the **Application ID**. You only need it to build the invite link
   by hand; the bot also prints the link in its log once it is running.
3. **Bot** tab:
   - Click **Reset Token** and copy the token into `DISCORD_TOKEN`. Treat it like a password.
   - **Privileged Gateway Intents:** leave all three **off**. RageGuard only uses the
     non-privileged `GUILDS` and `GUILD_VOICE_STATES` intents.
4. **Installation** (or **OAuth2 → URL Generator**):
   - Scopes: `bot`, `applications.commands`
   - Bot permissions: see the next section.
   - Open the generated URL and add the bot to your server. RageGuard also prints a ready-made
     invite link in its log on startup.
5. **Server Settings → Roles:** drag the **RageGuard** role **above** the roles of every member it
   should be able to time out. Discord only lets a bot time out members whose highest role is
   below the bot's highest role.
6. **Private channels:** if channels are hidden from `@everyone`, give the RageGuard role
   **View Channel** + **Connect** on the voice channels it should monitor, and **View Channel** +
   **Send Messages** on the log channel.

Slash commands are registered for every server the bot is in. After the first start they can take
a few minutes to appear; press Ctrl+R in Discord to refresh.

## Bot permissions

| Permission | Why |
| --- | --- |
| View Channels | See the voice channel and the moderation channel |
| Connect | Join the monitored user's voice channel |
| Send Messages, Embed Links | Moderation notices and `/anger-status` |
| **Moderate Members** | Apply timeouts |

Permission integer: `1099512695808`. RageGuard does **not** need Speak, Administrator, or any
privileged intent.

Who can use the commands: members with **Moderate Members** (or Administrator). The commands are
hidden from everyone else by default, and the bot checks the permission again on every use, so
changing the command visibility in *Server Settings → Integrations* cannot bypass it.

---

## Configuration

### Environment (`.env`), secrets and deployment

Copy `.env.example` to `.env` in the repository root. The bot searches the working directory and
its parents, so it is found from `bot/` too.

| Variable | Required | Default | Description |
| --- | --- | --- | --- |
| `DISCORD_TOKEN` | **yes** | | Bot token |
| `AI_SERVICE_URL` | no | `http://127.0.0.1:8000` | Where the Python service listens |
| `AI_SERVICE_PORT` | no | `8000` | Host port `docker compose` publishes the AI service on |
| `MONITOR_ONLY` | no | `true` | `true`: log timeouts only. `false`: apply them. Only `true/false/1/0/yes/no/on/off` are accepted; anything else refuses to start. |
| `LOG_LEVEL` | no | `info` | `trace`, `debug`, `info`, `warn`, `error`. `RUST_LOG` overrides it. |
| `RAGEGUARD_CONFIG` | no | `rageguard.toml` | Path to the detection config |
| `RAGEGUARD_DATA_DIR` | no | `data` | Where the log channel chosen in Discord is saved (`guild-settings.json`) |

Missing or invalid values stop the bot with a specific message, for example:

```text
error: missing required environment variable `DISCORD_TOKEN`. Copy .env.example to .env and fill it in.
```

### Detection rules (`bot/rageguard.toml`)

```toml
[anger_detection]
threshold = 0.70          # minimum anger score for a segment to count
required_detections = 2   # detections needed ...
window_seconds = 15       # ... inside this sliding window
timeout_minutes = 1       # timeout length (max 40320 = 28 days)
segment_seconds = 3       # audio per analysis request (2–5 recommended)
anger_emotions = ["Angry", "Disgust"]  # scores added up as "anger" (see below)

[audio]
min_segment_seconds = 1.0 # shorter utterances are discarded
silence_flush_ms = 800    # analyse a partial segment after this much silence
max_pending_segments = 2  # analysis backlog before segments are dropped

[ai_service]
request_timeout_seconds = 10
connect_timeout_seconds = 3
```

Every key is optional. Unknown keys are rejected, so a typo such as `threshhold` fails loudly
instead of being silently ignored. Rules that can never trigger (for example 6 detections of 3 s
segments in a 10 s window) produce a warning at startup. If the file is missing, the defaults
above are used.

---

## Python setup

```bash
cd ai-service
python -m venv .venv
# Windows: .venv\Scripts\activate      macOS/Linux: source .venv/bin/activate

# CPU-only PyTorch (small download). Skip this line to get your platform's default build (CUDA etc.).
pip install torch --index-url https://download.pytorch.org/whl/cpu
pip install -r requirements.txt -r requirements-dev.txt
```

Service environment variables (all optional): `MODEL_ID` (default
`Dpngtm/wav2vec2-emotion-recognition`), `DEVICE` (`cpu`, `cuda`, ...; auto-detected by default),
`MAX_UPLOAD_BYTES` (default 1 MiB), `TORCH_THREADS`, `LOG_LEVEL`, and `HF_TOKEN` for faster Hugging
Face downloads.

## Rust setup

```bash
cd bot
cargo build     # first build compiles Opus with CMake; takes a few minutes
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt
cargo run
```

VS Code: open the repository root and install the recommended extensions. `.vscode/` has build,
test, clippy and fmt tasks, launch configurations for the bot (LLDB, or the Windows debugger) and
the AI service, and pytest integration.

---

## Starting the AI service

**Docker (recommended):**

```bash
docker compose up -d
curl http://127.0.0.1:8000/health
# {"status":"ok","model_loaded":true}
```

The first start downloads the model into the `hf-cache` volume. While it loads, `/health` returns
`503 {"status":"loading","model_loaded":false}`.

**Locally:**

```bash
cd ai-service
.venv/bin/python -m uvicorn app.main:app --host 127.0.0.1 --port 8000
# Windows: .venv\Scripts\python.exe -m uvicorn app.main:app --host 127.0.0.1 --port 8000
```

API:

| Endpoint | Request | Response |
| --- | --- | --- |
| `POST /analyze` | `multipart/form-data`, field `file`: WAV/FLAC/OGG, 0.5–30 s, any sample rate or channel count | `200 {"emotion":"Angry","confidence":0.91,"scores":{"Angry":0.91,"Neutral":0.04,...}}` |
| | | `422` unsupported, corrupted, too short or too long audio · `413` upload too large · `503` model loading · `500` inference error |
| `GET /health` | | `200 {"status":"ok","model_loaded":true}` or `503` while loading or after a load failure |

Labels are normalised to `Angry, Disgust, Fear, Happy, Neutral, Sad, Surprise`. This model has no
"neutral" class; its **`calm`** class is reported as `Neutral`, and `fearful`/`surprised` become
`Fear`/`Surprise`.

## Starting RageGuard

```bash
cd bot
cargo run --release
```

```text
INFO loaded detection config path=rageguard.toml
INFO configuration threshold=0.8 required_detections=3 window_seconds=15 timeout_minutes=5 ...
WARN MONITOR_ONLY is enabled: RageGuard will log timeouts but never apply them
INFO connected to Discord bot=RageGuard guilds=1 monitor_only=true
INFO registered 5 slash commands
INFO AI service is ready
```

Stop with **Ctrl+C**: RageGuard ends all sessions and leaves voice channels before exiting.

---

## Slash commands

All replies are ephemeral (only the moderator sees them).

### `/anger-monitor user:@member`

Starts monitoring and records the guild, user, voice channel, who started it and when. If the
member is in a voice channel, RageGuard joins it (muted) right away. If not, monitoring starts in a
waiting state and RageGuard joins **as soon as they enter a voice channel**.

**Several members** can be monitored at once, each with their own anger count and their own
timeout. All monitored members in the same voice channel are analysed together. A bot can only be
in one voice channel per server, though, so when monitored members are in different channels
RageGuard sits in the channel with the most of them; the others show as *"Waiting (RageGuard is
listening to other monitored members in another channel)"* in `/anger-status`.

RageGuard follows the members: if they switch channels it moves to wherever most of them are, and
when none of them is in voice it leaves and waits until one returns.

### `/anger-stop user:@member`

Stops monitoring, leaves voice and discards the detection history.

### `/anger-status`

Shows the monitored user, monitoring status and channel, most recent emotion and confidence, angry
detections in the current window, segment and error counters, the current threshold and
timeout duration, whether monitor-only mode is on, and AI service health.

### `/anger-config [threshold] [detections] [window_seconds] [timeout_minutes] [reset]`

With no options it shows the current settings; with options it changes them for this server.
Changes are validated (invalid ones change nothing), apply immediately (including to an active
session). Changes are held in memory, so `rageguard.toml` applies again after a restart. `reset`
restores the rules from `rageguard.toml`. `segment_seconds` and `MONITOR_ONLY` are deliberately
not changeable from Discord, and the log channel is set with `/anger-setup`.

### `/anger-setup`

Chooses the **moderation log channel** from a dropdown. The dropdown lists **only the text
channels RageGuard can post in**, so a channel it can't see can't be picked by mistake. If yours
is missing, give the RageGuard role View Channel and Send Messages there and run `/anger-setup`
again. The first time RageGuard starts on a server that has no log channel, it posts the same
dropdown in the server's system channel (or the first text channel it can write in), once. When a
moderator picks a channel, RageGuard posts a confirmation there and saves the choice to
`data/guild-settings.json`, so it survives restarts. Only members with Moderate Members can
choose.

---

## How detection and moderation work

1. Songbird decrypts (including DAVE end-to-end encryption) and decodes everyone's audio. The
   receiver keeps only the frames whose SSRC belongs to the monitored user; other audio is dropped
   in the same 20 ms tick.
2. Frames accumulate into a **3 s segment**. Brief pauses are skipped; after 800 ms of silence a
   partial segment of at least 1 s is analysed, and shorter ones are discarded.
3. The segment is downmixed and resampled (windowed-sinc, anti-aliased) to **16 kHz mono**,
   encoded as an in-memory WAV and uploaded to `/analyze`.
4. The **anger score** (the Angry and Disgust probabilities added together, set by `anger_emotions`)
   is compared with the threshold. Disgust counts because in live testing the model labelled real
   angry shouting as Disgust (97–98%) rather than Angry. Scores at or above it are recorded as
   an `AngerDetection { timestamp, confidence }`, where the timestamp is the audio capture time.
5. Detections older than the window are dropped. When `required_detections` fall inside the
   window, moderation triggers and the history is cleared, so a further trigger needs a fresh set
   of detections.
6. Before any timeout RageGuard verifies, in order, that:
   1. the user is still in the server,
   2. the bot has **Moderate Members**,
   3. the bot's highest role is **above** the user's highest role,
   4. the user is not the **server owner**,
   5. the user is not an **Administrator** (Discord never allows timing out administrators).
7. It then applies the timeout (with an audit-log reason), logs it and posts to the moderation
   channel chosen with `/anger-setup`, or, if none was chosen yet, the channel where
   `/anger-monitor` was used.

```text
⚠️ RageGuard

@Username has been timed out for 1 minute.

Reason:
Repeated angry voice detection

Confidence:
91%

Detections:
2 within 15 seconds
```

Logs are structured (`tracing`):

```text
INFO voice_segment_received user_id=123456 duration=3.0
INFO emotion_detected user_id=123456 emotion=Angry confidence=0.87 angry=0.87
INFO anger_counter user_id=123456 count=2 required=3
WARN timeout_triggered user_id=123456 duration=60 confidence=0.91 detections=2
```

### Monitor-only mode (default)

With `MONITOR_ONLY=true` RageGuard **never** calls the timeout API. It still runs the read-only
pre-checks, so the notice tells you whether a real timeout would have worked:

```text
WARN [MONITOR ONLY] Timeout would have been applied to Username. Emotion: Angry, Confidence: 91%, Detections: 2/2
```

**Going live checklist:** run in monitor-only mode for a while; confirm the notices say *"A real
timeout would have been allowed"* (otherwise fix the role order or permissions); review how often
it would have fired and tune `threshold` and `required_detections`; tell your members; then set
`MONITOR_ONLY=false` and restart.

### Two logs: the bot log and the server log

| | Who reads it | Where | What's in it |
| --- | --- | --- | --- |
| **Bot log** | You, the person running the bot | The console (`cargo run`), or `docker compose logs -f bot`, the **docker compose logs (bot)** VS Code task, or Docker Desktop → `rageguard-bot-1` → Logs | Everything, for every server: every analysed segment, emotion scores, connections, errors. Level set by `LOG_LEVEL`. |
| **Server log** | That server's moderators | The log channel chosen with [`/anger-setup`](#anger-setup) | Only what moderators need to act on, for that server (see below). Nothing is posted until a channel is chosen. |

The server log only carries warnings and trigger notices. Everyday activity (monitoring started,
joined voice, each angry clip) stays in the bot log:

```text
⚠️ RageGuard  @Yap has been timed out for 1 minute ...            (or 🧪 MONITOR ONLY: would have been)
⚠️ Couldn't join #General to follow @Yap: RageGuard is missing ...  (once per problem)
⚠️ The AI service isn't responding, so voice analysis is paused ... (once per outage)
```

Mentions in these messages never ping anyone. Because the server log shows who was flagged, use a
channel only moderators can see.

---

## Testing

### Rust (`cd bot`)

```bash
cargo test
```

149 tests, with Discord and the AI service mocked:

| File | Covers |
| --- | --- |
| `tests/detection_engine.rs` | threshold boundaries, sliding window and edges, counting after a trigger, reset, invalid scores |
| `tests/config_loading.rs` | env parsing and errors, safe defaults, TOML parsing and validation, the shipped `rageguard.toml` |
| `tests/ai_client.rs` | response parsing (mocked `Angry 0.91` and `Neutral 0.92`), label normalisation, multipart upload, 422/503/500, timeouts, service offline (via `wiremock`) |
| `tests/moderation.rs` | moderator permission checks, permission and role-hierarchy computation, every timeout blocker, monitor-only never applying timeouts, enforcement, notice text |
| `tests/audio_pipeline.rs` | segmentation and silence flushing, resampler accuracy and anti-aliasing, WAV encoding |
| `tests/analysis_pipeline.rs` | end to end without Discord: audio → WAV → mock AI → detection → mock moderation, plus what reaches the server log |
| `tests/state_registry.rs` | 32 concurrent `/anger-monitor` attempts with exactly one winner, stop semantics, per-guild settings |
| `tests/settings_store.rs` | saved log channel surviving restarts, env fallback, one-time setup prompt, atomic JSON file, corrupt-file errors |
| `tests/setup_flow.rs` | where the setup message is posted, and that the picker lists only channels RageGuard can post in |
| `tests/server_log.rs` | server-log messages, and that nothing is posted before a channel is chosen |
| `tests/voice_access.rs` | the pre-join check: missing View Channel / Connect, full channels |

**Live end-to-end** against a running AI service, through the real audio pipeline:

```bash
cargo test --test e2e_ai_service -- --ignored --nocapture
# Optional: RAGEGUARD_AI_URL=http://127.0.0.1:8000  RAGEGUARD_E2E_WAV=path/to/speech.wav
```

### Python (`cd ai-service`)

```bash
python -m pytest                                 # 32 tests, fake model, no torch needed
RAGEGUARD_TEST_REAL_MODEL=1 python -m pytest     # also downloads and runs the real model
```

---

## Docker

```bash
docker compose up -d                  # AI service on 127.0.0.1:8000
docker compose logs -f ai-service
docker compose down                   # add -v to delete the cached model
```

- The port is published on **localhost only**; the service has no authentication.
- Port 8000 taken? Set `AI_SERVICE_PORT=8010` in `.env` (compose reads it) and
  `AI_SERVICE_URL=http://127.0.0.1:8010`.
- NVIDIA GPU: `docker compose build --build-arg TORCH_INDEX_URL=https://download.pytorch.org/whl/cu128`
  and add `gpus: all` to the service.
- Bake the model into the image: `docker compose build --build-arg PRELOAD_MODEL=true`.

**Running the bot in Docker (optional).** During development the bot normally runs on the host,
but it can also run as a container that reads `.env`:

```bash
docker compose --profile bot up -d --build
```

The container keeps the chosen log channel in the `bot-data` volume.

---

## Troubleshooting

| Symptom | Cause / fix |
| --- | --- |
| `An Application Control policy has blocked this file. (os error 4551)` during `cargo build` on Windows | **Smart App Control** blocks the unsigned executables Cargo compiles, including build scripts. Either turn it off (Windows Security → App & browser control → Smart App Control), build inside WSL2, or run the bot with `docker compose --profile bot up -d --build`. |
| `failed to run custom build command for libopus_sys` / `is cmake not installed?` | Install CMake and a C compiler (see [Prerequisites](#prerequisites)). With a portable CMake, set `CMAKE=C:\path\to\cmake.exe`. |
| `Bind for 0.0.0.0:8000 failed: port is already allocated` | Something else uses port 8000 (a local Supabase stack's gateway does). Use `AI_SERVICE_PORT` as described under [Docker](#docker), and update `AI_SERVICE_URL`. |
| `invalid response from AI service` or `HTTP 404` | `AI_SERVICE_URL` points at a different program. Check `curl <url>/health`. |
| `AI service is unreachable` | Start the service. Segments are skipped (not queued) while it is down, and RageGuard recovers automatically. |
| `AI service is not ready yet` | The model is still downloading or loading; watch `/health`. |
| `Discord rejected DISCORD_TOKEN` | Reset the token in the Developer Portal and update `.env`. |
| Slash commands missing | Wait a few minutes after the first start and press Ctrl+R in Discord; check the bot was invited with `applications.commands`, and that you have Moderate Members. |
| "I can't join #channel: RageGuard is missing the View Channel and Connect permissions there" | The channel (or its category) hides it from RageGuard. Allow both for the RageGuard role in the channel's Permissions. RageGuard checks this before joining. |
| "Couldn't join … gateway response from Discord timed out" | Discord ignored the join request, almost always a permission problem the cache couldn't see. Review the channel's permissions for the RageGuard role. |
| Bot joins but nothing is analysed | The member hasn't spoken yet (Discord reveals their audio stream when they first speak). Run with `LOG_LEVEL=debug` and look for `target_ssrc_mapped`. Don't server-deafen the bot. |
| `analysis_backlog_full: dropping segment` | Inference is slower than real time. Use a faster CPU or GPU, or increase `segment_seconds`. |
| Notice says the timeout would be blocked by role position | Move the RageGuard role above the member's highest role. |
| Owner or administrators are never timed out | Discord doesn't allow it; RageGuard reports this instead of failing. |
| Discord rate limits | Serenity queues requests automatically; RageGuard sends very few (one notice per trigger). |

---

## Privacy considerations

- **Tell people.** Analysing someone's voice for emotion is sensitive, and in many jurisdictions
  it is regulated personal or biometric data processing. Inform your members (for example in the
  rules channel), get consent where required, and check Discord's Developer Terms and Policy and
  your local law before enabling this.
- **Only the selected member.** Discord's voice protocol delivers everyone's audio, and songbird
  decodes all of it. RageGuard keeps only the monitored member's frames and drops everything else
  in the same 20 ms tick.
- **Nothing is recorded.** Audio exists only as short in-memory buffers: at most one 3 s segment
  being filled, plus up to `max_pending_segments` waiting for analysis. The WAV is built in memory,
  uploaded, and freed. The AI service decodes from memory, and uploads under 1 MiB are never
  spooled to disk. No audio is written to disk or logged; logs contain only emotion labels, scores,
  durations and Discord IDs.
- **Local processing.** The AI service is published on `127.0.0.1` only and makes no outbound
  calls except fetching the model files from Hugging Face; once cached, set `HF_HUB_OFFLINE=1` to
  stop even that.
- **Stopping discards everything:** `/anger-stop` drops buffers and detection history. The only
  thing RageGuard writes to disk is `data/guild-settings.json`: server and log-channel IDs, and
  whether the setup message was sent. It contains no user data.

## Accuracy and limitations

- The model ([`Dpngtm/wav2vec2-emotion-recognition`][model], wav2vec2-base) was fine-tuned on
  acted English speech (TESS, CREMA-D, SAVEE, RAVDESS). Real gaming voice chat is very different:
  compressed audio, background noise, laughter, accents, other languages.
- Measured during development with synthetic (text-to-speech) voices, analysed in 3 s segments:

  | Clip | Segments scored Angry ≥ 80% | Result with default rules |
  | --- | --- | --- |
  | Calm, friendly sentence (14 s) | 2 of 5 (**93%, 99%**), 6 s apart | **would trigger** with the default of 2; no action with 3 |
  | Loud, fast, shouted sentences (8 s) | 3 of 3 (92%, 87%, 94%) | trigger |

  High-confidence false positives happen. The multi-detection window and the human review that
  monitor-only mode allows are the main safeguards; don't lower `required_detections` to 1.
- Emotion is not intent. Excitement, sport commentary or raised voices over a noisy game can look
  like anger.
- Monitored members in different voice channels can't all be analysed at once: RageGuard can only
  be in one voice channel per server.

---

## Project structure

```text
RageGuard/
├── bot/                              Rust Discord bot
│   ├── Cargo.toml
│   ├── Dockerfile                    optional bot image (also a Linux build/test toolchain)
│   ├── rageguard.toml                detection rules
│   ├── src/
│   │   ├── main.rs                   startup: .env, config, logging
│   │   ├── lib.rs
│   │   ├── app.rs                    shared AppContext, client wiring, graceful shutdown
│   │   ├── config.rs                 env + TOML loading and validation
│   │   ├── logging.rs
│   │   ├── commands/                 /anger-monitor, /anger-stop, /anger-status, /anger-config, /anger-setup
│   │   ├── discord/                  gateway events, serenity moderation backend, log-channel setup
│   │   ├── voice/                    receiver, buffer, resampler, wav, connection
│   │   ├── ai/                       AI service client + response validation
│   │   ├── detection/                sliding-window anger engine
│   │   ├── moderation/               permission checks, timeout / monitor-only execution
│   │   ├── monitor/                  session start/stop, voice supervisor, analysis worker
│   │   └── state/                    monitor registry, per-guild settings
│   └── tests/                        integration tests (+ common mocks)
├── ai-service/                       Python speech-emotion service
│   ├── app/                          main.py (API), model.py, audio.py, settings.py
│   ├── tests/
│   ├── requirements.txt
│   ├── requirements-dev.txt
│   └── Dockerfile
├── .vscode/                          tasks, launch configs, settings
├── .env.example
├── docker-compose.yml
└── README.md
```

[serenity]: https://github.com/serenity-rs/serenity
[songbird]: https://github.com/serenity-rs/songbird
[model]: https://huggingface.co/Dpngtm/wav2vec2-emotion-recognition
