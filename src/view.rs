//! Render a store's artifacts to a single self-contained HTML view — "see the
//! details of the new setup": per-page layout schematic (colored bboxes), the
//! extracted tables, the structured text, OCR markers, and the artifact DAG.
//!
//! No server, no JS framework — `quarry view <store>` writes one HTML file you open
//! in a browser. Everything is read from the typed artifacts (bboxes, cells,
//! elements), so the view is exactly what's in the store.

use crate::artifact::*;
use crate::core::{BBox, Provenance, SourceAnchor};
use std::collections::{BTreeMap, HashMap};

/// A rasterized source page: size in POINTS (to scale the overlay to the same space)
/// plus a base64 PNG. Built by `scripts/render_pages.py`.
pub struct PageImage {
    pub w: f32,
    pub h: f32,
    pub png_b64: String,
}

/// Pages that carry positioned content (so the caller knows which to rasterize).
pub fn content_pages(artifacts: &[Box<dyn Artifact>]) -> Vec<u32> {
    let mut pages: Vec<u32> = Vec::new();
    for a in artifacts {
        let any = a.as_any();
        if let Some(sd) = any.downcast_ref::<StructuredDoc>() {
            for el in &sd.elements {
                if let Some((p, _)) = loc(&el.anchor) {
                    pages.push(p);
                }
            }
        } else if let Some((p, _)) = loc(a.provenance().anchor())
            .filter(|_| matches!(a.kind(), ArtifactKind::HtmlTable | ArtifactKind::Image | ArtifactKind::Region))
        {
            pages.push(p);
        }
    }
    pages.sort_unstable();
    pages.dedup();
    pages
}

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

/// One positioned, content-bearing element in the page reconstruction.
struct Item {
    page: u32,
    b: BBox,
    color: &'static str,
    html: String,
    tip: String,
    big: bool,
}

fn short(id: &str) -> &str {
    id.get(..20).unwrap_or(id)
}

/// A compact rendering of a table for the in-page reconstruction.
fn mini_table(cols: &[String], rows: &[Vec<String>]) -> String {
    let head = cols.iter().map(|c| format!("<th>{}</th>", esc(c))).collect::<String>();
    let body = rows
        .iter()
        .take(12)
        .map(|r| format!("<tr>{}</tr>", r.iter().map(|c| format!("<td>{}</td>", esc(c))).collect::<String>()))
        .collect::<String>();
    format!("<table class=mini><tr>{head}</tr>{body}</table>")
}

