"""Download multilingual-e5-small (ONNX) at a pinned revision and build an int8 variant.

Files land in data/models/multilingual-e5-small/ (gitignored):
  model.fp32.onnx   — upstream ONNX export, sha256-verified
  model.int8.onnx   — dynamic int8 quantization of the above (portable, built locally)
  tokenizer.json    — sha256-verified
  manifest.json     — revision and checksums, read by the service for model_version

Usage: python scripts/fetch_model.py [--out DIR] [--skip-int8]
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
import urllib.request
from pathlib import Path

REPO = "intfloat/multilingual-e5-small"
REVISION = "614241f622f53c4eeff9890bdc4f31cfecc418b3"
FILES = {
    # local name: (path in repo, sha256)
    "model.fp32.onnx": (
        "onnx/model.onnx",
        "ca456c06b3a9505ddfd9131408916dd79290368331e7d76bb621f1cba6bc8665",
    ),
    "tokenizer.json": (
        "onnx/tokenizer.json",
        "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39",
    ),
}
DEFAULT_OUT = Path(__file__).resolve().parents[2] / "data" / "models" / "multilingual-e5-small"


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def download(url: str, dest: Path) -> None:
    tmp = dest.with_suffix(dest.suffix + ".part")
    with urllib.request.urlopen(url) as resp, tmp.open("wb") as out:
        total = int(resp.headers.get("Content-Length", 0))
        done = 0
        while chunk := resp.read(1 << 20):
            out.write(chunk)
            done += len(chunk)
            if total:
                print(f"\r  {dest.name}: {done / total:6.1%} of {total / 1e6:.0f} MB", end="")
    print()
    tmp.rename(dest)


def fetch(out: Path) -> None:
    out.mkdir(parents=True, exist_ok=True)
    for name, (repo_path, expected) in FILES.items():
        dest = out / name
        if dest.exists() and sha256(dest) == expected:
            print(f"  {name}: present, checksum ok")
            continue
        url = f"https://huggingface.co/{REPO}/resolve/{REVISION}/{repo_path}"
        print(f"  downloading {url}")
        download(url, dest)
        actual = sha256(dest)
        if actual != expected:
            dest.unlink()
            sys.exit(f"checksum mismatch for {name}: {actual} != {expected}")


def quantize(out: Path) -> None:
    dest = out / "model.int8.onnx"
    if dest.exists():
        print("  model.int8.onnx: present")
        return
    try:
        from onnxruntime.quantization import QuantType, quantize_dynamic
    except ImportError:
        sys.exit("int8 quantization needs the tools extra: pip install -e 'embedder[tools]'")
    print("  quantizing to int8 (dynamic, per-channel weights)…")
    quantize_dynamic(
        model_input=out / "model.fp32.onnx",
        model_output=dest,
        weight_type=QuantType.QInt8,
        per_channel=True,
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--skip-int8", action="store_true")
    args = parser.parse_args()

    print(f"{REPO}@{REVISION[:7]} → {args.out}")
    fetch(args.out)
    if not args.skip_int8:
        quantize(args.out)
    manifest = {
        "repo": REPO,
        "revision": REVISION,
        "files": {
            p.name: sha256(p)
            for p in sorted(args.out.glob("*"))
            if p.suffix in {".onnx", ".json"} and p.name != "manifest.json"
        },
    }
    (args.out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print("done")


if __name__ == "__main__":
    main()
