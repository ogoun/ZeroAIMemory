"""Собственное int8-квантование bge-m3 из fp32-эталона BAAI (задача 14 M2, гейт качества).

Воспроизводимо: вход — `target/models/bge-m3-fp32/` (`cargo xtask fetch-models --id bge-m3-fp32`,
файлы закреплены sha256 в deploy/models.toml), выход — каталог модели для zam-ml с
`model.onnx`, токенизатором, sparse-головой и `model.toml`. Версии: onnx 1.19.1,
onnxruntime 1.23.2 (`pip install onnx==1.19.1 onnxruntime==1.23.2` в venv внутри проекта,
например `.tmp/venv-quant`).

Методы (`--method`):
- `dynamic` — `quantize_dynamic`: веса MatMul int8 (`--per-channel` — масштаб на выходной
  канал, без `reduce_range`, полные 8 бит), активации квантуются во время работы
  (`DynamicQuantizeLinear`, один масштаб на тензор); таблица эмбеддингов (`Gather`) — fp32;
- `nbits` — блочное квантование только весов (`MatMulNBits`, `--bits` 8, блок `--block-size`;
  с `--gather` — и таблица эмбеддингов, `GatherBlockQuantized`; у BAAI-графа не срабатывает). `--accuracy-level 0` —
  вычисления в fp32 (деквантование весов), `4` — активации int8 поблочно (быстрее).

`--emb-fp16` — таблица эмбеддингов токенов в fp16 с `Cast` (иначе она fp32, ~1 ГБ).

Запуск (из корня репозитория):
  .tmp/venv-quant/Scripts/python.exe tools/quantize-bge-m3.py --method nbits --emb-fp16 \
      --out target/models/bge-m3-int8-wo
"""

import argparse
import shutil
from pathlib import Path

import onnx


def dynamic(src: Path, out: Path, per_channel: bool) -> None:
    from onnxruntime.quantization import QuantType, quantize_dynamic

    quantize_dynamic(
        model_input=str(src / "model.onnx"),
        model_output=str(out / "model.onnx"),
        weight_type=QuantType.QInt8,
        per_channel=per_channel,
        reduce_range=False,
        op_types_to_quantize=["MatMul"],
        extra_options={"MatMulConstBOnly": True},
    )


def nbits(src: Path, out: Path, bits: int, block: int, level: int, gather: bool) -> None:
    from onnxruntime.quantization.matmul_nbits_quantizer import (
        DefaultWeightOnlyQuantConfig,
        MatMulNBitsQuantizer,
    )

    ops = ("MatMul", "Gather") if gather else ("MatMul",)
    axes = (("MatMul", 0), ("Gather", 1)) if gather else (("MatMul", 0),)
    cfg = DefaultWeightOnlyQuantConfig(
        block_size=block,
        is_symmetric=True,
        accuracy_level=level,
        op_types_to_quantize=ops,
        quant_axes=axes,
        bits=bits,
    )
    model = onnx.load(str(src / "model.onnx"))  # с внешними данными
    q = MatMulNBitsQuantizer(
        model,
        block_size=block,
        is_symmetric=True,
        accuracy_level=level,
        op_types_to_quantize=ops,
        quant_axes=axes,
        algo_config=cfg,
    )
    q.process()
    onnx.save_model(q.model.model, str(out / "model.onnx"))


def embeddings_fp16(path: Path) -> None:
    """Таблица эмбеддингов токенов (250002 × 1024, ~1 ГБ в fp32) — в fp16 с `Cast` после
    `Gather`: размер модели меньше на ~0,5 ГБ, потеря точности — как у fp16-варианта."""
    import numpy as np
    from onnx import TensorProto, helper, numpy_helper

    m = onnx.load(str(path))
    init = next(i for i in m.graph.initializer if i.name.endswith("word_embeddings.weight"))
    arr = numpy_helper.to_array(init).astype(np.float16)
    init.CopyFrom(numpy_helper.from_array(arr, init.name))
    for idx, n in enumerate(m.graph.node):
        if n.op_type == "Gather" and n.input[0] == init.name:
            out = n.output[0]
            n.output[0] = out + "_fp16"
            cast = helper.make_node("Cast", [out + "_fp16"], [out], to=TensorProto.FLOAT)
            m.graph.node.insert(idx + 1, cast)
            break
    else:
        raise SystemExit("не найден Gather таблицы эмбеддингов")
    onnx.save_model(m, str(path))


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--src", default="target/models/bge-m3-fp32", help="каталог fp32-эталона")
    ap.add_argument("--out", required=True, help="каталог новой модели (target/models/<id>)")
    ap.add_argument("--method", choices=["dynamic", "nbits"], default="dynamic")
    ap.add_argument("--per-channel", action=argparse.BooleanOptionalAction, default=True)
    ap.add_argument("--bits", type=int, default=8)
    ap.add_argument("--block-size", type=int, default=32)
    ap.add_argument("--accuracy-level", type=int, default=0)
    ap.add_argument("--gather", action="store_true", help="квантовать и таблицу эмбеддингов")
    ap.add_argument("--emb-fp16", action="store_true", help="таблица эмбеддингов в fp16")
    args = ap.parse_args()

    src, out = Path(args.src), Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    if args.method == "dynamic":
        dynamic(src, out, args.per_channel)
        dyn = True
    else:
        nbits(src, out, args.bits, args.block_size, args.accuracy_level, args.gather)
        # веса квантованы, активации — нет (или поблочно внутри ядра): батч на вектор не влияет
        dyn = False
    if args.emb_fp16:
        embeddings_fp16(out / "model.onnx")
    for f in ("tokenizer.json", "sparse_linear.pt"):
        shutil.copyfile(src / f, out / f)
    (out / "model.toml").write_text(
        "# Описатель модели для zam-ml: собственное квантование fp32 BAAI (tools/quantize-bge-m3.py)\n"
        f'id = "{out.name}"\nrole = "embed"\ndevice = "cpu"\ndim = 1024\npooling = "cls"\n'
        'sparse_head = "sparse_linear.pt"\nmax_tokens_passage = 1024\nmax_tokens_query = 512\n'
        f"dynamic_quant = {'true' if dyn else 'false'}\n",
        encoding="utf-8",
    )
    size = (out / "model.onnx").stat().st_size / 1e6
    print(f"готово: {out / 'model.onnx'} ({size:.0f} МБ)")


if __name__ == "__main__":
    main()