/// Render the whole store to one HTML document. When `page_images` has a page, the
/// rasterized source page is shown side-by-side with the extraction (aligned via the
/// real page dimensions); otherwise just the extraction reconstruction is shown.
pub fn render_store(
    artifacts: &[Box<dyn Artifact>],
    store: &str,
    page_images: &HashMap<u32, PageImage>,
) -> String {
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

    // ---- page reconstruction: the actual extracted content, positioned at its bbox ----
    let mut items: Vec<Item> = Vec::new();
    for sd in &docs {
        for el in &sd.elements {
            if let Some((page, b)) = loc(&el.anchor) {
                items.push(Item {
                    page,
                    b,
                    color: role_color(el.role),
                    html: esc(&el.text),
                    tip: format!("{:?}: {}", el.role, el.text),
                    big: matches!(el.role, DocRole::Title | DocRole::Heading),
                });
            }
        }
    }
    // tables: prefer the cleaned DbTable (anchored, via its parent, at the table bbox)
    if !dbs.is_empty() {
        for db in &dbs {
            if let Some((page, b)) = loc(db.meta.provenance.anchor()) {
                items.push(Item {
                    page,
                    b,
                    color: C_TABLE,
                    html: mini_table(&db.columns, &db.rows),
                    tip: format!("DbTable {}×{}", db.n_cols(), db.n_rows()),
                    big: false,
                });
            }
        }
    } else {
        for t in &htmls {
            if let Some((page, b)) = loc(t.meta.provenance.anchor()) {
                let cols: Vec<String> = (0..t.n_cols).map(|c| format!("c{c}")).collect();
                let grid = t.grid();
                items.push(Item {
                    page,
                    b,
                    color: C_TABLE,
                    html: mini_table(&cols, &grid),
                    tip: format!("HtmlTable {}×{}", t.n_rows, t.n_cols),
                    big: false,
                });
            }
        }
    }
    for im in &imgs {
        if let Some((page, b)) = loc(im.meta.provenance.anchor()) {
            items.push(Item {
                page,
                b,
                color: C_IMAGE,
                html: format!("🖼 {:?}", im.status),
                tip: format!("image — {:?}", im.status),
                big: false,
            });
        }
    }
    for r in &regs {
        if matches!(r.role(), RegionRole::Figure) {
            items.push(Item {
                page: r.page(),
                b: r.bbox(),
                color: C_IMAGE,
                html: "🖼 figure".into(),
                tip: "figure region".into(),
                big: false,
            });
        }
    }

    let mut pages: Vec<u32> = items.iter().map(|x| x.page).collect();
    pages.sort_unstable();
    pages.dedup();

    const DISP_W: f32 = 470.0;
    let mut pages_html = String::new();
    for &pg in &pages {
        let on: Vec<&Item> = items.iter().filter(|x| x.page == pg).collect();
        let img = page_images.get(&pg);
        // real page dims when we have the rendered image (so the overlay aligns to
        // the same coordinate space); otherwise fall back to the content extent.
        let (w, h) = match img {
            Some(pi) => (pi.w.max(1.0), pi.h.max(1.0)),
            None => {
                let (mut w, mut h) = (1.0f32, 1.0f32);
                for x in &on {
                    w = w.max(x.b.x1);
                    h = h.max(x.b.y1);
                }
                (w, h)
            }
        };
        let scale = DISP_W / w;
        let (cw, ch) = (w * scale, h * scale);
        let mut inner = String::new();
        for x in &on {
            let (lx, ty) = (x.b.x0.min(x.b.x1) * scale, x.b.y0.min(x.b.y1) * scale);
            let (bw, bh) = ((x.b.x1 - x.b.x0).abs() * scale, (x.b.y1 - x.b.y0).abs() * scale);
            let fs = if x.big { (bh * 0.62).clamp(8.0, 15.0) } else { (bh * 0.55).clamp(4.5, 10.0) };
            inner.push_str(&format!(
                "<div class=el style='left:{lx:.1}px;top:{ty:.1}px;width:{bw:.1}px;height:{bh:.1}px;\
                 border-left:2px solid {c};font-size:{fs:.1}px' title='{tip}'>{html}</div>",
                c = x.color,
                tip = esc(&x.tip.chars().take(300).collect::<String>()),
                html = x.html
            ));
        }
        let recon = format!("<div class=recon style='width:{cw:.0}px;height:{ch:.0}px'>{inner}</div>");
        let body = match img {
            Some(pi) => format!(
                "<div class=sxs>\
                 <figure><figcaption>document</figcaption>\
                 <img class=orig style='width:{cw:.0}px' src='data:image/png;base64,{b}'></figure>\
                 <figure><figcaption>extracted</figcaption>{recon}</figure></div>",
                b = pi.png_b64
            ),
            None => recon,
        };
        pages_html.push_str(&format!(
            "<div class=page><div class=pglabel>page {pg} — {n} items</div>{body}</div>",
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
 .pages{{display:flex;flex-wrap:wrap;gap:18px}} .pglabel{{font-size:12px;color:#6b7280;margin-bottom:4px}}
 .sxs{{display:flex;gap:14px;align-items:flex-start}} figure{{margin:0}} figcaption{{font-size:11px;color:#6b7280;margin-bottom:3px;font-weight:600}}
 .orig{{border:1px solid #d1d5db;display:block}}
 .recon{{position:relative;background:#fff;border:1px solid #d1d5db;overflow:hidden}}
 .el{{position:absolute;overflow:hidden;line-height:1.04;padding-left:2px;color:#111;box-sizing:border-box}}
 .el:hover{{overflow:visible;background:#fffbe6;z-index:5;box-shadow:0 0 0 1px #999}}
 table.mini{{border-collapse:collapse;font-size:4.5px;width:100%}} .mini td,.mini th{{border:.5px solid #d1b07a;padding:0 1px;white-space:nowrap;overflow:hidden}}
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
<section id=pages><h2 class=s>Document vs extraction (source page beside the reconstructed content)</h2>
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
