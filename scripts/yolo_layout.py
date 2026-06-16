#!/usr/bin/env python3
"""
yolo_layout.py - Learned document-layout detection (pluggable YOLO models).

A vision detector for the *original page layout*: finds Tables, Pictures, Titles,
paragraphs (Text), Section-headers, etc. as boxes on the rendered page — a learned
alternative to pdfplumber's ruled-line find_tables.

Models (selected by key):
  yolo26    - Armaggheddon/yolo26-document-layout (DocLayNet, nano)
  doclayout - opendatalab DocLayout-YOLO trained on DocStructBench (YOLOv10);
              stronger on diverse real-world docs (papers, financial, textbooks)

Lazy: each model's package + weights load on first use. NEVER hardcode class names
— read them from the loaded model (`model.names`); the order differs per model.
"""
from __future__ import annotations

MODELS = {
    "yolo26": {"repo": "Armaggheddon/yolo26-document-layout",
               "file": "yolo26n_doc_layout.pt", "imgsz": 1280, "loader": "ultralytics"},
    "doclayout": {"repo": "juliozhao/DocLayout-YOLO-DocStructBench",
                  "file": "doclayout_yolo_docstructbench_imgsz1024.pt", "imgsz": 1024, "loader": "doclayout"},
}

_models = {}


def _load(key: str):
    if key not in _models:
        cfg = MODELS[key]
        from huggingface_hub import hf_hub_download
        path = hf_hub_download(repo_id=cfg["repo"], filename=cfg["file"], repo_type="model")
        if cfg["loader"] == "doclayout":
            from doclayout_yolo import YOLOv10
            _models[key] = YOLOv10(path)
        else:
            from ultralytics import YOLO
            _models[key] = YOLO(path)
    return _models[key]


def detect(pil_img, res: float, key: str = "yolo26", conf: float = 0.25):
    """Run layout detection; return elements with bboxes in PDF points (top-left
    origin, matching pdfplumber), sorted by reading order."""
    cfg = MODELS[key]
    model = _load(key)
    names = model.names  # authoritative class id -> label, from the weights
    r = model.predict(pil_img, imgsz=cfg["imgsz"], conf=conf, verbose=False)[0]
    scale = 72.0 / res  # image pixels -> PDF points
    out = []
    for box, cls, cf in zip(r.boxes.xyxy.tolist(), r.boxes.cls.tolist(), r.boxes.conf.tolist()):
        x0, y0, x1, y1 = (v * scale for v in box)
        out.append({"label": str(names[int(cls)]), "cls": int(cls), "conf": round(float(cf), 3),
                    "bbox": [x0, y0, x1, y1]})
    out.sort(key=lambda e: (round(e["bbox"][1] / 10), e["bbox"][0]))
    return out
