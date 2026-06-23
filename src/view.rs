//! Render a store's artifacts to a single self-contained HTML view — "see the
//! details of the new setup": per-page layout schematic (colored bboxes), the
//! extracted tables, the structured text, OCR markers, and the artifact DAG.
//!
//! No server, no JS framework — `quarry view <store>` writes one HTML file you open
//! in a browser. Everything is read from the typed artifacts (bboxes, cells,
//! elements), so the view is exactly what's in the store.

use crate::artifact::*;
use crate::core::{BBox, Provenance, SourceAnchor};
use std::collections::BTreeMap;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn loc(anchor: &SourceAnchor) -> Option<(u32, BBox)> {
    match anchor {
        SourceAnchor::Pdf { page, bbox, .. } => Some((*page, *bbox)),
        _ => None,
    }
}

fn role_color(r: DocRole) -> &'static str {
    match r {
        DocRole::Title | DocRole::Heading => "#2563eb",
        DocRole::Paragraph => "#93c5fd",
        DocRole::Caption => "#0d9488",
        DocRole::ListItem => "#16a34a",
        DocRole::Other => "#9ca3af",
    }
}

const C_TABLE: &str = "#f59e0b";
const C_IMAGE: &str = "#ef4444";

struct Box2 {
    page: u32,
    b: BBox,
    color: &'static str,
    label: String,
}

fn short(id: &str) -> &str {
    id.get(..20).unwrap_or(id)
}

