#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["surya-ocr", "flask>=3"]
# ///
"""
surya_layout.py - SIDECAR for Surya layout detection (isolated env).

Two modes:
  one-shot:   uv run scripts/surya_layout.py page.png
              -> prints JSON layout boxes (PIXEL coords) for one image.
  server:     uv run scripts/surya_layout.py --serve 5071
              -> a long-lived HTTP server that loads the model ONCE and answers
                 POST /layout {"path": "page.png"} -> the same JSON. GET /health
                 returns 200 once the model is loaded.

The main server runs the server mode so the model stays warm (the per-call cost
drops from "spawn llama-server + load model" to just inference). Surya is VLM-based
and needs a backend (llama.cpp on CPU/Apple Silicon); first load downloads weights.
Boxes: [{"label": "Table", "conf": 0.98, "bbox": [x0, y0, x1, y1]}], pixel coords.
"""
import json
import sys

from PIL import Image


def _boxes(predictor, img):
    out = []
    for result in predictor([img]):
        for b in result.bboxes:
            conf = getattr(b, "confidence", None)
            out.append({"label": str(b.label),
                        "conf": round(float(conf), 3) if conf is not None else 1.0,
                        "bbox": [float(v) for v in b.bbox]})
    return out


def serve(port: int) -> None:
    from flask import Flask, jsonify, request
    from surya.layout import LayoutPredictor
    predictor = LayoutPredictor()           # loads the model / spawns llama-server now
    app = Flask(__name__)

    @app.get("/health")
    def health():
        return "ok"

    @app.post("/layout")
    def layout():
        img = Image.open(request.get_json()["path"]).convert("RGB")
        return jsonify(_boxes(predictor, img))

    print(f"surya layout server ready on :{port}", flush=True)
    app.run(host="127.0.0.1", port=port, threaded=False)


def main() -> None:
    if len(sys.argv) >= 3 and sys.argv[1] == "--serve":
        serve(int(sys.argv[2])); return
    from surya.layout import LayoutPredictor
    img = Image.open(sys.argv[1]).convert("RGB")
    print(json.dumps(_boxes(LayoutPredictor(), img)))


if __name__ == "__main__":
    main()
