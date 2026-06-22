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
        DoclingSidecar { cmd: vec!["uv".into(), "run".into(), "scripts/run_docling.py".into()] }
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
            origin: Origin::default(),
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
        LiteParseSidecar { cmd: vec!["uv".into(), "run".into(), "scripts/litparse_region.py".into()] }
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

// ---------------------------------------------------------------------------
// Layout models — detect regions: DocumentRegion(page) → Region(s).
// (YOLO / DocLayout-YOLO / Surya, each a `LayoutSidecar` with its own command.)
// ---------------------------------------------------------------------------

fn bbox4(a: [f32; 4]) -> BBox {
    BBox::new(a[0], a[1], a[2], a[3])
}

#[derive(Deserialize)]
struct LayoutOut {
    #[serde(default)]
    regions: Vec<LayoutRegion>,
    /// Render scale (pixels-per-point) when the model reported bboxes in image
    /// PIXELS (e.g. YOLO over a rendered page). Absent/1.0 ⇒ bboxes are already in
    /// PDF points. The adapter owns coordinate map #1 (build-plan §3), so the
    /// conversion is pinned in tested Rust rather than trusted to the sidecar.
    #[serde(default)]
    scale: Option<f32>,
}
#[derive(Deserialize)]
struct LayoutRegion {
    #[serde(default = "table_label")]
    label: String,
    #[serde(default = "one")]
    confidence: f32,
    bbox: [f32; 4],
}
fn table_label() -> String {
    "Table".into()
}
fn one() -> f32 {
    1.0
}

/// Pure adapter: a layout model's `{regions:[{label,confidence,bbox}]}` (page
/// top-left coords) into Region artifacts on `page`.
pub fn regions_from_json(json: &str, doc: DocHash, page: u32, generation: Generation) -> Result<Vec<Region>> {
    let lo: LayoutOut = serde_json::from_str(json).context("parsing layout JSON")?;
    let scale = lo.scale.unwrap_or(1.0);
    Ok(lo
        .regions
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            // coordinate map #1: pixels → points when the model rendered at `scale`;
            // scale 1.0 means the boxes are already in points (back-compat).
            let bbox = if scale != 1.0 {
                crate::coords::pixels_to_points(bbox4(r.bbox), scale)
            } else {
                bbox4(r.bbox)
            };
            let content = DocHash::of(format!("layout:{page}:{i}:{}:{bbox:?}", r.label).as_bytes());
            Region {
                meta: Meta {
                    id: ArtifactId::mint(&content, generation),
                    content_hash: content,
                    provenance: Provenance::Source(SourceAnchor::Pdf { doc, page, bbox }),
                    generation,
                    risk: RiskMarkers::default(),
                    origin: Origin::default(),
                },
                label: r.label,
                confidence: r.confidence,
            }
        })
        .collect())
}

pub struct LayoutSidecar {
    /// Model id (e.g. "yolo26n", "surya"); becomes the ExtractorId.
    pub model: String,
    pub cmd: Vec<String>,
}

impl LayoutSidecar {
    /// Default invocation: the `layout_detect.py` bridge run through `uv` (PEP 723
    /// per-script env — ultralytics/doclayout-yolo, isolated from docling's env). It
    /// renders the page and runs the named model. Source path + page number are
    /// appended at run time. Output bboxes are in PDF points (the bridge converts).
    pub fn model(model: &str) -> Self {
        LayoutSidecar {
            model: model.into(),
            cmd: vec![
                "uv".into(),
                "run".into(),
                "scripts/layout_detect.py".into(),
                model.into(),
            ],
        }
    }
}

