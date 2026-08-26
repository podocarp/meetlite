# Refactor Backlog

## Recording core

- Split `src/recording/mod.rs` into focused modules for frames, buffers, mixing, output artifacts, metadata, and session orchestration.
- Keep `recording::record` and `recording::record_with_samples` as thin public entrypoints.
- Move platform/system-audio selection behind recording adapters so the recorder only starts selected sources and mixes frames.

## macOS capture boundary

- Move LaunchServices, TCC app discovery, capture-agent IPC, and Core Audio FFI out of the generic recording core.
- Keep the macOS capture app as the permission boundary; do not move mixing, WAV output, transcription, or metadata into the app bundle.
- Consider a dedicated `macos` or `platform/macos` module containing agent client, hidden agent command, app path resolution, and Core Audio bridge.

## Artifacts and metadata

- Centralize generated artifact names: `audio.wav`, `metadata.json`, `transcript.json`, `transcript.jsonl`, `summary.md`, and `chunks/`.
- Move output directory cleanup and overwrite policy into an artifact/session module.
- Avoid having transcription mutate recording metadata directly; let a pipeline layer combine recording and transcription results.

## Pipeline orchestration

- Add a pipeline layer for `record`, `record --transcribe`, `record --summarize`, file transcription, and file summarization.
- Keep `src/main.rs` as thin CLI dispatch.
- Make live transcription consume recorder sample events through a stable interface rather than reaching into recording internals.

## Provider HTTP clients

- Share auth/header/environment handling between STT and LLM clients.
- Consider a common OpenAI-compatible provider helper for endpoint construction, request timeouts, and error normalization.

## Platform expansion

- Add Windows support as a new adapter without changing recorder core behavior.
- Reserve Cargo features for optional capabilities, not operating-system selection.

## Project hygiene

- Restore `PLAN.md` or remove references to it from `README.md` and `AGENTS.md`.
- Keep release/install behavior documented in one canonical location to avoid drift between scripts, workflows, and README text.
