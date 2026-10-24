# Stereo capture conversion

The capture converter resamples interleaved input before downmixing. The shared
streaming resampler retains partial input frames between ring drains, so a drain
ending between left and right samples does not discard a frame or shift channel
alignment. End padding is counted in frames and includes every input channel.

A behavioral regression compares a whole stereo waveform with the same waveform
drained in odd-sized 319-sample chunks, including finalization. The old path
produced 7040 output samples instead of 7200. The correction must preserve both
output length and waveform within the test tolerance.

Run `cargo test --locked --lib stereo_conversion_preserves` and the full workspace
suite. Tests use synthetic PCM and open no microphone or provider connection.

This prepares Murmur for actual multichannel format reporting in FlyOnTheWall
PR #139. The shared producer revision is deliberately unchanged in this increment;
upgrade it in a separate compatibility-verified change after that backend merges.
Physical USB/Bluetooth route and format changes remain acceptance gates.
