# The narrator runtime (staged at release)

The release build stages these files here before packaging:

- fable-tts — the narration helper (the only sherpa-linked
  artifact; built from crates/ufm-tts with --features engine)
- libonnxruntime.so, libsherpa-onnx-c-api.so,
  libsherpa-onnx-cxx-api.so — its runtime libraries

They are build products, not source: the .gitignore in this
directory keeps them out of the repo, and this README keeps the
directory (and therefore the bundle resources entry) valid for
ordinary dev builds.
