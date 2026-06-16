#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["surya-ocr"]
# ///
"""
surya_layout.py - SIDECAR for Surya layout detection.

Run out-of-process (its heavy, foundation-model deps never touch the main server's
env). Reads one page-image path, prints layout boxes as JSON in PIXEL coordinates:

    [{"label": "Table", "conf": 0.98, "bbox": [x0, y0, x1, y1]}, ...]

  uv run scripts/surya_layout.py page.png

Surya (datalab-to/surya) is a VLM-based detector and needs an inference backend
(vllm on NVIDIA, or llama.cpp on CPU/Apple Silicon); the first run downloads the
model. The caller treats any failure as "layout unavailable".
"""
import json
import sys

from PIL import Image


def main() -> None:
    img = Image.open(sys.argv[1]).convert("RGB")
    from surya.layout import LayoutPredictor
    predictor = LayoutPredictor()
    out = []
    for result in predictor([img]):
        for b in result.bboxes:
            conf = getattr(b, "confidence", None)
            out.append({"label": str(b.label),
                        "conf": round(float(conf), 3) if conf is not None else 1.0,
                        "bbox": [float(v) for v in b.bbox]})
    print(json.dumps(out))


if __name__ == "__main__":
    main()
