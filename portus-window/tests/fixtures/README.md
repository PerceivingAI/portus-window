# Deterministic Target Fixtures

This directory contains the source-controlled HTTP fixtures used by the Linux acceptance harness. They are served only from a loopback `ThreadingHTTPServer`; target acceptance does not depend on public websites for these behaviors.

## Web fixtures

- `interaction.html` — current v7 click/fill/key/wait/check behavior plus deterministic console messages.
- `navigation.html` — source page with a same-origin link at `#navigate`.
- `navigation-destination.html` — deterministic navigation destination/title/state.
- `observability.html` — explicit title-change, console-log, and console-error controls.

Network load failure remains deterministic without another page: the harness opens an unused loopback TCP port and requires the native load state to become `failed` with a load error.

## Generated media fixtures

`scripts/linux_acceptance/common.py::create_media_fixtures()` writes the media set into each acceptance evidence directory and emits `manifest.json` with exact byte size and SHA-256. The generator is stdlib-only and its hashes are locked by `scripts/linux_acceptance/test_harness.py`.

| Key | File | Purpose | SHA-256 |
| --- | --- | --- | --- |
| `png` | `fixture.png` | valid 2x2 PNG | `8e546007599fa6df13786602a501cc2933ae498bb8ed8ff0fa4664f551cad2be` |
| `jpeg` | `fixture.jpg` | valid 1x1 JPEG | `7bc59a570ffc9c16202780cf4b76b53bd4b46bd201f6489f64d4424c4cb13175` |
| `gif` | `fixture.gif` | valid 1x1 GIF89a | `1e85ec81b9800b4c443d39caca0d0926089a3ac201120db1ceb45b93789480b8` |
| `webp` | `fixture.webp` | valid 1x1 WebP | `05d3010ac1117dad75abd1617997d5d223ec88142422f6f8f123ed899cd434dc` |
| `wav` | `fixture.wav` | valid deterministic PCM WAV | `5b4e13f29d962374737093975f2f5a7e0959d6b16ea271a72b419413d78177c6` |
| `mismatched_jpeg` | `mismatched.jpg` | PNG bytes under a JPEG extension; admission must reject | `8e546007599fa6df13786602a501cc2933ae498bb8ed8ff0fa4664f551cad2be` |
| `fake_mp4` | `fake.mp4` | no MP4 signature; admission must reject | `235a12b26af2a70e13f44194471edb75176aeefb83a1272f6f7e96452a3fd66b` |
| `truncated_png` | `truncated.png` | PNG signature present but decoder-invalid/incomplete | `218ad85a233eff829618a6865ab681222b734c62d35a32b3eabd5c37d8945f86` |
| `truncated_wav` | `truncated.wav` | RIFF/WAVE signature present but decoder-invalid/incomplete | `1fe5a351bf0314c8a1840b023fd1e4cab3f0f123468940c241bd7bf20e989ab8` |

The first five are success fixtures. `mismatched_jpeg` and `fake_mp4` are admission-failure fixtures. `truncated_png` and `truncated_wav` intentionally pass the shallow signature boundary and are reserved for real WebKitGTK/GStreamer decoder-failure evidence.

No canonical video fixture is defined yet. The accepted container/video-codec/audio-codec combination must be chosen from the Artix GStreamer target evidence first; only then should one tiny versioned video fixture and its checksum be added.
