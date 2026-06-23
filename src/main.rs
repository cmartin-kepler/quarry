//! Quarry CLI (brief §5). `eval` is the point of the build; the rest support it.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use quarry::adjudicate::{Adjudicator, DefaultAdjudicator};
use quarry::artifact::{Artifact, ArtifactKind, HtmlTable};
use quarry::check::{
    cross_tier_agreement, CheckCtx, CheckOutcome, IntrinsicArithmetic, QualityCheck,
    ReconstructionError, StructuralValidity,
};
use quarry::core::{Origin, SourceAnchor, Severity};
use quarry::doc::QDoc;
use quarry::eval::{CatchReport, GroundTruth, run_eval};
use quarry::pipeline;
use quarry::store::FlatStore;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "quarry",
    about = "Lazy, iterative document parsing for LLM agents (Phase-0 example)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run an extractor and emit artifacts (HTML + manifest) to <out>. Choose a
    /// named op with `--op` (e.g. pdf-text, docling, reducto, yolo26n) or a tier;
    /// sidecars run on `--source` (the original file) if it isn't `file` itself.
    Parse {
        file: PathBuf,
        #[arg(long)]
        op: Option<String>,
        #[arg(long, default_value_t = 0)]
        tier: u8,
        #[arg(long)]
        source: Option<PathBuf>,
        /// Demand-driven: re-parse/repair tables that fail their checks.
        #[arg(long)]
        escalate: bool,
        #[arg(long)]
        out: PathBuf,
    },
    /// Run a CHAIN of ops (comma-separated), feeding each the previous op's
    /// outputs — e.g. --ops regions,text-grid,structure,sign-fix. Sidecars run on
    /// `--source` (the original file).
    Chain {
        file: PathBuf,
        #[arg(long)]
        ops: String,
        #[arg(long)]
        source: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Judge raw grids: read JSON `[{id, grid, header_rows}]`, run the detectors
    /// on each, and emit `[{id, html, flagged, signals}]` to stdout. The eval-
    /// harness entry point — same detectors that run in the pipeline, on any table.
    Judge { input: PathBuf },
    /// Run all applicable quality checks over a parsed artifact directory.
    Check { artifact_dir: PathBuf },
    /// THE first deliverable: measure the silent-failure catch rate vs truth.
    Eval {
        file: PathBuf,
        #[arg(long)]
        truth: PathBuf,
        #[arg(long, default_value_t = 0)]
        tier: u8,
        /// Show per-table detail: reconstructed grid, cell diffs, detector evidence.
        #[arg(long)]
        detail: bool,
    },
    /// Dump document structure: pages, elements, anchors, reading order.
    Inspect { file: PathBuf },
    /// Explain, per table, the evidence that it was parsed correctly (or not):
    /// reconciliation, column typing, row classification, and every signal.
    Explain {
        artifact_dir: PathBuf,
        /// Only show tables whose impression is Suspect.
        #[arg(long)]
        suspect_only: bool,
        /// Emit per-table evidence as a JSON array (for tooling).
        #[arg(long)]
        json: bool,
    },
    /// Import a real parser's output (Docling JSON) → artifacts, bypassing .qdoc.
    /// Proves the detector core runs on any parser's tables, not just the cheap one.
    ImportDocling {
        json: PathBuf,
        /// Original PDF, hashed for document identity (doc_hash). Recommended.
        #[arg(long)]
        pdf: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
    /// Segment a page from its word boxes: read JSON `[{text,x0,y0,x1,y1}]`, run the
    /// model-free XY-cut segmenter (build-plan B′'s independent region source) plus
    /// column-alignment, and print the discovered blocks. Exercises region quality
    /// on real pdfplumber word boxes — no YOLO required.
    Regions { words: PathBuf },
    /// Full Step B′ region-quality check on a layout model's regions vs a page's
    /// words: typed roles, coverage diagnostic, table-overlap gate, and agreement
    /// with the model-free XY-cut source (the decorrelated cross-check), plus figure
    /// markers. `--regions` = `{regions:[{label,confidence,bbox}]}` (e.g. from
    /// `layout_detect.py`); `--words` = `[{text,x0,y0,x1,y1}]`.
    RegionCheck {
        #[arg(long)]
        regions: PathBuf,
        #[arg(long)]
        words: PathBuf,
    },
    /// Stage-0 page triage: classify every page (text / image_content / blank) via
    /// scripts/triage.py and report. The cheap gate that keeps image/blank pages
    /// out of docling (where the table-structure model wastes ~950ms on an image).
    Triage { pdf: PathBuf },
    /// Run the pipeline (Stage 0–1): triage → docling-whole on TEXT pages only
    /// (tables + sections via litparse-free docling) → append-only store. Image
    /// pages become OCR-deferred markers; blanks are skipped.
    Pipeline {
        pdf: PathBuf,
        #[arg(long)]
        out: PathBuf,
        /// Also emit a Region artifact per docling layout box (text/table/picture)
        /// — the layout model's full detection list, queryable + crop-ready.
        #[arg(long)]
        regions: bool,
    },
    /// Materialize the HtmlTables in a store into queryable DbTables (Stage 3 MVP:
    /// cell cleanup + header/dtype inference) and preview them.
    Materialize { store: PathBuf },
    /// Print the extracted structured text (sections/paragraphs/captions) from a
    /// store's StructuredDoc as markdown.
    Text {
        store: PathBuf,
        /// Show a role + counts summary instead of the full text.
        #[arg(long)]
        summary: bool,
    },
    /// Compute (or return cached) an enrichment of the store's StructuredDoc — e.g.
    /// `--kind summary`. Lazy: a derived, content-addressed artifact computed once.
    Enrich {
        store: PathBuf,
        #[arg(long, default_value = "summary")]
        kind: String,
    },
    /// List the artifacts currently in a store — kind, id, generation, lineage, and
    /// a one-line preview. Shows the append-only artifact graph.
    Ls { store: PathBuf },
    /// Start a local web server: browse the PDFs under <dir>, click one to run the
    /// full pipeline (triage → docling → materialize, + OCR) and view the result.
    Serve {
        #[arg(default_value = "input")]
        dir: PathBuf,
        #[arg(long, default_value_t = 8765)]
        port: u16,
    },
    /// Actually OCR the store's OcrDeferred image regions (whole image pages +
    /// text-less embedded figures) and append the recovered text as derived
    /// artifacts. Needs the original --pdf.
    Ocr {
        store: PathBuf,
        #[arg(long)]
        pdf: PathBuf,
    },
    /// Render a store to a single self-contained HTML view (page layout, tables,
    /// structured text, OCR markers, lineage). With --pdf, shows each source page
    /// side-by-side with its extraction. Open it in a browser.
    View {
        store: PathBuf,
        /// Output file (default: <store>/view.html).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Original PDF — renders source pages beside the extraction.
        #[arg(long)]
        pdf: Option<PathBuf>,
    },
}

fn short_id(id: &quarry::core::ArtifactId) -> String {
    id.0.chars().take(20).collect()
}

fn artifact_preview(a: &dyn Artifact) -> String {
    use quarry::artifact::*;
    let any = a.as_any();
    if let Some(t) = any.downcast_ref::<HtmlTable>() {
        format!("{}×{} table", t.n_rows, t.n_cols)
    } else if let Some(d) = any.downcast_ref::<StructuredDoc>() {
        format!("{} text elements, {} sections", d.elements.len(), d.sections().len())
    } else if let Some(db) = any.downcast_ref::<DbTable>() {
        format!("[{}]", db.columns.join(", "))
    } else if let Some(e) = any.downcast_ref::<Enrichment>() {
        format!("{}: {}…", e.kind, e.text.chars().take(48).collect::<String>())
    } else if let Some(r) = any.downcast_ref::<Region>() {
        let b = r.bbox();
        let page = match a.anchor() {
            quarry::core::SourceAnchor::Pdf { page, .. } => *page,
            _ => 0,
        };
        format!("{:?} '{}' p{page} [{:.0},{:.0},{:.0},{:.0}]", r.role(), r.label, b.x0, b.y0, b.x1, b.y1)
    } else if let Some(i) = any.downcast_ref::<ImageRef>() {
        format!("{:?}", i.status)
    } else if let Some(g) = any.downcast_ref::<TextGrid>() {
        format!("{} words", g.words.len())
    } else {
        String::new()
    }
}

fn cmd_ls(store_dir: &Path) -> Result<()> {
    use quarry::core::Provenance;
    use quarry::store::FlatStore;

    let arts = FlatStore::open(store_dir).current_artifacts()?;
    println!("{} artifacts in {}", arts.len(), store_dir.display());
    for a in &arts {
        let lineage = match a.provenance() {
            Provenance::Source(_) => "source".to_string(),
            Provenance::Derived { parents, .. } => {
                format!("← {}", parents.iter().map(short_id).collect::<Vec<_>>().join(","))
            }
        };
        println!(
            "  {:10} {:22} gen{} {:14} | {}",
            format!("{:?}", a.kind()),
            short_id(&a.id()),
            a.generation().0,
            lineage,
            artifact_preview(a.as_ref())
        );
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Command::Parse { file, op, tier, source, escalate, out } => {
            cmd_parse(&file, op.as_deref(), tier, source.as_deref(), escalate, &out)
        }
        Command::Chain { file, ops, source, out } => {
            cmd_chain(&file, &ops, source.as_deref(), &out)
        }
        Command::Judge { input } => cmd_judge(&input),
        Command::Check { artifact_dir } => cmd_check(&artifact_dir),
        Command::Eval {
            file,
            truth,
            tier,
            detail,
        } => cmd_eval(&file, &truth, tier, detail),
        Command::Inspect { file } => cmd_inspect(&file),
        Command::ImportDocling { json, pdf, out } => {
            cmd_import_docling(&json, pdf.as_deref(), &out)
        }
        Command::Explain { artifact_dir, suspect_only, json } => {
            cmd_explain(&artifact_dir, suspect_only, json)
        }
        Command::Regions { words } => cmd_regions(&words),
        Command::RegionCheck { regions, words } => cmd_region_check(&regions, &words),
        Command::Triage { pdf } => cmd_triage(&pdf),
        Command::Pipeline { pdf, out, regions } => cmd_pipeline(&pdf, &out, regions),
        Command::Materialize { store } => cmd_materialize(&store),
        Command::Text { store, summary } => cmd_text(&store, summary),
        Command::Enrich { store, kind } => cmd_enrich(&store, &kind),
        Command::Ls { store } => cmd_ls(&store),
        Command::View { store, out, pdf } => cmd_view(&store, out.as_deref(), pdf.as_deref()),
        Command::Ocr { store, pdf } => cmd_ocr(&store, &pdf),
        Command::Serve { dir, port } => cmd_serve(&dir, port),
    }
}

/// Collect *.pdf paths under `dir` (recursive, depth-bounded).
fn collect_pdfs(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 4 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_pdfs(&p, depth + 1, out);
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("pdf")) {
            out.push(p);
        }
    }
}

