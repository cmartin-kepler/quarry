//! Sidecar extractors: wrap an external parser (Docling, LiteParse, and later the
//! layout models / cloud APIs) as an `Extractor`. Each runs the tool over the
//! ORIGINAL document file (`ctx.source_path`) and feeds its output through a pure
//! adapter onto the Artifact model — after which the detector / adjudicator / eval
//! core runs unchanged.
//!
//! The command is explicit (a `Vec<String>`), so the shell-out + adapt path is
//! testable with a fixture-echoing stub and needs no real tool installed. The pure
//! adapters (`artifacts_from_docling`, `textgrid_from_json`) are tested directly.

use crate::artifact::*;
use crate::core::*;
use crate::docling::artifacts_from_docling;
use crate::extract::*;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::process::Command;

/// Run a command and capture stdout (the tool's JSON output).
fn run_capture(cmd: &[String]) -> Result<String> {
    if cmd.is_empty() {
        bail!("empty sidecar command");
    }
    let out = Command::new(&cmd[0])
        .args(&cmd[1..])
        .output()
        .with_context(|| format!("spawning sidecar `{}`", cmd.join(" ")))?;
    if !out.status.success() {
        bail!(
            "sidecar `{}` failed: {}",
            cmd.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ---------------------------------------------------------------------------
// Docling — a real table parser: DocumentRegion → HtmlTable(s).
// (Reuses the pure adapter already proven in `docling.rs`.)
// ---------------------------------------------------------------------------

pub struct DoclingSidecar {
    /// Base command; the source PDF path is appended at run time.
    pub cmd: Vec<String>,
}

impl DoclingSidecar {
    /// Default invocation: a thin Python bridge that runs Docling and prints its
    /// `DoclingDocument` JSON to stdout.
    pub fn default_cmd() -> Self {
        DoclingSidecar { cmd: vec!["python3".into(), "scripts/docling_parse.py".into()] }
    }
}

const DOC_ACCEPTS: [InputKind; 1] = [InputKind::DocumentRegion];

impl Extractor for DoclingSidecar {
    fn id(&self) -> ExtractorId {
        ExtractorId("docling".into())
    }
    fn version(&self) -> Version {
        Version(1)
    }
    fn cost_tier(&self) -> CostTier {
        CostTier(2)
    }
    fn op_kind(&self) -> OpKind {
        OpKind::Extract
    }
    fn accepts(&self) -> &[InputKind] {
        &DOC_ACCEPTS
    }
    fn produces(&self) -> ArtifactKind {
        ArtifactKind::HtmlTable
    }
    fn extract(&self, input: ExtractInput<'_>, ctx: &ExtractCtx<'_>) -> Result<Vec<Box<dyn Artifact>>> {
        let doc = match input {
            ExtractInput::DocumentRegion { doc, .. } => doc,
            ExtractInput::Artifacts(_) => bail!("docling parses the document, not artifacts"),
        };
        let mut cmd = self.cmd.clone();
        if let Some(p) = &ctx.source_path {
            cmd.push(p.display().to_string());
        }
        let json = run_capture(&cmd)?;
        artifacts_from_docling(&json, doc, ctx.generation)
    }
}

// ---------------------------------------------------------------------------
// LiteParse — crop the region, parse it: Region → TextGrid.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct LiteOut {
    #[serde(default)]
    text: String,
    #[serde(default)]
    words: Vec<LiteWord>,
}

#[derive(Deserialize)]
struct LiteWord {
    text: String,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

/// Pure adapter: a LiteParse region JSON `{ text, words:[{text,x0,y0,x1,y1}] }`
/// into a `TextGrid` derived from `parent` (the Region). The sidecar script is
/// contracted to emit word boxes in ORIGINAL PAGE coordinates (top-left), so the
/// downstream cell anchors resolve to the source.
pub fn textgrid_from_json(
    json: &str,
    doc: DocHash,
    page: u32,
    bbox: BBox,
    parent: ArtifactId,
    generation: Generation,
) -> Result<TextGrid> {
    let lo: LiteOut = serde_json::from_str(json).context("parsing LiteParse region JSON")?;
    let words: Vec<Word> = lo
        .words
        .into_iter()
        .map(|w| Word { text: w.text, bbox: BBox::new(w.x0, w.y0, w.x1, w.y1) })
        .collect();
    let content = DocHash::of(format!("litegrid:{page}:{bbox:?}:{}", lo.text).as_bytes());
    Ok(TextGrid {
        meta: Meta {
            id: ArtifactId::mint(&content, generation),
            content_hash: content,
            provenance: Provenance::Derived {
                parents: vec![parent],
                anchor: SourceAnchor::Pdf { doc, page, bbox },
            },
            generation,
            risk: RiskMarkers::default(),
        },
        text: lo.text,
        words,
    })
}

pub struct LiteParseSidecar {
    pub cmd: Vec<String>,
}

impl LiteParseSidecar {
    /// Default invocation: a Python bridge that crops the PDF to the region bbox
    /// and runs `lit`, emitting `{text, words}`.
    pub fn default_cmd() -> Self {
        LiteParseSidecar { cmd: vec!["python3".into(), "scripts/litparse_region.py".into()] }
    }
}

const LITE_ACCEPTS: [InputKind; 1] = [InputKind::Artifact(ArtifactKind::Region)];

impl Extractor for LiteParseSidecar {
    fn id(&self) -> ExtractorId {
        // Same role as the native `text-grid`, but faithful LiteParse geometry —
        // the prototype's region "text-table".
        ExtractorId("text-table".into())
    }
    fn version(&self) -> Version {
        Version(1)
    }
    fn cost_tier(&self) -> CostTier {
        CostTier(1)
    }
    fn op_kind(&self) -> OpKind {
        OpKind::Extract
    }
    fn accepts(&self) -> &[InputKind] {
        &LITE_ACCEPTS
    }
    fn produces(&self) -> ArtifactKind {
        ArtifactKind::TextGrid
    }
    fn extract(&self, input: ExtractInput<'_>, ctx: &ExtractCtx<'_>) -> Result<Vec<Box<dyn Artifact>>> {
        let arts = match input {
            ExtractInput::Artifacts(a) => a,
            ExtractInput::DocumentRegion { .. } => bail!("text-table consumes a Region artifact"),
        };
        let region = arts
            .iter()
            .find_map(|a| a.as_any().downcast_ref::<Region>())
            .ok_or_else(|| anyhow::anyhow!("text-table expects a Region input"))?;
        let (doc, page, bbox) = match region.provenance().anchor() {
            SourceAnchor::Pdf { doc, page, bbox } => (*doc, *page, *bbox),
            _ => bail!("text-table only handles PDF regions"),
        };
        let mut cmd = self.cmd.clone();
        if let Some(p) = &ctx.source_path {
            cmd.push(p.display().to_string());
        }
        cmd.push(page.to_string());
        for v in [bbox.x0, bbox.y0, bbox.x1, bbox.y1] {
            cmd.push(v.to_string());
        }
        let json = run_capture(&cmd)?;
        Ok(vec![Box::new(textgrid_from_json(&json, doc, page, bbox, region.id(), ctx.generation)?)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{DocFormat, QDoc};

    /// A stub command that echoes a fixture file (ignoring any trailing args the
    /// real tool would receive) — exercises the real shell-out + adapt path with
    /// no tool installed.
    fn echo(fixture: &str) -> Vec<String> {
        vec!["sh".into(), "tests/data/echo_json.sh".into(), fixture.into()]
    }

    fn ctx<'a>(doc: &'a QDoc) -> ExtractCtx<'a> {
        ExtractCtx { source: doc, generation: Generation(0), source_path: None }
    }

    fn refs(v: &[Box<dyn Artifact>]) -> Vec<&dyn Artifact> {
        v.iter().map(|b| b.as_ref()).collect()
    }

    #[test]
    fn docling_sidecar_runs_a_command_and_adapts_its_json() {
        let dh = DocHash::of(b"pdf");
        let doc = QDoc { format: DocFormat::Pdf, pages: vec![] };
        let anchor = SourceAnchor::Pdf { doc: dh, page: 1, bbox: BBox::new(0.0, 0.0, 600.0, 800.0) };
        let s = DoclingSidecar { cmd: echo("tests/data/sample.docling.json") };
        let out = s
            .extract(ExtractInput::DocumentRegion { doc: dh, anchor }, &ctx(&doc))
            .unwrap();
        // same two tables the pure adapter produces, now via an Extractor
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|a| a.kind() == ArtifactKind::HtmlTable));
        assert_eq!(s.op_kind(), OpKind::Extract);
    }

    #[test]
    fn liteparse_adapter_builds_a_textgrid_derived_from_the_region() {
        let dh = DocHash::of(b"pdf");
        let parent = ArtifactId("art_region".into());
        let tg = textgrid_from_json(
            include_str!("../tests/data/sample.litparse.json"),
            dh,
            1,
            BBox::new(0.0, 0.0, 200.0, 60.0),
            parent.clone(),
            Generation(0),
        )
        .unwrap();
        assert_eq!(tg.words.len(), 4);
        assert!(!tg.text.is_empty());
        assert!(matches!(tg.provenance(), Provenance::Derived { parents, .. } if parents == &vec![parent]));
    }

    #[test]
    fn liteparse_sidecar_produces_a_structurable_textgrid() {
        let dh = DocHash::of(b"pdf");
        let anchor = SourceAnchor::Pdf { doc: dh, page: 1, bbox: BBox::new(0.0, 0.0, 200.0, 60.0) };
        let region: Box<dyn Artifact> = Box::new(Region {
            meta: Meta {
                id: ArtifactId::mint(&DocHash::of(b"r"), Generation(0)),
                content_hash: DocHash::of(b"r"),
                provenance: Provenance::Source(anchor),
                generation: Generation(0),
                risk: RiskMarkers::default(),
            },
            label: "Table".into(),
            confidence: 1.0,
        });
        let doc = QDoc { format: DocFormat::Pdf, pages: vec![] };
        let s = LiteParseSidecar { cmd: echo("tests/data/sample.litparse.json") };
        let grids = s.extract(ExtractInput::Artifacts(&refs(&[region])), &ctx(&doc)).unwrap();
        let tg = grids[0].as_any().downcast_ref::<TextGrid>().unwrap();
        // and it feeds the native structurer to a real table
        let structured = crate::grid::structure_words(&tg.words, 3.0, 6.0);
        assert!(structured.rows.len() >= 2 && structured.rows[0].len() >= 2);
    }

    #[test]
    fn a_failing_sidecar_command_is_an_error_not_a_panic() {
        let dh = DocHash::of(b"pdf");
        let doc = QDoc { format: DocFormat::Pdf, pages: vec![] };
        let anchor = SourceAnchor::Pdf { doc: dh, page: 1, bbox: BBox::new(0.0, 0.0, 1.0, 1.0) };
        let s = DoclingSidecar { cmd: vec!["false".into()] }; // exits non-zero
        assert!(s.extract(ExtractInput::DocumentRegion { doc: dh, anchor }, &ctx(&doc)).is_err());
    }
}