/// Render the whole store to one HTML document.
pub fn render_store(artifacts: &[Box<dyn Artifact>], store: &str) -> String {
    // typed views
    let docs: Vec<&StructuredDoc> =
        artifacts.iter().filter_map(|a| a.as_any().downcast_ref()).collect();
    let htmls: Vec<&HtmlTable> = artifacts.iter().filter_map(|a| a.as_any().downcast_ref()).collect();
    let dbs: Vec<&DbTable> = artifacts.iter().filter_map(|a| a.as_any().downcast_ref()).collect();
    let imgs: Vec<&ImageRef> = artifacts.iter().filter_map(|a| a.as_any().downcast_ref()).collect();
    let regs: Vec<&Region> = artifacts.iter().filter_map(|a| a.as_any().downcast_ref()).collect();
    let enrs: Vec<&Enrichment> = artifacts.iter().filter_map(|a| a.as_any().downcast_ref()).collect();

    // counts by kind
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for a in artifacts {
        *counts.entry(format!("{:?}", a.kind())).or_default() += 1;
    }
    let summary = counts
        .iter()
        .map(|(k, n)| format!("<span class=chip>{k} × {n}</span>"))
        .collect::<Vec<_>>()
        .join(" ");

    // ---- page-layout boxes ----
    let mut boxes: Vec<Box2> = Vec::new();
    for sd in &docs {
        for el in &sd.elements {
            if let Some((page, b)) = loc(&el.anchor) {
                boxes.push(Box2 {
                    page,
                    b,
                    color: role_color(el.role),
                    label: format!("{:?}: {}", el.role, el.text.chars().take(50).collect::<String>()),
                });
            }
        }
    }
    for t in &htmls {
        if let Some((page, b)) = loc(t.meta.provenance.anchor()) {
            boxes.push(Box2 { page, b, color: C_TABLE, label: format!("table {}×{}", t.n_rows, t.n_cols) });
        }
    }
    for im in &imgs {
        if let Some((page, b)) = loc(im.meta.provenance.anchor()) {
            boxes.push(Box2 { page, b, color: C_IMAGE, label: format!("{:?}", im.status) });
        }
    }
    // figure regions add image boxes not otherwise shown
    for r in &regs {
        if matches!(r.role(), RegionRole::Figure) {
            boxes.push(Box2 { page: r.page(), b: r.bbox(), color: C_IMAGE, label: "figure".into() });
        }
    }

    let mut pages: Vec<u32> = boxes.iter().map(|x| x.page).collect();
    pages.sort_unstable();
    pages.dedup();

    let mut pages_html = String::new();
    for &pg in &pages {
        let on: Vec<&Box2> = boxes.iter().filter(|x| x.page == pg).collect();
        let (mut w, mut h) = (1.0f32, 1.0f32);
        for x in &on {
            w = w.max(x.b.x1);
            h = h.max(x.b.y1);
        }
        let disp_w = 460.0;
        let disp_h = (disp_w * h / w).clamp(60.0, 1400.0);
        let mut rects = String::new();
        for x in &on {
            let (rx, ry) = (x.b.x0.min(x.b.x1), x.b.y0.min(x.b.y1));
            let (rw, rh) = ((x.b.x1 - x.b.x0).abs().max(1.0), (x.b.y1 - x.b.y0).abs().max(1.0));
            rects.push_str(&format!(
                "<rect x='{rx:.1}' y='{ry:.1}' width='{rw:.1}' height='{rh:.1}' \
                 fill='{c}' fill-opacity='0.18' stroke='{c}' stroke-width='1.2'>\
                 <title>{lbl}</title></rect>",
                c = x.color,
                lbl = esc(&x.label)
            ));
        }
        pages_html.push_str(&format!(
            "<div class=page><div class=pglabel>page {pg} — {n} elements</div>\
             <svg viewBox='0 0 {w:.0} {h:.0}' width='{disp_w:.0}' height='{disp_h:.0}' \
             preserveAspectRatio='xMidYMin meet' style='background:#fff;border:1px solid #ddd'>{rects}</svg></div>",
            n = on.len()
        ));
    }

    // ---- tables (prefer DbTable; fall back to HtmlTable) ----
    let mut tables_html = String::new();
    if !dbs.is_empty() {
        for db in &dbs {
            let head = db
                .columns
                .iter()
                .zip(&db.dtypes)
                .map(|(c, d)| format!("<th>{}<br><small>{:?}</small></th>", esc(c), d))
                .collect::<Vec<_>>()
                .join("");
            let body = db
                .rows
                .iter()
                .take(50)
                .map(|row| {
                    let tds = row.iter().map(|c| format!("<td>{}</td>", esc(c))).collect::<String>();
                    format!("<tr>{tds}</tr>")
                })
                .collect::<String>();
            tables_html.push_str(&format!(
                "<div class=card><div class=cardh>DbTable — {}×{} <small>← {}</small></div>\
                 <table><thead><tr>{head}</tr></thead><tbody>{body}</tbody></table>\
                 {more}</div>",
                db.n_cols(),
                db.n_rows(),
                short(&db.source.0),
                more = if db.rows.len() > 50 {
                    format!("<small>… {} more rows</small>", db.rows.len() - 50)
                } else {
                    String::new()
                }
            ));
        }
    } else {
        for t in &htmls {
            tables_html.push_str(&format!(
                "<div class=card><div class=cardh>HtmlTable — {}×{} <small>(not yet materialized)</small></div></div>",
                t.n_rows, t.n_cols
            ));
        }
    }

    // ---- structured text ----
    let mut text_html = String::new();
    for sd in &docs {
        for el in &sd.elements {
            let t = esc(&el.text);
            text_html.push_str(&match el.role {
                DocRole::Title => format!("<h2>{t}</h2>"),
                DocRole::Heading => format!("<h3>{t}</h3>"),
                DocRole::ListItem => format!("<li>{t}</li>"),
                DocRole::Caption => format!("<p class=cap><em>{t}</em></p>"),
                _ => format!("<p>{t}</p>"),
            });
        }
    }

    // ---- enrichments ----
    let mut enrich_html = String::new();
    for e in &enrs {
        enrich_html.push_str(&format!(
            "<div class=card><div class=cardh>{} <small>← {}</small></div><pre>{}</pre></div>",
            esc(&e.kind),
            short(&e.source.0),
            esc(&e.text)
        ));
    }
    if enrich_html.is_empty() {
        enrich_html = "<p class=muted>none — run <code>quarry enrich</code></p>".into();
    }

    // ---- artifact DAG (lineage from provenance) ----
    let kind_of: BTreeMap<String, String> =
        artifacts.iter().map(|a| (a.id().0.clone(), format!("{:?}", a.kind()))).collect();
    let mut edges = String::new();
    for a in artifacts {
        if let Provenance::Derived { parents, .. } = a.provenance() {
            for p in parents {
                let pk = kind_of.get(&p.0).cloned().unwrap_or_else(|| "?".into());
                edges.push_str(&format!(
                    "<div class=edge><b>{:?}</b> <code>{}</code> ← {} <code>{}</code></div>",
                    a.kind(),
                    short(&a.id().0),
                    pk,
                    short(&p.0)
                ));
            }
        }
    }
    if edges.is_empty() {
        edges = "<p class=muted>all artifacts are sources (no derivations yet)</p>".into();
    }

    format!(
        "<!doctype html><html><head><meta charset=utf-8><title>Quarry — {store}</title>
<style>
 body{{font:14px/1.5 -apple-system,system-ui,sans-serif;margin:0;color:#1f2937;background:#f9fafb}}
 header{{position:sticky;top:0;background:#111827;color:#fff;padding:12px 20px;z-index:9}}
 header h1{{margin:0 0 6px;font-size:16px}} header a{{color:#93c5fd;margin-right:14px;text-decoration:none;font-size:13px}}
 .chip{{background:#374151;border-radius:10px;padding:1px 8px;margin-right:6px;font-size:12px}}
 section{{padding:18px 20px;border-bottom:1px solid #e5e7eb}}
 h2.s{{font-size:15px;margin:0 0 12px;color:#374151}}
 .pages{{display:flex;flex-wrap:wrap;gap:16px}} .page{{}} .pglabel{{font-size:12px;color:#6b7280;margin-bottom:4px}}
 .legend span{{margin-right:14px;font-size:12px}} .legend i{{display:inline-block;width:11px;height:11px;border-radius:2px;margin-right:4px;vertical-align:-1px}}
 .card{{background:#fff;border:1px solid #e5e7eb;border-radius:6px;margin-bottom:14px;overflow:hidden}}
 .cardh{{background:#f3f4f6;padding:6px 10px;font-weight:600;font-size:13px}}
 table{{border-collapse:collapse;width:100%;font-size:13px}} th,td{{border:1px solid #e5e7eb;padding:4px 8px;text-align:left;vertical-align:top}}
 th{{background:#fafafa}} small{{color:#6b7280;font-weight:400}}
 .doc{{background:#fff;border:1px solid #e5e7eb;border-radius:6px;padding:8px 18px;max-height:520px;overflow:auto}}
 .doc h2{{font-size:16px}} .doc h3{{font-size:14px;color:#1d4ed8}} .doc p{{margin:6px 0}} .cap{{color:#0d9488}}
 pre{{white-space:pre-wrap;margin:8px 10px;font-size:12px}} .edge{{font-size:12px;margin:2px 0}} code{{background:#f3f4f6;padding:0 4px;border-radius:3px}}
 .muted{{color:#9ca3af}}
</style></head><body>
<header><h1>Quarry store — {store}</h1><div>{summary}</div>
 <nav><a href=#pages>Layout</a><a href=#tables>Tables</a><a href=#text>Text</a><a href=#enrich>Enrichments</a><a href=#dag>Lineage</a></nav>
</header>
<section id=pages><h2 class=s>Page layout (docling regions, bboxes to scale)</h2>
 <div class=legend>
  <span><i style='background:#2563eb'></i>heading</span><span><i style='background:#93c5fd'></i>paragraph</span>
  <span><i style='background:#16a34a'></i>list</span><span><i style='background:#0d9488'></i>caption</span>
  <span><i style='background:#f59e0b'></i>table</span><span><i style='background:#ef4444'></i>image / OCR-deferred</span></div>
 <div class=pages>{pages_html}</div></section>
<section id=tables><h2 class=s>Tables → DbTable</h2>{tables_html}</section>
<section id=text><h2 class=s>Structured text</h2><div class=doc>{text_html}</div></section>
<section id=enrich><h2 class=s>Enrichments</h2>{enrich_html}</section>
<section id=dag><h2 class=s>Artifact lineage (DAG)</h2>{edges}</section>
</body></html>"
    )
}