fn serve_index(dir: &Path) -> String {
    use quarry::serve::url_encode;
    let mut pdfs = Vec::new();
    collect_pdfs(dir, 0, &mut pdfs);
    pdfs.sort();
    let rows = pdfs
        .iter()
        .map(|p| {
            let enc = url_encode(&p.display().to_string());
            let name = p.strip_prefix(dir).unwrap_or(p).display();
            format!(
                "<li><span>{name}</span> <a href='/run?pdf={enc}'>parse</a> \
                 <a class=ocr href='/run?pdf={enc}&ocr=1'>parse + OCR</a></li>"
            )
        })
        .collect::<String>();
    format!(
        "<!doctype html><meta charset=utf-8><title>quarry</title>\
         <style>body{{font:15px system-ui,sans-serif;max-width:760px;margin:40px auto;color:#1f2937}}\
         h1{{font-size:18px}} li{{margin:6px 0;display:flex;gap:12px;align-items:center}}\
         li span{{flex:1;color:#374151}} a{{color:#2563eb;text-decoration:none;font-size:13px}} a.ocr{{color:#7c3aed}}\
         .muted{{color:#9ca3af}}</style>\
         <h1>quarry — pick a document to parse</h1>\
         <p class=muted>{} PDFs under {}. Parsing runs docling (first run loads models — be patient).</p>\
         <ul>{rows}</ul>",
        pdfs.len(),
        dir.display()
    )
}

/// Run pipeline (+materialize, +OCR) on a chosen PDF and return the rendered view.
fn serve_run(pdf: &Path, ocr: bool) -> Result<String> {
    let stem = pdf.file_stem().and_then(|s| s.to_str()).unwrap_or("doc");
    let store = std::env::temp_dir().join("quarry_serve").join(stem);
    let _ = std::fs::remove_dir_all(&store);
    cmd_pipeline(pdf, &store, true)?;
    cmd_materialize(&store)?;
    if ocr {
        cmd_ocr(&store, pdf)?;
    }
    render_view(&store, Some(pdf))
}

fn cmd_serve(dir: &Path, port: u16) -> Result<()> {
    use quarry::serve::{read_request, write_html};
    use std::net::TcpListener;

    let listener = TcpListener::bind(("127.0.0.1", port))
        .with_context(|| format!("binding 127.0.0.1:{port}"))?;
    println!("quarry serve → http://127.0.0.1:{port}   (PDF root: {})", dir.display());
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let Ok(req) = read_request(&stream) else { continue };
        let result = match req.path.as_str() {
            "/" => Ok(serve_index(dir)),
            "/run" => match req.query.get("pdf") {
                Some(pdf) => {
                    let ocr = req.query.get("ocr").map(|v| v == "1").unwrap_or(false);
                    println!("→ parse {pdf}{}", if ocr { " + OCR" } else { "" });
                    serve_run(Path::new(pdf), ocr)
                }
                None => Ok("<p>missing ?pdf=</p>".into()),
            },
            _ => Ok("<h1>404</h1>".into()),
        };
        let (status, body) = match result {
            Ok(b) => ("200 OK", b),
            Err(e) => ("500 Internal Server Error", format!("<pre>error: {e:#}</pre>")),
        };
        let _ = write_html(&mut stream, status, &body);
    }
    Ok(())
}

