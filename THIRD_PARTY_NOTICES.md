# Third-party notices

Murmur's own license is in `LICENSE` (Apache-2.0) and its attributions in
`NOTICE`. This file holds the notices for the optional speech helper.

## The speech helper, fotw-speechd

A Murmur bundle built with `MURMUR_SPEECHD=1` (#101) also contains
`Contents/Helpers/fotw-speechd` (FlyOnTheWall #243), a Swift executable that runs speech
models with Core ML. It is built from FlyOnTheWall's `helpers/fotw-speechd` and the libraries
below; a bundle built without it contains none of this.

### Muesli

Parts of fotw-speechd are ported from Muesli,
https://github.com/Muesli-HQ/muesli, at commit
97e455794112126a896cae17f5c829e8ffa820c8. Each ported file names its source.

````text
MIT License

Copyright (c) 2026 Pranav Hari

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
````

### FluidAudio

fotw-speechd compiles FluidAudio 0.17.3 (revision
a6b826ce9a09028fc685a4b32a8927ec920c3188),
https://github.com/FluidInference/FluidAudio, licensed under the Apache
License 2.0, whose text is Murmur's own `LICENSE`. FluidAudio
ships no NOTICE file at that revision. Its NeMo text normalizer is not linked.
FluidAudio's speaker clustering includes code derived from VBx
(https://github.com/BUTSpeechFIT/VBx), also under the Apache License 2.0.

### fastcluster

FluidAudio compiles fastcluster's C++ hierarchical clustering:

````text
Copyright:
  * Until package version 1.1.23: © 2011 Daniel Müllner <https://danifold.net>
  * All changes from version 1.1.24 on: © Google Inc. <https://www.google.com>
All rights reserved.

Redistribution and use in source and binary forms, with or without modification, are permitted provided that the following conditions are met:

  * Redistributions of source code must retain the above copyright notice, this list of conditions and the following disclaimer.
  * Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the following disclaimer in the documentation and/or other materials provided with the distribution.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
````

### Speech models

No model weights are in the app. fotw-speechd downloads them only when you
choose Transcription → Install Parakeet model… or run
`murmur speech install-parakeet`, never during dictation, pinned to an exact revision and checked file by
file against SHA-256 hashes. Their licenses:

| Model | Source | License |
|---|---|---|
| Parakeet Unified 0.6B (int8) | https://huggingface.co/FluidInference/parakeet-unified-en-0.6b-coreml | CC BY 4.0 |
| Silero VAD | https://huggingface.co/FluidInference/silero-vad-coreml | MIT |
| Parakeet realtime EOU 120M | https://huggingface.co/FluidInference/parakeet-realtime-eou-120m-coreml | NVIDIA Open Model License |
| Nemotron 3.5 ASR Streaming 0.6B | https://huggingface.co/FluidInference/Nemotron-3.5-ASR-Streaming-Multilingual-0.6b-CoreML | OpenMDW-1.1 |
