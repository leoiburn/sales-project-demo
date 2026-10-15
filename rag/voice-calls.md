---
feature: voice-calls
files: [web/index.html, crates/automotrix/src/bin/server.rs, docker-compose.yml]
status: committed
last_reviewed: 2026-10-10
---

# Voice calls

## What it does

The 📞 **Call** button lets a customer talk to the bot. Audio is transcribed and synthesized by
self-hosted containers, so it never leaves the machine.

## How it works

1. **Start:** asks for the microphone, plays a ring tone, then listens.
2. **Turn detection** (`listen()` in the page): records with `MediaRecorder` and measures loudness
   every 100 ms. A turn ends after **1.2 s of quiet** once speech was heard (loudness above 6), or
   after **30 s**. If nothing was heard it listens again.
3. **Speech to text:** the audio (webm/opus) is posted to `POST /api/stt` (limit 10 MB). The server
   relays it to Whisper at `STT_URL/asr?output=json&vad_filter=true` (the
   `openai-whisper-asr-webservice` image, `faster_whisper`, model `STT_MODEL`, default `base`) and
   returns `{text, language}`.
4. The text is sent through `/api/chat`, the same as typed text.
5. **Text to speech:** `speak()` removes links and markdown and **replaces every price with "the
   price shown in the chat"** (or "el precio que aparece en el chat"). Prices are only shown, never
   spoken. Then `POST /api/tts` asks Kokoro (`TTS_URL/v1/audio/speech`) for MP3, using voice
   `ef_dora` for Spanish and `af_heart` otherwise. If that fails, the page falls back to the
   browser's own voice (`es-MX` or `en-US`).
6. When playback ends, it listens again. **Hang up** stops everything.

## Configuration

`STT_URL` (default `http://stt:9000`), `TTS_URL` (default `http://tts:8880`), `STT_MODEL`. Both
containers have no published ports; only the app reaches them.

## Gotchas

- The loudness threshold is fixed. A noisy room can end turns too early or never.
- The booking form doesn't work by voice, which is why the system prompt allows the slots-and-tools
  path for voice calls.
- `/api/stt` and `/api/tts` don't check a session id and are open to anyone who can reach the server.