impl Extractor for LayoutSidecar {
    fn id(&self) -> ExtractorId {
        ExtractorId(self.model.clone())
    }
    fn version(&self) -> Version {
        Version(1)
    }
    fn cost_tier(&self) -> CostTier {
        CostTier(1)
    }
    fn op_kind(&self) -> OpKind {
        OpKind::Layout
    }
    fn accepts(&self) -> &[InputKind] {
        &DOC_ACCEPTS
    }
    fn produces(&self) -> ArtifactKind {
        ArtifactKind::Region
    }
    fn extract(&self, input: ExtractInput<'_>, ctx: &ExtractCtx<'_>) -> Result<Vec<Box<dyn Artifact>>> {
        let (doc, page) = match input {
            ExtractInput::DocumentRegion { doc, anchor } => match anchor {
                SourceAnchor::Pdf { page, .. } => (doc, page),
                _ => bail!("layout only handles PDF anchors"),
            },
            ExtractInput::Artifacts(_) => bail!("layout detects on a page, not artifacts"),
        };
        let mut cmd = self.cmd.clone();
        if let Some(p) = &ctx.source_path {
            cmd.push(p.display().to_string());
        }
        cmd.push(page.to_string());
        let json = run_capture(&cmd)?;
        Ok(regions_from_json(&json, doc, page, ctx.generation)?
            .into_iter()
            .map(|r| Box::new(r) as Box<dyn Artifact>)
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Cloud table parsers — Reducto / LlamaParse: DocumentRegion → HtmlTable(s).
// A generic cell-based contract; each parser's bridge converts its native output.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CloudOut {
    #[serde(default)]
    tables: Vec<CloudTable>,
}
#[derive(Deserialize)]
struct CloudTable {
    page: u32,
    bbox: [f32; 4],
    n_rows: u32,
    n_cols: u32,
    cells: Vec<CloudCell>,
}
#[derive(Deserialize)]
struct CloudCell {
    row: u32,
    col: u32,
    text: String,
    #[serde(default)]
    bbox: Option<[f32; 4]>,
    #[serde(default)]
    is_header: bool,
}

/// Pure adapter: a cloud parser's `{tables:[{page,bbox,n_rows,n_cols,cells}]}`
/// (cells with their own page-coord boxes) into HtmlTables.
pub fn tables_from_json(json: &str, doc: DocHash, generation: Generation) -> Result<Vec<HtmlTable>> {
    let co: CloudOut = serde_json::from_str(json).context("parsing cloud table JSON")?;
    let mut out = Vec::new();
    for t in co.tables {
        let table_bbox = bbox4(t.bbox);
        let cells: Vec<Cell> = t
            .cells
            .iter()
            .map(|c| Cell {
                row: c.row,
                col: c.col,
                text: c.text.clone(),
                anchor: SourceAnchor::Pdf { doc, page: t.page, bbox: c.bbox.map(bbox4).unwrap_or(table_bbox) },
                is_header: c.is_header,
            })
            .collect();
        // dense grid + header rows for rendering
        let mut grid = vec![vec![String::new(); t.n_cols as usize]; t.n_rows as usize];
        let mut hdr = vec![false; t.n_rows as usize];
        for c in &t.cells {
            if (c.row as usize) < grid.len() && (c.col as usize) < t.n_cols as usize {
                grid[c.row as usize][c.col as usize] = c.text.clone();
                if c.is_header {
                    hdr[c.row as usize] = true;
                }
            }
        }
        let header_rows = hdr.iter().take_while(|&&h| h).count();
        let html = crate::grid::to_html(&grid, header_rows);
        let content = DocHash::of(html.as_bytes());
        out.push(HtmlTable {
            meta: Meta {
                id: ArtifactId::mint(&content, generation),
                content_hash: content,
                provenance: Provenance::Source(SourceAnchor::Pdf { doc, page: t.page, bbox: table_bbox }),
                generation,
                risk: RiskMarkers::default(),
                origin: Origin::default(),
            },
            n_rows: t.n_rows,
            n_cols: t.n_cols,
            cells,
            html,
        });
    }
    Ok(out)
}

pub struct TableSidecar {
    /// Parser id (e.g. "reducto", "llamaparse").
    pub parser: String,
    pub cmd: Vec<String>,
}

impl TableSidecar {
    /// Default invocation: a Python bridge for the named cloud parser, converting
    /// its native output to the generic cell-based table contract.
    pub fn parser(parser: &str) -> Self {
        TableSidecar {
            parser: parser.into(),
            cmd: vec!["python3".into(), "scripts/cloud_parse.py".into(), parser.into()],
        }
    }
}

impl Extractor for TableSidecar {
    fn id(&self) -> ExtractorId {
        ExtractorId(self.parser.clone())
    }
    fn version(&self) -> Version {
        Version(1)
    }
    fn cost_tier(&self) -> CostTier {
        CostTier(3) // metered cloud APIs — the most expensive tier
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
            ExtractInput::Artifacts(_) => bail!("{} parses the document, not artifacts", self.parser),
        };
        let mut cmd = self.cmd.clone();
        if let Some(p) = &ctx.source_path {
            cmd.push(p.display().to_string());
        }
        let json = run_capture(&cmd)?;
        Ok(tables_from_json(&json, doc, ctx.generation)?
            .into_iter()
            .map(|t| Box::new(t) as Box<dyn Artifact>)
            .collect())
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
                origin: Origin::default(),
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

    #[test]
    fn layout_sidecar_detects_regions_on_a_page() {
        let dh = DocHash::of(b"pdf");
        let doc = QDoc { format: DocFormat::Pdf, pages: vec![] };
        let anchor = SourceAnchor::Pdf { doc: dh, page: 2, bbox: BBox::new(0.0, 0.0, 600.0, 800.0) };
        let s = LayoutSidecar { model: "yolo26n".into(), cmd: echo("tests/data/sample.layout.json") };
        let out = s.extract(ExtractInput::DocumentRegion { doc: dh, anchor }, &ctx(&doc)).unwrap();
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|a| a.kind() == ArtifactKind::Region));
        let r = out[0].as_any().downcast_ref::<Region>().unwrap();
        assert_eq!(r.label, "Table");
        assert!((r.confidence - 0.97).abs() < 1e-3);
        assert_eq!(s.op_kind(), OpKind::Layout);
        assert_eq!(s.id(), ExtractorId("yolo26n".into()));
    }