/// Actually OCR the store's OcrDeferred image regions and append the recovered text
/// (an `ocr` Enrichment derived from each ImageRef — lineage preserved).
fn cmd_ocr(store_dir: &Path, pdf: &Path) -> Result<()> {
    use quarry::artifact::{Enrichment, ImageRef, ImageStatus};
    use quarry::core::{Generation, SourceAnchor};
    use quarry::store::FlatStore;

    let store = FlatStore::open(store_dir);
    let arts = store.current_artifacts()?;
    let targets: Vec<&ImageRef> = arts
        .iter()
        .filter_map(|a| a.as_any().downcast_ref::<ImageRef>())
        .filter(|i| i.status == ImageStatus::OcrDeferred)
        .collect();
    if targets.is_empty() {
        println!("no OcrDeferred images in {} — nothing to OCR", store_dir.display());
        return Ok(());
    }

    let req: Vec<serde_json::Value> = targets
        .iter()
        .map(|im| {
            let b = im.bbox();
            let page = match im.meta.provenance.anchor() {
                SourceAnchor::Pdf { page, .. } => *page,
                _ => 0,
            };
            serde_json::json!({"page": page, "bbox": [b.x0, b.y0, b.x1, b.y1]})
        })
        .collect();
    let req_s = serde_json::to_string(&req)?;
    println!("OCR-ing {} image region(s)…", targets.len());
    let texts: Vec<String> =
        serde_json::from_str(&run_uv(&["run", "scripts/ocr.py", &pdf.display().to_string(), &req_s])?)?;

    let doc = targets[0].anchor().doc();
    let mut out: Vec<Box<dyn Artifact>> = Vec::new();
    for (im, text) in targets.iter().zip(&texts) {
        if !text.trim().is_empty() {
            out.push(Box::new(Enrichment::derive(*im, "ocr", text.clone(), Generation(1))));
        }
    }
    store.write(doc, &out, &[])?;
    println!("recovered text from {}/{} regions → {} OCR artifacts", out.len(), targets.len(), out.len());
    Ok(())
}

/// Build the HTML view for a store (rasterizing source pages when `pdf` is given).
fn render_view(store_dir: &Path, pdf: Option<&Path>) -> Result<String> {
    use quarry::store::FlatStore;
    use quarry::view::{content_pages, PageImage};
    use std::collections::HashMap;

    let arts = FlatStore::open(store_dir).current_artifacts()?;
    let mut page_images: HashMap<u32, PageImage> = HashMap::new();
    if let Some(pdf) = pdf {
        let pages = content_pages(&arts);
        if !pages.is_empty() {
            let list = pages.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(",");
            let json = run_uv(&["run", "scripts/render_pages.py", &pdf.display().to_string(), "--pages", &list])?;
            let raw: HashMap<String, serde_json::Value> = serde_json::from_str(&json)?;
            for (k, v) in raw {
                if let (Ok(pg), Some(w), Some(h), Some(png)) =
                    (k.parse::<u32>(), v["w"].as_f64(), v["h"].as_f64(), v["png"].as_str())
                {
                    page_images.insert(pg, PageImage { w: w as f32, h: h as f32, png_b64: png.to_string() });
                }
            }
        }
    }
    Ok(quarry::view::render_store(&arts, &store_dir.display().to_string(), &page_images))
}

fn cmd_view(store_dir: &Path, out: Option<&Path>, pdf: Option<&Path>) -> Result<()> {
    let html = render_view(store_dir, pdf)?;
    let out_path = out.map(PathBuf::from).unwrap_or_else(|| store_dir.join("view.html"));
    std::fs::write(&out_path, &html).with_context(|| format!("writing {}", out_path.display()))?;
    println!("wrote {} — open it in a browser", out_path.display());
    Ok(())
}

/// Lazy enrichment: derive (e.g.) a summary of the store's StructuredDoc — computed
/// once, content-addressed, cached. The compute backend (here a deterministic stub)
/// is the only LLM-specific part; everything around it is the generic substrate.
fn cmd_enrich(store_dir: &Path, kind: &str) -> Result<()> {
    use quarry::artifact::{DocRole, Enrichment, StructuredDoc};
    use quarry::core::Generation;
    use quarry::store::FlatStore;

    let store = FlatStore::open(store_dir);
    let arts = store.current_artifacts()?;
    let sd = arts
        .iter()
        .find_map(|a| a.as_any().downcast_ref::<StructuredDoc>())
        .ok_or_else(|| anyhow::anyhow!("no StructuredDoc in store — run `pipeline` first"))?;
    let source_id = sd.id();

    // LAZY: content-addressed by (kind, source) → if already computed, return cached.
    if let Some(e) = arts
        .iter()
        .filter_map(|a| a.as_any().downcast_ref::<Enrichment>())
        .find(|e| e.kind == kind && e.source == source_id)
    {
        println!("(cached) {kind} of {source_id}:\n\n{}", e.text);
        return Ok(());
    }

    // COMPUTE ON DEMAND. This is where an LLM call goes — a sidecar, like docling:
    // hand the model `sd.to_markdown()`, store its response. Everything else is generic.
    let text = match kind {
        "summary" => {
            let headings: Vec<&str> = sd
                .elements
                .iter()
                .filter(|e| matches!(e.role, DocRole::Heading | DocRole::Title))
                .map(|e| e.text.as_str())
                .collect();
            let opens: String = sd
                .elements
                .iter()
                .find(|e| e.role == DocRole::Paragraph)
                .map(|e| e.text.chars().take(180).collect())
                .unwrap_or_default();
            format!(
                "[stub summary — drop in an LLM call here over sd.to_markdown()]\n\
                 {} elements, {} sections.\nSections: {}.\nOpens: {opens}",
                sd.elements.len(),
                headings.len(),
                headings.join("; ")
            )
        }
        other => anyhow::bail!("unknown enrichment kind: {other}"),
    };

    let enr = Enrichment::derive(sd, kind, text.clone(), Generation(1));
    store.write(sd.anchor().doc(), &[Box::new(enr)], &[])?;
    println!("(computed + cached) {kind} of {source_id}:\n\n{text}");
    Ok(())
}

/// Print a store's structured text (StructuredDoc) as markdown, or a role summary.
fn cmd_text(store_dir: &Path, summary: bool) -> Result<()> {
    use quarry::artifact::{DocRole, StructuredDoc};
    use quarry::store::FlatStore;
    use std::collections::BTreeMap;

    let arts = FlatStore::open(store_dir).current_artifacts()?;
    let docs: Vec<&StructuredDoc> =
        arts.iter().filter_map(|a| a.as_any().downcast_ref::<StructuredDoc>()).collect();
    if docs.is_empty() {
        println!("no StructuredDoc in {}", store_dir.display());
        return Ok(());
    }
    for d in docs {
        if summary {
            let mut roles: BTreeMap<String, usize> = BTreeMap::new();
            for el in &d.elements {
                *roles.entry(format!("{:?}", el.role)).or_default() += 1;
            }
            let chars: usize = d.elements.iter().map(|e| e.text.chars().count()).sum();
            let headings: Vec<&str> = d
                .elements
                .iter()
                .filter(|e| matches!(e.role, DocRole::Heading | DocRole::Title))
                .map(|e| e.text.as_str())
                .collect();
            println!("{} elements, {chars} chars; roles {roles:?}", d.elements.len());
            println!("headings ({}):", headings.len());
            for h in headings {
                println!("  • {h}");
            }
        } else {
            print!("{}", d.to_markdown());
        }
    }
    Ok(())
}

