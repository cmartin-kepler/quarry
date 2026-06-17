//! Quarry CLI (brief §5). `eval` is the point of the build; the rest support it.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use quarry::adjudicate::{Adjudicator, DefaultAdjudicator};
use quarry::artifact::{Artifact, ArtifactKind, HtmlTable};
use quarry::check::{CheckCtx, CheckOutcome, IntrinsicArithmetic, QualityCheck, StructuralValidity};
use quarry::core::{SourceAnchor, Severity};
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
    /// named op with `--op` (e.g. pdf-text, docling, reducto, yolo26) or a tier;
    /// sidecars run on `--source` (the original file) if it isn't `file` itself.
    Parse {
        file: PathBuf,
        #[arg(long)]
        op: Option<String>,
        #[arg(long, default_value_t = 0)]
        tier: u8,
        #[arg(long)]
        source: Option<PathBuf>,
        #[arg(long)]
        out: PathBuf,
    },
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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Command::Parse { file, op, tier, source, out } => {
            cmd_parse(&file, op.as_deref(), tier, source.as_deref(), &out)
        }
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
    }
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

fn cmd_parse(
    file: &Path,
    op: Option<&str>,
    tier: u8,
    source: Option<&Path>,
    out: &Path,
) -> Result<()> {
    // A `.qdoc` loads as the native text-layer context; any other file (a real
    // PDF for a sidecar) gets an empty context + a doc_hash of its bytes.
    let (doc, doc_hash) = match QDoc::load(file) {
        Ok(x) => x,
        Err(_) => {
            let bytes = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
            (QDoc { format: quarry::doc::DocFormat::Pdf, pages: vec![] }, quarry::core::DocHash::of(&bytes))
        }
    };
    // sidecars run the tool on `--source`, defaulting to `file` itself.
    let source_path = Some(source.unwrap_or(file).to_path_buf());

    let extractor = match op {
        Some(id) => pipeline::extractor_by_id(id)
            .ok_or_else(|| anyhow::anyhow!("unknown op `{id}`"))?,
        None => pipeline::extractor_for(doc.format, tier)?,
    };
    let artifacts = pipeline::run_document_extractor(&doc, doc_hash, source_path, extractor.as_ref())?;
    let n_tables = adjudicate_and_store(&artifacts, doc_hash, out)?;
    println!(
        "parsed {} with `{}` (doc {}) → {} artifact(s), {} table(s) → {}",
        file.display(),
        op.unwrap_or("pdf-text"),
        doc_hash.short(),
        artifacts.len(),
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
