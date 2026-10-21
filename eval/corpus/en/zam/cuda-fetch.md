# Installing CUDA libraries for the GPU kit

The GPU kit contains ONNX Runtime with the CUDA EP but deliberately no NVIDIA libraries (owner
decision: they would blow up the archive). Install them with:

```
zam-server cuda fetch --config config.toml
```

- Downloads NVIDIA redist archives from `developer.download.nvidia.com` as pinned in
  `deploy/cuda.toml`; size and SHA-256 are verified. Roughly 1.9 GB of archives on Windows and
  2.2 GB on Linux.
- Libraries land in `cuda/` next to ONNX Runtime (by default next to `zam-server`): cudart,
  cublas, cublasLt, cufft and cuDNN 9, plus curand and nvrtc on Linux. Each component gets a
  stamp file `.<component>.sha256`.
- A rerun skips an archive only if its stamp matches the manifest, so after upgrading
  `zam-server` with a new `cuda.toml` the old libraries are replaced.
- Offline: unpack `zam-cuda-<ver>-<os>-x64` (same version as the server) into the server
  directory. Running `cuda fetch` afterwards should report zero downloads (`"downloaded": 0`
  with `--json`) — a cheap way to verify the unpack.
- Needs an NVIDIA driver with CUDA 13 support (R580 branch or newer). The CUDA Toolkit itself is
  not needed. In the CPU kit the command refuses to run.

`ml.device`: `auto` uses CUDA if it initialised, else CPU; `cuda` also warns on fallback;
`cpu` never tries CUDA.