    #[test]
    fn regions_adapter_places_each_region_on_its_page() {
        let rs = regions_from_json(
            include_str!("../tests/data/sample.layout.json"),
            DocHash::of(b"d"),
            2,
            Generation(0),
        )
        .unwrap();
        assert_eq!(rs.len(), 2);
        assert_eq!(rs[0].bbox(), BBox::new(50.0, 100.0, 540.0, 300.0));
        match rs[0].provenance().anchor() {
            SourceAnchor::Pdf { page, .. } => assert_eq!(*page, 2),
            _ => panic!("expected a PDF region anchor"),
        }
    }

    #[test]
    fn regions_adapter_converts_pixel_boxes_at_scale() {
        // coordinate map #1: a model reporting pixels at 2x render scale → the
        // adapter halves them into PDF points. Without `scale` (prior test) the
        // boxes are taken as points unchanged.
        let json = r#"{"scale":2.0,"regions":[{"label":"Table","bbox":[100.0,200.0,1080.0,600.0]}]}"#;
        let rs = regions_from_json(json, DocHash::of(b"d"), 1, Generation(0)).unwrap();
        assert_eq!(rs.len(), 1);
        assert_eq!(rs[0].bbox(), BBox::new(50.0, 100.0, 540.0, 300.0), "pixels/2 = points");
        assert_eq!(rs[0].role(), crate::artifact::RegionRole::Table);
    }

    #[test]
    fn cloud_table_sidecar_makes_tables_the_detectors_accept() {
        use crate::check::{CheckCtx, IntrinsicArithmetic, QualityCheck};
        let dh = DocHash::of(b"pdf");
        let doc = QDoc { format: DocFormat::Pdf, pages: vec![] };
        let anchor = SourceAnchor::Pdf { doc: dh, page: 1, bbox: BBox::new(0.0, 0.0, 600.0, 800.0) };
        let s = TableSidecar { parser: "reducto".into(), cmd: echo("tests/data/sample.cloudtable.json") };
        let out = s.extract(ExtractInput::DocumentRegion { doc: dh, anchor }, &ctx(&doc)).unwrap();
        assert_eq!(out.len(), 1);
        let t = out[0].as_any().downcast_ref::<HtmlTable>().unwrap();
        assert_eq!((t.n_rows, t.n_cols), (4, 2));
        assert!(t.cell(0, 0).unwrap().is_header);
        // the detector core runs on a cloud-sourced table just like any other:
        // 100 + 220 == 320 reconciles, so no false flag.
        let dummy = QDoc { format: DocFormat::Pdf, pages: vec![] };
        let cctx = CheckCtx { source: &dummy };
        assert!(!IntrinsicArithmetic::default().check(out[0].as_ref(), &cctx).is_flag());
        assert_eq!(s.cost_tier(), CostTier(3), "metered cloud APIs are the top tier");
    }
}
