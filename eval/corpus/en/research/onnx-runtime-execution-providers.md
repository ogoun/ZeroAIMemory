# ONNX Runtime execution providers: notes and gotchas

## What an EP is

ONNX Runtime splits a model graph between **execution providers** (EPs). Each EP claims the nodes it can run; the rest falls back to the next provider in the list, ultimately the CPU EP. The order passed at session creation is a priority list:

```python
import onnxruntime as ort
sess = ort.InferenceSession(
    "model.onnx",
    providers=["CUDAExecutionProvider", "CPUExecutionProvider"],
)
print(sess.get_providers())   # what was actually enabled
```

## Common providers

| EP | Hardware | Notes |
|---|---|---|
| CPU | any x86/ARM | default; MLAS kernels, int8 via VNNI where available |
| CUDA | NVIDIA | needs matching CUDA and cuDNN major versions for the ORT build |
| TensorRT | NVIDIA | builds engines per input shape; slow first run, engine cache recommended |
| DirectML | any DX12 GPU on Windows | convenient, but Microsoft has moved it to maintenance mode |
| OpenVINO | Intel CPU/GPU/NPU | separate package |
| CoreML | Apple | macOS/iOS |

## Gotchas I hit or read about

- **Silent fallback.** If the CUDA EP fails to load (missing `cudnn` DLL, version mismatch), the session is still created on the CPU unless you check `get_providers()` or enable verbose logging. Always assert the provider list after creation.
- **Partial placement.** Unsupported ops run on the CPU, and every boundary inserts Memcpy nodes. A few CPU nodes in the middle of a transformer can erase the GPU speedup. Verbose logs show the node assignment.
- **Dynamic shapes.** CUDA EP handles them fine; TensorRT rebuilds an engine for new shape ranges unless you give explicit optimization profiles.
- **Memory arena.** The CUDA EP grows an arena and does not give memory back by default; `arena_extend_strategy` and `gpu_mem_limit` control it.
- **cuDNN algorithm search.** `cudnn_conv_algo_search` matters for conv nets, barely for transformers.
- **Quantized models on GPU.** int8 QDQ models may run slower on CUDA than fp16 because not all integer kernels exist; fp16 conversion is the usual GPU path.
- **Graph optimizations.** `graph_optimization_level` (basic/extended/all) and saving the optimized model offline shortens session start.
- **IOBinding.** For GPU, binding inputs/outputs to device memory avoids host copies on every call.

## Open questions

- Is the CUDA EP with fp16 consistently ahead of TensorRT for small-batch embedding workloads once warm-up cost is counted?
- How stable is EP behaviour across ORT minor versions with the same model file?