/// Stage 3 MVP: materialize every HtmlTable in a store into a DbTable, persist, preview.
fn cmd_materialize(store_dir: &Path) -> Result<()> {
    use quarry::artifact::HtmlTable;
    use quarry::core::Generation;
    use quarry::materialize::materialize;
    use quarry::store::FlatStore;

    let store = FlatStore::open(store_dir);
    let arts = store.current_artifacts()?;
    let tables: Vec<&HtmlTable> =
        arts.iter().filter_map(|a| a.as_any().downcast_ref::<HtmlTable>()).collect();
    if tables.is_empty() {
        println!("no HtmlTables in {}", store_dir.display());
        return Ok(());
    }

    let doc = tables[0].anchor().doc();
    let mut dbs: Vec<Box<dyn Artifact>> = Vec::new();
    for t in &tables {
        let db = materialize(t, Generation(1));
        println!(
            "\nDbTable ({} cols × {} rows) ← {}",
            db.n_cols(),
            db.n_rows(),
            db.source
        );
        println!(
            "  {}",
            db.columns
                .iter()
                .zip(&db.dtypes)
                .map(|(c, d)| format!("{c} [{d:?}]"))
                .collect::<Vec<_>>()
                .join(" | ")
        );
        for row in db.rows.iter().take(3) {
            println!("    {}", row.join(" | "));
        }
        dbs.push(Box::new(db));
    }

    store.write(doc, &dbs, &[])?;
    println!("\nmaterialized {} table(s) → {}", dbs.len(), store_dir.display());
    Ok(())
}

