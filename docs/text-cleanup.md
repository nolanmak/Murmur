# Optional text cleanup

Cleanup is off by default and independent of speech transcription. Raw text and
local snippets need no language model. Explicitly enabled cleanup sends only the
winning utterance text, its whitespace tokens and selected language to your
configured compatible provider. It never receives clipboard/screen contents or
unspoken vocabulary/snippets. Provider handling and charges follow that provider's
policy. A loopback proxy can forward text to a hosted model.

## Configure and opt in

Use the app bundle's `Contents/MacOS/murmur` executable or your developer binary:

```sh
murmur cleanup configure https://provider.example/v1/chat/completions YOUR_MODEL MURMUR_CLEANUP_API_KEY
murmur cleanup status
murmur cleanup on
murmur cleanup off
```

Replace the example endpoint and model with your chosen provider. Set the named
credential variable in the app's process environment or existing configured `.env`
file (`FOTW_ENV_FILE`, beside the app bundle, or developer working directory).
Supply a **variable name**, never a key value, to the CLI. Cleanup has no Keychain
fallback. Configuring a destination leaves cleanup off; enabling it is a separate
choice. The Text cleanup menu displays the model/destination and toggles the
reviewed configuration. If another process changes it, review the refreshed menu
before enabling. A missing key keeps raw text; it never opens an authentication
prompt or changes ASR settings.

Settings are separate credential-free `cleanup.json` and `network-policy.json`
files under `~/Library/Application Support/Murmur`. Saves are validated and atomic;
cooperating writers use nonblocking locks. Cleanup's default additional deadline is
1500 ms, configurable from 100–5000 ms in the versioned settings. This includes
credential-file lookup and native client preparation. One service owns that work
through actual cleanup; timeout returns raw text without cancelling dictation or
starting a second blocked initializer. Cancel, timeout and dropping the request
also prevent late preparation from sending text. Failed saves
retain the prior file. Unknown/malformed cleanup configuration keeps raw text.
Invalid network policy blocks dictation before capture or credential resolution.

## Offline-all

Choose the native **Offline-all: local speech, no cleanup** control, or:

```sh
murmur privacy offline-all
murmur privacy configured
```

Offline-all uses the configured local Whisper endpoint without resolving a cloud
speech key, and bypasses cleanup entirely, including loopback proxies. Install and
start the trusted local Whisper runtime first. Restoring configured providers does
not enable cleanup unless you already opted in. Native settings are disabled during
dictation; CLI changes apply to the next attempt. Cancel an active attempt before
changing its network policy.

## Editing and delivery

The provider proposes a strict version-1 edit plan, not replacement prose. The app
renders from original source tokens. It accepts English `um`/`uh`/`erm` removals,
conservative single-token corrections with `sorry`, `correction` or `I mean`, and
list/paragraph breaks. Arbitrary clause deletion, reordering, generated words,
negation deletion, unsupported languages for deletion, quotations and literal
material fall back to raw text. Empty/invalid plans, refusals, tool calls,
truncated/oversized responses, network errors and deadlines retain raw text.
Locally configured recognition hints are protected from deletion without being
sent to the cleanup provider. Corrections and punctuation outside these
conservative rules remain unchanged.

Exact deliberate snippet cues expand locally and bypass cleanup. Microphone Stop
and cleanup must succeed before optional text processing. Escape/cancellation,
generation fencing and the original-field/window checks still govern delivery.
Local multiline formatting is retained for **Copy Last Transcript**, without an
automatic paste or Enter key. Remote cleanup retains raw text when formatting
would violate its existing single-line/printable paste policy.

## Corpus and evidence

`tests/fixtures/cleanup/v1.json` is synthetic, versioned golden/adversarial data.
Run its validator and timing report without any provider or credential access:

```sh
cargo run --locked --example cleanup_eval
```

For a separately opted-in provider, an explicit `--provider` run evaluates that
configured model on the same synthetic corpus. It reports golden matches, raw
fallbacks and elapsed time without logging transcript content. The agent did not
run provider mode. Passing the deterministic validator is not proof of general
semantic fidelity or real-model quality; a provider that returns unchanged text
can be faithful while offering no cleanup utility. Real-model meaning preservation
and total release-to-paste latency remain unverified release gates in issue #8.
Cleanup stays off by default.

Actual tests first exposed calls while disabled/offline, unbounded waiting,
late-success cancellation, missing edit validation, credential access under
Offline-all, premature snippet expansion, a contracted-negation deletion, a vocabulary name
removed as a filler and an unbounded credential initializer. The
regression suites cover source-preserving edits, HTTP privacy/failure behavior,
configuration persistence, generation/focus changes and owned AppKit menu actions.
The AppKit fixture has no Shell, event tap, microphone, credential, clipboard or
foreground-window capability. It checks real native menu state/action routing;
it does not establish physical typing, IME or app compatibility.

Protocol references: [Chat Completions request and response](https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create)
and [structured outputs](https://developers.openai.com/api/docs/guides/structured-outputs?api-mode=chat).
Compatible providers must support the requested strict JSON schema. Redirects,
ambient proxies and plaintext remote endpoints are refused; no fallback protocol
silently weakens validation. `store: false` and a completion-token cap accompany
requests, and the response is bounded to 64 KiB.
