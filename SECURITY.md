# Security and privacy

Murmur is an early macOS developer preview. See the [README](README.md) for
current behavior and the [QA checklist](docs/QA.md) for outstanding native checks.

## Reporting a vulnerability

Use [GitHub private vulnerability reporting](https://github.com/nolanmak/Murmur/security/advisories/new)
under the repository's Security tab. If it is unavailable, open a minimal issue
requesting a private reporting channel, without exploit details or sensitive information. Do not post
credentials, recordings, transcripts, signing keys, or private machine paths.
No formal response SLA is offered.

## Handling credentials and audio

Keep your Deepgram API key in the ignored local `.env` file, the process environment,
or the supported macOS Keychain entry. Never commit it. Audio is sent directly to
Deepgram using your key while dictating; provider API charges apply. Local audio
and transcripts are held in memory, with clipboard behavior described in the README.

Before sharing diagnostics, remove private text and credentials. Use synthetic
examples for reproduction steps. If a key is exposed, revoke or rotate it with
the provider; deleting a file or issue alone does not invalidate the key.