/// Run a `uv run <script ...>` sidecar and capture stdout.
fn run_uv(args: &[&str]) -> Result<String> {
    let out = std::process::Command::new("uv")
        .args(args)
        .output()
        .with_context(|| format!("running: uv {}", args.join(" ")))?;
    if !out.status.success() {
        anyhow::bail!("`uv {}` failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Pipeline Stage 0–1: triage → docling-whole on text pages → store.
fn cmd_pipeline(pdf: &Path, out: &Path, regions: bool) -> Result<()> {
    use quarry::artifact::{ImageRef, Region, RegionRole};
    use quarry::core::{DocHash, Generation};
    use quarry::docling::{artifacts_from_docling, regions_from_docling, structured_doc_from_docling};
    use quarry::store::FlatStore;
    use quarry::triage::{counts, ocr_markers, parse as parse_triage, PageClass};

    // a Figure region with fewer than this many words under its bbox is text-less → OCR target
    const OCR_WORD_MIN: i32 = 3;

    let bytes = std::fs::read(pdf).with_context(|| format!("reading {}", pdf.display()))?;
    let doc = DocHash::of(&bytes);
    let generation = Generation(0);
    let pdf_s = pdf.display().to_string();

    // Stage 0 — triage
    let pages = parse_triage(&run_uv(&["run", "scripts/triage.py", &pdf_s])?)?;
    let (t, i, b) = counts(&pages);
    println!("triage: {} pages — {t} text, {i} image_content, {b} blank", pages.len());

    let mut artifacts: Vec<Box<dyn Artifact>> = Vec::new();
    // image_content pages → OCR-deferred markers (invariant 11: recorded, not dropped)
    for m in ocr_markers(&pages, doc) {
        artifacts.push(Box::new(m));
    }

    // Stage 1 — docling whole-page on TEXT pages only (image/blank skipped)
    let text: Vec<String> =
        pages.iter().filter(|p| p.klass == PageClass::Text).map(|p| p.page.to_string()).collect();
    let (mut n_tables, mut n_elems, mut n_regions, mut n_embedded_ocr) = (0usize, 0usize, 0usize, 0usize);
    if !text.is_empty() {
        let dj = run_uv(&["run", "scripts/run_docling.py", &pdf_s, "--pages", &text.join(",")])?;
        let tables = artifacts_from_docling(&dj, doc, generation)?;
        n_tables = tables.len();
        artifacts.extend(tables);
        let sd = structured_doc_from_docling(&dj, doc, generation)?;
        n_elems = sd.elements.len();
        if n_elems > 0 {
            artifacts.push(Box::new(sd));
        }

        // Embedded image regions (docling Figures) whose bbox has no text layer are
        // OCR targets — invariant 11 extended from whole pages to sub-page images.
        let regs = regions_from_docling(&dj, doc, generation)?;
        let figures: Vec<&Region> =
            regs.iter().filter(|r| r.role() == RegionRole::Figure).collect();
        if !figures.is_empty() {
            let req: Vec<serde_json::Value> = figures
                .iter()
                .map(|r| {
                    let b = r.bbox();
                    serde_json::json!({"page": r.page(), "bbox": [b.x0, b.y0, b.x1, b.y1]})
                })
                .collect();
            let req_s = serde_json::to_string(&req)?;
            let word_counts: Vec<i32> = serde_json::from_str(
                &run_uv(&["run", "scripts/region_text.py", &pdf_s, &req_s])?,
            )?;
            for (fig, &words) in figures.iter().zip(&word_counts) {
                if (0..OCR_WORD_MIN).contains(&words) {
                    artifacts.push(Box::new(ImageRef::ocr_deferred_region(fig)));
                    n_embedded_ocr += 1;
                }
            }
        }

        if regions {
            n_regions = regs.len();
            for r in regs {
                artifacts.push(Box::new(r));
            }
        }
    }

    FlatStore::open(out).write(doc, &artifacts, &[])?;
    let reg_note = if regions { format!(", {n_regions} layout regions") } else { String::new() };
    let ocr_note =
        if n_embedded_ocr > 0 { format!(", {n_embedded_ocr} embedded-image OCR markers") } else { String::new() };
    println!(
        "parsed: {n_tables} tables, {n_elems} text elements, {i} OCR markers{ocr_note}{reg_note} → {}",
        out.display()
    );
    Ok(())
}

/// Stage-0 triage: run scripts/triage.py via uv, classify pages, report.
fn cmd_triage(pdf: &Path) -> Result<()> {
    use quarry::triage::{counts, parse};
    let pages = parse(&run_uv(&["run", "scripts/triage.py", &pdf.display().to_string()])?)?;
    let (t, i, b) = counts(&pages);
    println!("{} pages: {t} text, {i} image_content (OCR-deferred), {b} blank", pages.len());
    for p in &pages {
        let sd = p.stddev.map(|s| format!("{s:.1}")).unwrap_or_else(|| "-".into());
        println!("  p{:<4} {:?}  chars={} stddev={}", p.page, p.klass, p.chars, sd);
    }
    Ok(())
}

/// Run the full Step B′ region-quality check on a layout model's regions vs a
/// page's words: coverage diagnostic, table-overlap gate, and agreement with the
/// decorrelated XY-cut source. Exercises region_check/segment/figure_markers on
/// real layout output.
fn cmd_region_check(regions_path: &Path, words_path: &Path) -> Result<()> {
    use quarry::artifact::{figure_markers, Word};
    use quarry::core::{BBox, DocHash, Generation};
    use quarry::region_check::{
        boundary_agreement, disagreeing_regions, overlapping_table_pairs, passes_agreement_bar,
        passes_overlap_bar, typed_orphans, AGREEMENT_IOU,
    };
    use quarry::segment::{xy_cut, CutParams};
    use quarry::sidecar::regions_from_json;
    use std::collections::BTreeMap;

    #[derive(serde::Deserialize)]
    struct WordIn { text: String, x0: f32, y0: f32, x1: f32, y1: f32 }

    let regions = regions_from_json(
        &std::fs::read_to_string(regions_path)?,
        DocHash::of(b"region-check"),
        1,
        Generation(0),
    )?;
    let wins: Vec<WordIn> = serde_json::from_slice(&std::fs::read(words_path)?)?;
    let words: Vec<Word> = wins
        .iter()
        .map(|w| Word { text: w.text.clone(), bbox: BBox::new(w.x0, w.y0, w.x1, w.y1) })
        .collect();

    println!("{} regions, {} words", regions.len(), words.len());
    let mut roles: BTreeMap<String, usize> = BTreeMap::new();
    for r in &regions {
        *roles.entry(format!("{:?}", r.role())).or_default() += 1;
    }
    println!("  roles: {roles:?}");

    let orphans = typed_orphans(&regions, &words);
    println!(
        "  coverage (diagnostic): {} typed-orphan spans — expect page furniture; \
         body-content orphans mean a missed box",
        orphans.len()
    );

    let pairs = overlapping_table_pairs(&regions);
    println!(
        "  overlap gate: {} ({} offending table pair(s))",
        if passes_overlap_bar(&regions) { "PASS" } else { "FAIL" },
        pairs.len()
    );

    let indep = xy_cut(&words, &CutParams::default());
    let yolo: Vec<BBox> = regions.iter().map(|r| r.bbox()).collect();
    let agree = boundary_agreement(&yolo, &indep, AGREEMENT_IOU);
    println!(
        "  agreement vs XY-cut: {} blocks, {:.0}% of regions match (bar 90%) — {}",
        indep.len(),
        agree * 100.0,
        if passes_agreement_bar(&yolo, &indep) { "PASS" } else { "FAIL" }
    );
    let dis = disagreeing_regions(&yolo, &indep);
    if !dis.is_empty() {
        println!("    {} region(s) with no independent match (flag for review): {dis:?}", dis.len());
    }

    let figs = figure_markers(&regions);
    println!("  figure markers: {} ImageRef(s) recorded (extraction deferred)", figs.len());
    Ok(())
}

/// Run the model-free region machinery on a page's word boxes (build-plan B′):
/// XY-cut segmentation + per-block column-alignment. A dev/inspection tool that
/// exercises `segment` and `columns` on real spans without a layout model.
fn cmd_regions(words: &Path) -> Result<()> {
    use quarry::artifact::Word;
    use quarry::columns::{column_count, COLUMN_GUTTER};
    use quarry::core::BBox;
    use quarry::segment::{xy_cut, CutParams};

    #[derive(serde::Deserialize)]
    struct WordIn { text: String, x0: f32, y0: f32, x1: f32, y1: f32 }

    let ins: Vec<WordIn> = serde_json::from_slice(&std::fs::read(words)?)?;
    let spans: Vec<Word> = ins
        .iter()
        .map(|w| Word { text: w.text.clone(), bbox: BBox::new(w.x0, w.y0, w.x1, w.y1) })
        .collect();

    let blocks = xy_cut(&spans, &CutParams::default());
    println!("{} spans → {} blocks (XY-cut, model-free)", spans.len(), blocks.len());
    for (i, b) in blocks.iter().enumerate() {
        let inside: Vec<Word> = spans.iter().filter(|w| b.contains_center(&w.bbox)).cloned().collect();
        let cols = column_count(&inside, COLUMN_GUTTER);
        let preview: String = inside.iter().take(10).map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
        println!(
            "  block {i}: ({:.0},{:.0})-({:.0},{:.0})  words={:<4} cols={}  | {}",
            b.x0, b.y0, b.x1, b.y1, inside.len(), cols, preview
        );
    }
    Ok(())
}

fn cmd_explain(artifact_dir: &Path, suspect_only: bool, json: bool) -> Result<()> {
    use quarry::analysis::RowKind;
    use quarry::evidence::{Impression, assess};

    let store = FlatStore::open(artifact_dir);
    let artifacts = store
        .current_artifacts()
        .with_context(|| "loading artifacts (did you `parse`/`import-docling` first?)")?;

    if json {
        let mut out = Vec::new();
        for a in &artifacts {
            let Some(t) = a.as_any().downcast_ref::<HtmlTable>() else {
                continue;
            };
            let ev = assess(t);
            let impression = match ev.impression {
                Impression::Confirmed => "confirmed",
                Impression::NoIssues => "no_issues",
                Impression::Suspect => "suspect",
            };
            let signals: Vec<serde_json::Value> = ev
                .signals
                .iter()
                .map(|s| serde_json::json!({"positive": s.positive, "detail": s.detail}))
                .collect();
            out.push(serde_json::json!({
                "id": a.id().to_string(),
                "rows": ev.n_rows,
                "cols": ev.n_cols,
                "header_rows": ev.header_rows,
                "col_types": ev.col_types.iter().map(|c| col_tag(*c)).collect::<Vec<_>>(),
                "impression": impression,
                "signals": signals,
            }));
        }
        println!("{}", serde_json::to_string(&out)?);
        return Ok(());
    }

    let mut shown = 0;
    let mut counts = (0usize, 0usize, 0usize); // confirmed, no-issues, suspect
    for a in &artifacts {
        let Some(t) = a.as_any().downcast_ref::<HtmlTable>() else {
            continue;
        };
        let ev = assess(t);
        match ev.impression {
            Impression::Confirmed => counts.0 += 1,
            Impression::NoIssues => counts.1 += 1,
            Impression::Suspect => counts.2 += 1,
        }
        if suspect_only && ev.impression != Impression::Suspect {
            continue;
        }
        shown += 1;

        println!("\n{}", "═".repeat(76));
        println!("▌ {}  ({}x{}, {} header row(s))", a.id(), ev.n_rows, ev.n_cols, ev.header_rows);
        // Column typing + row classification — the model the signals reason over.
        let coltypes: Vec<String> = ev
            .col_types
            .iter()
            .enumerate()
            .map(|(i, ct)| format!("{i}:{}", col_tag(*ct)))
            .collect();
        println!("  columns: {}", coltypes.join("  "));
        let (h, s, d, t_) = (
            ev.row_kinds.iter().filter(|k| **k == RowKind::Header).count(),
            ev.row_kinds.iter().filter(|k| **k == RowKind::Section).count(),
            ev.row_kinds.iter().filter(|k| **k == RowKind::Data).count(),
            ev.row_kinds.iter().filter(|k| **k == RowKind::Total).count(),
        );
        println!("  rows: {h} header, {s} section-label, {d} data, {t_} total/subtotal");

        for sig in ev.positives() {
            println!("  ✓ {}", sig.detail);
        }
        for sig in ev.negatives() {
            println!("  ✗ {}", sig.detail);
        }
        println!("  ⇒ {}", ev.impression.label());
    }

    println!("\n{}", "─".repeat(76));
    println!(
        "{} table(s): {} likely-correct, {} no-issues, {} suspect{}",
        counts.0 + counts.1 + counts.2,
        counts.0,
        counts.1,
        counts.2,
        if suspect_only {
            format!(" (showed {shown} suspect)")
        } else {
            String::new()
        }
    );
    Ok(())
}

fn col_tag(c: quarry::analysis::ColType) -> &'static str {
    use quarry::analysis::ColType::*;
    match c {
        Label => "label",
        Numeric => "num",
        Ratio => "ratio",
        Empty => "empty",
    }
}

/// Run the parse-time detectors (arithmetic + structural) over table artifacts,
/// record append-only adjudication verdicts, and write the store. Shared by the
/// cheap-parse path and the Docling import path. These checks read only the
/// artifact, so no source text layer is needed here.
fn adjudicate_and_store(
    artifacts: &[Box<dyn Artifact>],
    doc_hash: quarry::core::DocHash,
    out: &Path,
) -> Result<usize> {
    let arithmetic = IntrinsicArithmetic::default();
    let structural = StructuralValidity;
    let dummy = QDoc { format: quarry::doc::DocFormat::Pdf, pages: vec![] };
    let ctx = CheckCtx { source: &dummy };
    let adj = DefaultAdjudicator;

    let mut verdicts = Vec::new();
    for a in artifacts {
        if a.kind() != ArtifactKind::HtmlTable {
            continue;
        }
        let outcomes: Vec<CheckOutcome> = vec![
            arithmetic.check(a.as_ref(), &ctx),
            structural.check(a.as_ref(), &ctx),
        ];
        let candidate: [&dyn Artifact; 1] = [a.as_ref()];
        verdicts.push(adj.adjudicate(&candidate, &outcomes));
    }

    FlatStore::open(out).write(doc_hash, artifacts, &verdicts)?;
    Ok(artifacts
        .iter()
        .filter(|a| a.kind() == ArtifactKind::HtmlTable)
        .count())
}

/// A `.qdoc` loads as the native text-layer context; any other file (a real PDF
/// for a sidecar/chain) gets an empty context + a doc_hash of its bytes.
fn load_doc(file: &Path) -> Result<(QDoc, quarry::core::DocHash)> {
    match QDoc::load(file) {
        Ok(x) => Ok(x),
        Err(_) => {
            let bytes = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
            Ok((QDoc { format: quarry::doc::DocFormat::Pdf, pages: vec![] }, quarry::core::DocHash::of(&bytes)))
        }
    }
}

fn cmd_judge(input: &Path) -> Result<()> {
    use quarry::artifact::{Cell, HtmlTable, Meta};
    use quarry::core::{ArtifactId, BBox, DocHash, Generation, Provenance};
    use quarry::doc::{DocFormat, Page, Span};
    use quarry::grid::to_html;

    #[derive(serde::Deserialize)]
    struct Word {
        text: String,
        bbox: [f32; 4],
    }
    #[derive(serde::Deserialize)]
    struct In {
        id: String,
        grid: Vec<Vec<String>>,
        #[serde(default = "one")]
        header_rows: usize,
        #[serde(default)]
        source: String,
        // Optional source geometry; when all three are present the
        // reconstruction-error detector runs (else it no-ops, as for synthetic).
        #[serde(default)]
        page: u32,
        #[serde(default)]
        cell_boxes: Vec<Vec<[f32; 4]>>, // parallel to `grid`: bbox per cell
        #[serde(default)]
        source_words: Vec<Word>, // words in the table's region
        #[serde(default)]
        region: Option<[f32; 4]>, // detected table region (provenance bbox)
        // An independent second-tier parse of the same region; when present, the
        // cross-tier-agreement detector compares the two.
        #[serde(default)]
        alt_grid: Vec<Vec<String>>,
        #[serde(default = "one")]
        alt_header_rows: usize,
        #[serde(default)]
        alt_cell_boxes: Vec<Vec<[f32; 4]>>, // parallel to `alt_grid`: bbox per cell
        #[serde(default)]
        alt_page: u32,
    }
    fn one() -> usize { 1 }
    #[derive(serde::Serialize)]
    struct Sig { check: String, severity: String, reason: String }
    #[derive(serde::Serialize)]
    struct Out { id: String, source: String, html: String, flagged: bool, signals: Vec<Sig> }

    let ins: Vec<In> = serde_json::from_slice(&std::fs::read(input).with_context(|| format!("reading {}", input.display()))?)?;
    let arith = IntrinsicArithmetic::default();
    let structural = StructuralValidity;
    let recon = ReconstructionError::default();
    let checks: [(&str, &dyn QualityCheck); 3] = [
        ("intrinsic_arithmetic", &arith),
        ("structural_validity", &structural),
        ("reconstruction_error", &recon),
    ];

    let mut outs = Vec::new();
    for it in ins {
        let dh = DocHash::of(it.id.as_bytes());
        let dummy = SourceAnchor::Pdf { doc: dh, page: 1, bbox: BBox::new(0.0, 0.0, 1.0, 1.0) };
        // Cell geometry (for reconstruction + cross-tier) is independent of source
        // words (for reconstruction's region scan): cross-tier needs boxes but no
        // source words; reconstruction needs both.
        let ext_geo = it.page > 0 && !it.cell_boxes.is_empty();
        let prov = match (ext_geo, it.region) {
            (true, Some(r)) => SourceAnchor::Pdf { doc: dh, page: it.page, bbox: BBox::new(r[0], r[1], r[2], r[3]) },
            _ => dummy.clone(),
        };
        let n_rows = it.grid.len() as u32;
        let n_cols = it.grid.iter().map(|r| r.len()).max().unwrap_or(0) as u32;
        let mk_cells = |grid: &[Vec<String>], boxes: &[Vec<[f32; 4]>], page: u32, hdr: usize| {
            let mut cells = Vec::new();
            for (r, row) in grid.iter().enumerate() {
                for (c, t) in row.iter().enumerate() {
                    let anchor = match boxes.get(r).and_then(|rr| rr.get(c)) {
                        Some(b) if page > 0 => SourceAnchor::Pdf { doc: dh, page, bbox: BBox::new(b[0], b[1], b[2], b[3]) },
                        _ => dummy.clone(),
                    };
                    cells.push(Cell { row: r as u32, col: c as u32, text: t.clone(), anchor, is_header: r < hdr });
                }
            }
            cells
        };
        let cells = mk_cells(&it.grid, &it.cell_boxes, it.page, it.header_rows);
        let html = to_html(&it.grid, it.header_rows);
        let table = HtmlTable {
            meta: Meta {
                id: ArtifactId::mint(&dh, Generation(0)),
                content_hash: dh,
                provenance: Provenance::Source(prov.clone()),
                generation: Generation(0),
                risk: Default::default(),
                origin: Origin::default(),
            },
            n_rows,
            n_cols,
            cells,
            html: html.clone(),
        };
        // Real region words when supplied, else an empty doc so the reconstruction
        // detector no-ops on grid-only input.
        let source = if !it.source_words.is_empty() {
            QDoc {
                format: DocFormat::Pdf,
                pages: vec![Page {
                    page: it.page,
                    width: 10_000.0,
                    height: 10_000.0,
                    spans: it.source_words.iter().map(|w| Span {
                        text: w.text.clone(), bbox: w.bbox, confidence: 1.0, rotated: false,
                    }).collect(),
                    table_regions: vec![],
                }],
            }
        } else {
            QDoc { format: DocFormat::Pdf, pages: vec![] }
        };
        let cctx = CheckCtx { source: &source };
        let mut signals = Vec::new();
        for (cid, chk) in &checks {
            if let CheckOutcome::Flag { reason, severity } = chk.check(&table, &cctx) {
                signals.push(Sig { check: cid.to_string(), severity: format!("{severity:?}"), reason });
            }
        }
        // Cross-tier agreement (compares two parses, so it isn't a QualityCheck).
        // Geometry-keyed, so the alt parse needs its own cell boxes + page.
        if !it.alt_grid.is_empty() {
            let alt = HtmlTable {
                meta: Meta {
                    id: ArtifactId::mint(&dh, Generation(0)),
                    content_hash: dh,
                    provenance: Provenance::Source(dummy.clone()),
                    generation: Generation(0),
                    risk: Default::default(),
                    origin: Origin::default(),
                },
                n_rows: it.alt_grid.len() as u32,
                n_cols: it.alt_grid.iter().map(|r| r.len()).max().unwrap_or(0) as u32,
                cells: mk_cells(&it.alt_grid, &it.alt_cell_boxes, it.alt_page, it.alt_header_rows),
                html: String::new(),
            };
            if let CheckOutcome::Flag { reason, severity } = cross_tier_agreement(&table, &alt) {
                signals.push(Sig { check: "cross_tier_agreement".into(), severity: format!("{severity:?}"), reason });
            }
        }
        outs.push(Out { id: it.id, source: it.source, html, flagged: !signals.is_empty(), signals });
    }
    println!("{}", serde_json::to_string(&outs)?);
    Ok(())
}

fn cmd_chain(file: &Path, ops: &str, source: Option<&Path>, out: &Path) -> Result<()> {
    let (doc, doc_hash) = load_doc(file)?;
    let source_path = Some(source.unwrap_or(file).to_path_buf());
    let op_ids: Vec<String> = ops.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    let artifacts = pipeline::run_chain(&doc, doc_hash, source_path, &op_ids)?;
    let n_tables = adjudicate_and_store(&artifacts, doc_hash, out)?;
    println!(
        "ran chain [{}] on {} (doc {}) → {} artifact(s), {} table(s) → {}",
        op_ids.join(" → "),
        file.display(),
        doc_hash.short(),
        artifacts.len(),
        n_tables,
        out.display()
    );
    Ok(())
}

fn cmd_parse(
    file: &Path,
    op: Option<&str>,
    tier: u8,
    source: Option<&Path>,
    escalate: bool,
    out: &Path,
) -> Result<()> {
    let (doc, doc_hash) = load_doc(file)?;
    // sidecars run the tool on `--source`, defaulting to `file` itself.
    let source_path = Some(source.unwrap_or(file).to_path_buf());

    let extractor = match op {
        Some(id) => pipeline::extractor_by_id(id)
            .ok_or_else(|| anyhow::anyhow!("unknown op `{id}`"))?,
        None => pipeline::extractor_for(doc.format, tier)?,
    };
    let mut artifacts = pipeline::run_document_extractor(
        &doc,
        doc_hash,
        source_path.clone(),
        quarry::core::Generation(0),
        extractor.as_ref(),
    )?;

    let mut escalated = 0usize;
    if escalate {
        let arith = IntrinsicArithmetic::default();
        let structural = StructuralValidity;
        let checks: Vec<&dyn QualityCheck> = vec![&arith, &structural];
        let extra = quarry::route::escalate(
            &doc,
            doc_hash,
            source_path,
            &artifacts,
            &checks,
            3,
            &|id| pipeline::extractor_by_id(id),
        )?;
        escalated = extra.len();
        artifacts.extend(extra);
    }

    let n_tables = adjudicate_and_store(&artifacts, doc_hash, out)?;
    println!(
        "parsed {} with `{}` (doc {}) → {} artifact(s){}, {} table(s) → {}",
        file.display(),
        op.unwrap_or("pdf-text"),
        doc_hash.short(),
        artifacts.len(),
        if escalate { format!(" (+{escalated} escalated)") } else { String::new() },
        n_tables,
        out.display()
    );
    Ok(())
}

fn cmd_import_docling(json: &Path, pdf: Option<&Path>, out: &Path) -> Result<()> {
    let json_text = std::fs::read_to_string(json)
        .with_context(|| format!("reading {}", json.display()))?;
    // doc_hash IS document identity (brief §3): hash the original PDF bytes when
    // available, else fall back to hashing the Docling JSON.
    let doc_hash = match pdf {
        Some(p) => quarry::core::DocHash::of(
            &std::fs::read(p).with_context(|| format!("reading {}", p.display()))?,
        ),
        None => quarry::core::DocHash::of(json_text.as_bytes()),
    };
    let artifacts =
        quarry::docling::artifacts_from_docling(&json_text, doc_hash, quarry::core::Generation(0))?;
    let n_tables = adjudicate_and_store(&artifacts, doc_hash, out)?;
    println!(
        "imported {} (doc {}) via Docling → {} table(s) → {}\nrun `quarry check {}` to see detector flags.",
        json.display(),
        doc_hash.short(),
        n_tables,
        out.display(),
        out.display()
    );
    Ok(())
}

fn cmd_check(artifact_dir: &Path) -> Result<()> {
    let store = FlatStore::open(artifact_dir);
    let artifacts = store
        .current_artifacts()
        .with_context(|| "loading artifacts (did you `parse` first?)")?;

    let arithmetic = IntrinsicArithmetic::default();
    let structural = StructuralValidity;
    // No source doc handy from a flat dir; checks here use only artifact data.
    let dummy = QDoc {
        format: quarry::doc::DocFormat::Pdf,
        pages: vec![],
    };
    let ctx = CheckCtx { source: &dummy };
    let checks: Vec<&dyn QualityCheck> = vec![&arithmetic, &structural];

    println!(
        "{:<28} {:<22} {:<8} reason",
        "element", "check", "severity"
    );
    println!("{}", "-".repeat(78));
    let mut flags = 0;
    for a in &artifacts {
        for c in &checks {
            if !c.applies_to(a.kind()) {
                continue;
            }
            if let CheckOutcome::Flag { reason, severity } = c.check(a.as_ref(), &ctx) {
                flags += 1;
                println!(
                    "{:<28} {:<22} {:<8} {}",
                    a.id().to_string(),
                    c.id().0,
                    sev(severity),
                    reason
                );
            }
        }
    }
    if flags == 0 {
        println!("(no flags)");
    }
    Ok(())
}

fn cmd_eval(file: &Path, truth: &Path, tier: u8, detail: bool) -> Result<()> {
    let (doc, doc_hash) = QDoc::load(file)?;
    let truth = GroundTruth::load(truth)?;
    let report = run_eval(&doc, doc_hash, &truth, tier)?;
    print_report(file, &report, detail);
    Ok(())
}

fn print_report(file: &Path, report: &CatchReport, detail: bool) {
    println!("eval: {}", file.display());
    println!(
        "cheap parser reconstructed {} table(s); {} truth table(s) to check.\n",
        report.total_extracted,
        report.tables.len()
    );

    // Summary line per table.
    println!(
        "{:<26} {:<8} {:<7} {:<6} flagged_by",
        "table", "matched", "wrong", "iou"
    );
    println!("{}", "-".repeat(80));
    for t in &report.tables {
        let fb = t.flagged_by();
        println!(
            "{:<26} {:<8} {:<7} {:<6} {}",
            truncate(&t.name, 25),
            if t.matched { "yes" } else { "NO" },
            if t.wrong { "WRONG" } else { "ok" },
            format!("{:.2}", t.iou),
            if fb.is_empty() { "-".to_string() } else { fb.join(",") },
        );
    }

    if detail {
        for t in &report.tables {
            print_table_detail(t);
        }
    }

    println!("\n=== silent-failure catch rate ===");
    let wrong = report.n_wrong();
    println!("wrong extractions: {wrong} / {}", report.tables.len());
    match report.catch_rate() {
        Some(r) => println!(
            "CATCH RATE: {:.0}%  ({} of {} wrong extractions flagged by >=1 detector)",
            r * 100.0,
            (r * wrong as f32).round() as usize,
            wrong
        ),
        None => println!("CATCH RATE: n/a (no wrong extractions in this corpus)"),
    }
    if let Some(fp) = report.false_positive_rate() {
        println!("false-alarm rate on correct tables: {:.0}%", fp * 100.0);
    }
    println!("\nper-detector (of the {wrong} wrong, how many each caught):");
    for (name, c) in report.per_detector_catches() {
        println!("  {name:<22} {c}");
    }

    // The dangerous case: wrong extractions no detector flagged.
    let missed = report.missed();
    if missed.is_empty() {
        println!("\nMISSED (wrong but unflagged): none");
    } else {
        println!("\nMISSED (wrong but unflagged) — these are the silent failures:");
        for t in missed {
            println!("  {} — {}", t.name, t.cell_diffs.first().map(String::as_str).unwrap_or(""));
        }
        if !detail {
            println!("  (re-run with --detail to see grids and detector evidence)");
        }
    }
}

fn print_table_detail(t: &quarry::eval::TableEval) {
    let diff = t.difficulty.as_deref().unwrap_or("?");
    println!("\n{}", "═".repeat(72));
    println!(
        "▌ {}   [difficulty: {diff}]   {}",
        t.name,
        if t.wrong { "WRONG" } else { "correct" }
    );
    if !t.matched {
        println!("  no reconstructed table mapped to this region (IoU>0.3 found none)");
        return;
    }
    let (gr, gc) = t.got_dims();
    let (wr, wc) = t.want_dims();
    println!(
        "  matched {} at anchor IoU {:.2};  reconstructed {gr}x{gc}, truth {wr}x{wc}",
        t.matched_id.as_deref().unwrap_or("?"),
        t.iou
    );

    println!("\n  RECONSTRUCTED (what the cheap parser produced):");
    print_grid(&t.got_grid, "    ");
    println!("\n  GROUND TRUTH (hand-labeled):");
    print_grid(&t.want_grid, "    ");

    if t.cell_diffs.is_empty() {
        println!("\n  diff: exact match");
    } else {
        println!("\n  diff: {} divergence(s)", t.cell_diffs.len());
        for d in t.cell_diffs.iter().take(12) {
            println!("    {d}");
        }
        if t.cell_diffs.len() > 12 {
            println!("    (+{} more)", t.cell_diffs.len() - 12);
        }
    }

    if let Some(r) = &t.risk {
        println!(
            "\n  parse-time risk markers: col_count_variance {:.2}, merged_rows {}, \
             empty_cells {}, min_ocr_conf {:.2}{}",
            r.column_count_variance,
            r.merged_cell_rows,
            r.empty_cells,
            r.min_ocr_confidence,
            if r.rotated_text { ", rotated" } else { "" }
        );
    }

    println!("\n  detectors (the evidence — how we know):");
    for d in &t.detectors {
        let tag = if d.flagged {
            match d.severity {
                Some(Severity::Error) => "FLAG/ERROR",
                Some(Severity::Warn) => "FLAG/WARN",
                _ => "FLAG",
            }
        } else {
            "pass"
        };
        println!("    {:<22} {:<11} {}", d.detector, tag, d.detail);
    }

    let fb = t.flagged_by();
    let verdict = match (t.wrong, fb.is_empty()) {
        (true, false) => format!("WRONG, caught by [{}]", fb.join(", ")),
        (true, true) => "WRONG, MISSED by all detectors (silent failure)".to_string(),
        (false, false) => format!("correct, but flagged by [{}] (false alarm)", fb.join(", ")),
        (false, true) => "correct, no flags".to_string(),
    };
    println!("\n  VERDICT: {verdict}");
}

/// Render a grid as an aligned text table.
fn print_grid(grid: &[Vec<String>], indent: &str) {
    if grid.is_empty() {
        println!("{indent}(empty)");
        return;
    }
    let n_cols = grid.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut widths = vec![0usize; n_cols];
    for row in grid {
        for (c, cell) in row.iter().enumerate() {
            widths[c] = widths[c].max(cell.chars().count());
        }
    }
    for row in grid {
        let mut line = String::from(indent);
        line.push_str("│ ");
        for (c, w) in widths.iter().enumerate() {
            let cell = row.get(c).map(String::as_str).unwrap_or("");
            line.push_str(&format!("{cell:<w$} │ "));
        }
        println!("{}", line.trim_end());
    }
}

fn cmd_inspect(file: &Path) -> Result<()> {
    let (doc, doc_hash) = QDoc::load(file)?;
    println!("document {} — format {:?}", doc_hash.short(), doc.format);
    println!("{} page(s)\n", doc.pages.len());

    // Reuse the extractor's reading-order + table detection for the dump.
    let artifacts = pipeline::cheap_parse(&doc, doc_hash, 0)?;
    for page in &doc.pages {
        println!(
            "── page {} ({:.0}x{:.0}, {} spans, {} table region(s))",
            page.page,
            page.width,
            page.height,
            page.spans.len(),
            page.table_regions.len()
        );
        for a in &artifacts {
            let on_page = matches!(a.anchor(), SourceAnchor::Pdf { page: p, .. } if *p == page.page);
            if !on_page {
                continue;
            }
            match a.kind() {
                ArtifactKind::Text => {
                    let r = a.risk();
                    println!(
                        "   text  {}  (min_conf {:.2}{})",
                        a.id(),
                        r.min_ocr_confidence,
                        if r.rotated_text { ", rotated" } else { "" }
                    );
                }
                ArtifactKind::HtmlTable => {
                    if let Some(t) = a.as_any().downcast_ref::<HtmlTable>() {
                        let r = t.risk();
                        println!(
                            "   table {}  {}x{}  risk[var {:.2}, merged {}, empty {}]",
                            t.id(),
                            t.n_rows,
                            t.n_cols,
                            r.column_count_variance,
                            r.merged_cell_rows,
                            r.empty_cells
                        );
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

fn sev(s: Severity) -> &'static str {
    match s {
        Severity::Info => "info",
        Severity::Warn => "warn",
        Severity::Error => "ERROR",
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n.saturating_sub(1)])
    }
}
