# Architecture and scope

## M1: first macOS dictation slice

Native Space key down/up events with a 350 ms hold threshold → pure Dictation state machine → microphone worker →
lock-free bounded ring → FlyOnTheWall resampler → Deepgram WebSocket → final
transcript → original Accessibility target check → AXSelectedText insertion.

AppKit and retained AX objects stay on the main thread. Audio and credential
resolution run off-thread. The callback performs no allocation or network work.
Session generations prevent cancelled or stale work from reaching another target.
A recording is never launched automatically. Escape and Space chords cancel pending
work; a 120-second limit bounds missing-release events. The indicator cannot take focus.

## M2: writing features

Optional meaning-preserving cleanup, vocabulary, snippets, language controls and
explicit recovery/history. These have separate issues and are not enabled in the MVP.

## M3: distribution and portability

Signing/notarization, native compatibility QA and eventual public-source readiness.
Additional STT providers and Windows/Linux adapters follow the same tested boundaries.
No public repository or release is authorized by the current private-development setup.

## Protocol decision

Deepgram CloseStream requests final audio processing and then emits Metadata.
Only is_final results enter the normalizer; its committed chunks are drained after
Metadata. An unexpected socket close, timeout or cancellation fails without insertion.
The long-meeting transport in FlyOnTheWall closes its client socket immediately
on close; the new dictation adapter intentionally waits for the provider to finish.

## References

- Product workflow: https://wisprflow.ai/features
- Hold/release and Escape: https://docs.wisprflow.ai/articles/6409258247-starting-your-first-dictation
- Finalization: https://developers.deepgram.com/docs/close-stream
- Optional Finalize acknowledgement: https://developers.deepgram.com/docs/finalize

## Space shortcut

A quick Space tap replays its original native keydown before keyup. If another
character arrives first, that deferred Space is replayed before the character.
Long holds suppress repeated spaces and start dictation once; release finishes.
Command/Option/Control/Shift/Fn-modified Space is passed through. Fn itself is
not registered, so Wispr Flow can keep using it.
