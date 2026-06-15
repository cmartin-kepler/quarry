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
    /// Run the tier-n extractor and emit artifacts (HTML + manifest) to <out>.
    Parse {
        file: PathBuf,
        #[arg(long, default_value_t = 0)]
        tier: u8,
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
    },
    /// Dump document structure: pages, elements, anchors, reading order.
    Inspect { file: PathBuf },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Command::Parse { file, tier, out } => cmd_parse(&file, tier, &out),
        Command::Check { artifact_dir } => cmd_check(&artifact_dir),
        Command::Eval { file, truth, tier } => cmd_eval(&file, &truth, tier),
        Command::Inspect { file } => cmd_inspect(&file),
    }
}

fn cmd_parse(file: &Path, tier: u8, out: &Path) -> Result<()> {
    let (doc, doc_hash) = QDoc::load(file)?;
    let artifacts = pipeline::cheap_parse(&doc, doc_hash, tier)?;

    // Adjudicate per anchor group. In Phase 0 there's one candidate per anchor,
    // so this mostly records check outcomes as append-only verdicts.
    let arithmetic = IntrinsicArithmetic::default();
    let structural = StructuralValidity;
    let ctx = CheckCtx { source: &doc };
    let adj = DefaultAdjudicator;

    let mut verdicts = Vec::new();
    for a in &artifacts {
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

    let store = FlatStore::open(out);
    store.write(doc_hash, &artifacts, &verdicts)?;

    let n_tables = artifacts
        .iter()
        .filter(|a| a.kind() == ArtifactKind::HtmlTable)
        .count();
    println!(
        "parsed {} (doc {}) → {} artifact(s), {} table(s) → {}",
        file.display(),
        doc_hash.short(),
        artifacts.len(),
        n_tables,
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

fn cmd_eval(file: &Path, truth: &Path, tier: u8) -> Result<()> {
    let (doc, doc_hash) = QDoc::load(file)?;
    let truth = GroundTruth::load(truth)?;
    let report = run_eval(&doc, doc_hash, &truth, tier)?;
    print_report(file, &report);
    Ok(())
}

fn print_report(file: &Path, report: &CatchReport) {
    println!("eval: {}\n", file.display());
    println!(
        "{:<26} {:<8} {:<7} {:<30} detail",
        "table", "matched", "wrong", "flagged_by"
    );
    println!("{}", "-".repeat(96));
    for t in &report.tables {
        println!(
            "{:<26} {:<8} {:<7} {:<30} {}",
            truncate(&t.name, 25),
            if t.matched { "yes" } else { "NO" },
            if t.wrong { "WRONG" } else { "ok" },
            if t.flagged_by.is_empty() {
                "-".to_string()
            } else {
                t.flagged_by.join(",")
            },
            truncate(&t.diff_summary, 40),
        );
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
