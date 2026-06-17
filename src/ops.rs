//! Native (pure-Rust, no sidecar) ops over the artifact graph — the prototype's
//! op chain, each an `Extractor`:
//!
//! ```text
//!   page --(regions: Layout)--> Region
//!   Region --(text-grid: Extract)--> TextGrid
//!   TextGrid --(structure: Transform)--> HtmlTable
//!   HtmlTable --(sign-fix | markdown: Transform)--> HtmlTable
//!   [Region] --(merge: Merge)--> Region
//! ```
//!
//! They run end-to-end on the `.qdoc` fixtures with no external tools: the
//! TextGrid is built from the fixture's own positioned `Span`s. (LiteParse / the
//! cloud + layout parsers are later sidecar swap-ins for the same `Extractor`
//! shapes.)

use crate::artifact::*;
use crate::core::*;
use crate::extract::*;
use crate::grid;
use anyhow::{Result, bail};

fn meta(content: DocHash, prov: Provenance, generation: Generation, risk: RiskMarkers) -> Meta {
    Meta { id: ArtifactId::mint(&content, generation), content_hash: content, provenance: prov, generation, risk }
}

fn hash_str(s: &str) -> DocHash {
    DocHash::of(s.as_bytes())
}

/// Resolve a native op by id (for the CLI / tests).
pub fn op_by_id(id: &str) -> Option<Box<dyn Extractor>> {
    match id {
        "regions" => Some(Box::new(RegionLayout)),
        "text-grid" => Some(Box::new(TextGridExtractor)),
        "structure" => Some(Box::new(Structure)),
        "sign-fix" => Some(Box::new(SignFix)),
        "markdown" => Some(Box::new(Markdown)),
        "merge" => Some(Box::new(Merge)),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// regions — Layout: page → Region(s) from the fixture's marked table regions.
// ---------------------------------------------------------------------------

pub struct RegionLayout;
const REGION_ACCEPTS: [InputKind; 1] = [InputKind::DocumentRegion];

impl Extractor for RegionLayout {
    fn id(&self) -> ExtractorId {
        ExtractorId("regions".into())
    }
    fn version(&self) -> Version {
        Version(1)
    }
    fn cost_tier(&self) -> CostTier {
        CostTier(0)
    }
    fn op_kind(&self) -> OpKind {
        OpKind::Layout
    }
    fn accepts(&self) -> &[InputKind] {
        &REGION_ACCEPTS
    }
    fn produces(&self) -> ArtifactKind {
        ArtifactKind::Region
    }
    fn extract(&self, input: ExtractInput<'_>, ctx: &ExtractCtx<'_>) -> Result<Vec<Box<dyn Artifact>>> {
        let (doc, page_no) = match input {
            ExtractInput::DocumentRegion { doc, anchor } => match anchor {
                SourceAnchor::Pdf { page, .. } => (doc, page),
                _ => bail!("regions only handles PDF anchors"),
            },
            ExtractInput::Artifacts(_) => bail!("regions consumes a page region, not artifacts"),
        };
        let page = ctx
            .source
            .page(page_no)
            .ok_or_else(|| anyhow::anyhow!("page {page_no} not in document"))?;
        let mut out: Vec<Box<dyn Artifact>> = Vec::new();
        for (i, tr) in page.table_regions.iter().enumerate() {
            let bbox = tr.bbox();
            let anchor = SourceAnchor::Pdf { doc, page: page_no, bbox };
            let content = hash_str(&format!("region:{page_no}:{i}:{bbox:?}"));
            let risk = RiskMarkers { figure_score: tr.figure_score, ..Default::default() };
            out.push(Box::new(Region {
                meta: meta(content, Provenance::Source(anchor), ctx.generation, risk),
                label: "Table".into(),
                confidence: 1.0,
            }));
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// text-grid — Extract: Region → TextGrid (words from the source spans in the box).
// ---------------------------------------------------------------------------

pub struct TextGridExtractor;
const TEXTGRID_ACCEPTS: [InputKind; 1] = [InputKind::Artifact(ArtifactKind::Region)];

impl Extractor for TextGridExtractor {
    fn id(&self) -> ExtractorId {
        ExtractorId("text-grid".into())
    }
    fn version(&self) -> Version {
        Version(1)
    }
    fn cost_tier(&self) -> CostTier {
        CostTier(0)
    }
    fn op_kind(&self) -> OpKind {
        OpKind::Extract
    }
    fn accepts(&self) -> &[InputKind] {
        &TEXTGRID_ACCEPTS
    }
    fn produces(&self) -> ArtifactKind {
        ArtifactKind::TextGrid
    }
    fn extract(&self, input: ExtractInput<'_>, ctx: &ExtractCtx<'_>) -> Result<Vec<Box<dyn Artifact>>> {
        let arts = match input {
            ExtractInput::Artifacts(a) => a,
            ExtractInput::DocumentRegion { .. } => bail!("text-grid consumes a Region artifact"),
        };
        let region = arts
            .iter()
            .find_map(|a| a.as_any().downcast_ref::<Region>())
            .ok_or_else(|| anyhow::anyhow!("text-grid expects a Region input"))?;
        let (doc, page_no, bbox) = match region.provenance().anchor() {
            SourceAnchor::Pdf { doc, page, bbox } => (*doc, *page, *bbox),
            _ => bail!("text-grid only handles PDF regions"),
        };
        let page = ctx
            .source
            .page(page_no)
            .ok_or_else(|| anyhow::anyhow!("page {page_no} not in document"))?;
        let words: Vec<Word> = page
            .spans
            .iter()
            .filter(|s| bbox.contains_center(&s.bbox()))
            .map(|s| Word { text: s.text.clone(), bbox: s.bbox() })
            .collect();
        let text = grid::words_to_ascii(&words);
        let content = hash_str(&format!("textgrid:{page_no}:{bbox:?}:{text}"));
        let prov = Provenance::Derived { parents: vec![region.id()], anchor: SourceAnchor::Pdf { doc, page: page_no, bbox } };
        Ok(vec![Box::new(TextGrid {
            meta: meta(content, prov, ctx.generation, RiskMarkers::default()),
            text,
            words,
        })])
    }
}

// ---------------------------------------------------------------------------
// structure — Transform: TextGrid → HtmlTable (commit columns by word geometry).
// ---------------------------------------------------------------------------

pub struct Structure;
const STRUCTURE_ACCEPTS: [InputKind; 1] = [InputKind::Artifact(ArtifactKind::TextGrid)];

impl Extractor for Structure {
    fn id(&self) -> ExtractorId {
        ExtractorId("structure".into())
    }
    fn version(&self) -> Version {
        Version(1)
    }
    fn cost_tier(&self) -> CostTier {
        CostTier(0)
    }
    fn op_kind(&self) -> OpKind {
        OpKind::Transform
    }
    fn accepts(&self) -> &[InputKind] {
        &STRUCTURE_ACCEPTS
    }
    fn produces(&self) -> ArtifactKind {
        ArtifactKind::HtmlTable
    }
    fn extract(&self, input: ExtractInput<'_>, ctx: &ExtractCtx<'_>) -> Result<Vec<Box<dyn Artifact>>> {
        let arts = match input {
            ExtractInput::Artifacts(a) => a,
            ExtractInput::DocumentRegion { .. } => bail!("structure consumes a TextGrid artifact"),
        };
        let tg = arts
            .iter()
            .find_map(|a| a.as_any().downcast_ref::<TextGrid>())
            .ok_or_else(|| anyhow::anyhow!("structure expects a TextGrid input"))?;
        let (doc, page_no) = match tg.provenance().anchor() {
            SourceAnchor::Pdf { doc, page, .. } => (*doc, *page),
            _ => bail!("structure only handles PDF text grids"),
        };
        let s = grid::structure_words(&tg.words, 3.0, 6.0);
        let mut cells = Vec::new();
        for (r, row) in s.rows.iter().enumerate() {
            for (c, gc) in row.iter().enumerate() {
                cells.push(Cell {
                    row: r as u32,
                    col: c as u32,
                    text: gc.text.clone(),
                    anchor: SourceAnchor::Pdf { doc, page: page_no, bbox: gc.bbox },
                    is_header: r < s.header_rows,
                });
            }
        }
        let grid_text: Vec<Vec<String>> =
            s.rows.iter().map(|r| r.iter().map(|c| c.text.clone()).collect()).collect();
        let n_rows = grid_text.len() as u32;
        let n_cols = grid_text.iter().map(|r| r.len()).max().unwrap_or(0) as u32;
        let html = grid::to_html(&grid_text, s.header_rows);
        let content = hash_str(&html);
        let prov = Provenance::Derived { parents: vec![tg.id()], anchor: tg.anchor().clone() };
        Ok(vec![Box::new(HtmlTable {
            meta: meta(content, prov, ctx.generation, RiskMarkers::default()),
            n_rows,
            n_cols,
            cells,
            html,
        })])
    }
}

// ---------------------------------------------------------------------------
// sign-fix / markdown — Transform: HtmlTable → HtmlTable.
// ---------------------------------------------------------------------------

const TABLE_ACCEPTS: [InputKind; 1] = [InputKind::Artifact(ArtifactKind::HtmlTable)];

/// Leading rows of `t` that carry header cells.
fn header_rows_of(t: &HtmlTable) -> usize {
    let mut flag = vec![false; t.n_rows as usize];
    for c in &t.cells {
        if c.is_header && (c.row as usize) < flag.len() {
            flag[c.row as usize] = true;
        }
    }
    let mut hr = 0;
    while hr < flag.len() && flag[hr] {
        hr += 1;
    }
    hr
}

/// Build a derived HtmlTable from a transformed grid, carrying each cell's anchor
/// over from the parent by (row, col) and falling back to the table anchor.
fn rebuild(parent: &HtmlTable, grid_text: Vec<Vec<String>>, header_rows: usize, generation: Generation) -> HtmlTable {
    let table_anchor = parent.anchor().clone();
    let mut cells = Vec::new();
    for (r, row) in grid_text.iter().enumerate() {
        for (c, text) in row.iter().enumerate() {
            let anchor = parent
                .cell(r as u32, c as u32)
                .map(|pc| pc.anchor.clone())
                .unwrap_or_else(|| table_anchor.clone());
            cells.push(Cell { row: r as u32, col: c as u32, text: text.clone(), anchor, is_header: r < header_rows });
        }
    }
    let n_rows = grid_text.len() as u32;
    let n_cols = grid_text.iter().map(|r| r.len()).max().unwrap_or(0) as u32;
    let html = grid::to_html(&grid_text, header_rows);
    let content = hash_str(&html);
    let prov = Provenance::Derived { parents: vec![parent.id()], anchor: table_anchor };
    HtmlTable { meta: meta(content, prov, generation, parent.meta.risk.clone()), n_rows, n_cols, cells, html }
}

fn one_table<'a>(input: &ExtractInput<'a>, op: &str) -> Result<&'a HtmlTable> {
    match input {
        ExtractInput::Artifacts(a) => a
            .iter()
            .find_map(|x| x.as_any().downcast_ref::<HtmlTable>())
            .ok_or_else(|| anyhow::anyhow!("{op} expects an HtmlTable input")),
        ExtractInput::DocumentRegion { .. } => bail!("{op} consumes an HtmlTable artifact"),
    }
}

pub struct SignFix;

impl Extractor for SignFix {
    fn id(&self) -> ExtractorId {
        ExtractorId("sign-fix".into())
    }
    fn version(&self) -> Version {
        Version(1)
    }
    fn cost_tier(&self) -> CostTier {
        CostTier(0)
    }
    fn op_kind(&self) -> OpKind {
        OpKind::Transform
    }
    fn accepts(&self) -> &[InputKind] {
        &TABLE_ACCEPTS
    }
    fn produces(&self) -> ArtifactKind {
        ArtifactKind::HtmlTable
    }
    fn extract(&self, input: ExtractInput<'_>, ctx: &ExtractCtx<'_>) -> Result<Vec<Box<dyn Artifact>>> {
        let parent = one_table(&input, "sign-fix")?;
        let (grid_text, _changed) = grid::sign_fix(&parent.grid());
        let hr = header_rows_of(parent);
        Ok(vec![Box::new(rebuild(parent, grid_text, hr, ctx.generation))])
    }
}

pub struct Markdown;

impl Extractor for Markdown {
    fn id(&self) -> ExtractorId {
        ExtractorId("markdown".into())
    }
    fn version(&self) -> Version {
        Version(1)
    }
    fn cost_tier(&self) -> CostTier {
        CostTier(0)
    }
    fn op_kind(&self) -> OpKind {
        OpKind::Transform
    }
    fn accepts(&self) -> &[InputKind] {
        &TABLE_ACCEPTS
    }
    fn produces(&self) -> ArtifactKind {
        ArtifactKind::HtmlTable
    }
    fn extract(&self, input: ExtractInput<'_>, ctx: &ExtractCtx<'_>) -> Result<Vec<Box<dyn Artifact>>> {
        let parent = one_table(&input, "markdown")?;
        let md = grid::to_markdown(&parent.grid());
        let reparsed = grid::markdown_to_grid(&md);
        let grid_text = if reparsed.is_empty() { parent.grid() } else { reparsed };
        let hr = if grid_text.is_empty() { 0 } else { 1 };
        Ok(vec![Box::new(rebuild(parent, grid_text, hr, ctx.generation))])
    }
}

// ---------------------------------------------------------------------------
// merge — Merge: [Region] → Region (per-edge median bbox; cross-model agreement).
// ---------------------------------------------------------------------------

pub struct Merge;

impl Extractor for Merge {
    fn id(&self) -> ExtractorId {
        ExtractorId("merge".into())
    }
    fn version(&self) -> Version {
        Version(1)
    }
    fn cost_tier(&self) -> CostTier {
        CostTier(0)
    }
    fn op_kind(&self) -> OpKind {
        OpKind::Merge
    }
    fn accepts(&self) -> &[InputKind] {
        &REGION_ACCEPTS_M
    }
    fn produces(&self) -> ArtifactKind {
        ArtifactKind::Region
    }
    fn extract(&self, input: ExtractInput<'_>, ctx: &ExtractCtx<'_>) -> Result<Vec<Box<dyn Artifact>>> {
        let arts = match input {
            ExtractInput::Artifacts(a) => a,
            ExtractInput::DocumentRegion { .. } => bail!("merge consumes Region artifacts"),
        };
        let regions: Vec<&Region> =
            arts.iter().filter_map(|a| a.as_any().downcast_ref::<Region>()).collect();
        if regions.len() < 2 {
            bail!("merge needs at least 2 Region inputs (got {})", regions.len());
        }
        let (doc, page_no) = match regions[0].provenance().anchor() {
            SourceAnchor::Pdf { doc, page, .. } => (*doc, *page),
            _ => bail!("merge only handles PDF regions"),
        };
        let boxes: Vec<BBox> = regions.iter().map(|r| r.bbox()).collect();
        let bbox = grid::median_bbox(&boxes).expect("≥2 boxes");
        let parents: Vec<ArtifactId> = regions.iter().map(|r| r.id()).collect();
        let content = hash_str(&format!("merge:{page_no}:{bbox:?}"));
        let prov = Provenance::Derived { parents, anchor: SourceAnchor::Pdf { doc, page: page_no, bbox } };
        Ok(vec![Box::new(Region {
            meta: meta(content, prov, ctx.generation, RiskMarkers::default()),
            label: "Table".into(),
            confidence: 1.0,
        })])
    }
}

const REGION_ACCEPTS_M: [InputKind; 1] = [InputKind::Artifact(ArtifactKind::Region)];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{DocFormat, Page, QDoc, Span, TableRegion};

    fn span(t: &str, x0: f32, y0: f32, x1: f32, y1: f32) -> Span {
        Span { text: t.into(), bbox: [x0, y0, x1, y1], confidence: 1.0, rotated: false }
    }

    /// A fixture: one page, one marked table region covering a 2-col, header +
    /// 2-row table whose total reconciles.
    fn fixture() -> (QDoc, DocHash) {
        let page = Page {
            page: 1,
            width: 600.0,
            height: 800.0,
            spans: vec![
                span("Segment", 0.0, 0.0, 40.0, 10.0),
                span("Revenue", 100.0, 0.0, 140.0, 10.0),
                span("Sports", 0.0, 20.0, 35.0, 30.0),
                span("100", 100.0, 20.0, 135.0, 30.0),
                span("Parks", 0.0, 40.0, 35.0, 50.0),
                span("220", 100.0, 40.0, 135.0, 50.0),
                span("Total", 0.0, 60.0, 35.0, 70.0),
                span("320", 100.0, 60.0, 135.0, 70.0),
            ],
            table_regions: vec![TableRegion { bbox: [0.0, 0.0, 200.0, 80.0], note: None, figure_score: 0.0 }],
        };
        (QDoc { format: DocFormat::Pdf, pages: vec![page] }, DocHash::of(b"fixture"))
    }

    fn ctx<'a>(doc: &'a QDoc) -> ExtractCtx<'a> {
        ExtractCtx { source: doc, generation: Generation(0) }
    }

    fn refs(v: &[Box<dyn Artifact>]) -> Vec<&dyn Artifact> {
        v.iter().map(|b| b.as_ref()).collect()
    }

    #[test]
    fn op_kinds_are_what_we_expect() {
        assert_eq!(RegionLayout.op_kind(), OpKind::Layout);
        assert_eq!(TextGridExtractor.op_kind(), OpKind::Extract);
        assert_eq!(Structure.op_kind(), OpKind::Transform);
        assert_eq!(SignFix.op_kind(), OpKind::Transform);
        assert_eq!(Markdown.op_kind(), OpKind::Transform);
        assert_eq!(Merge.op_kind(), OpKind::Merge);
    }

    #[test]
    fn op_by_id_resolves_all_native_ops() {
        for id in ["regions", "text-grid", "structure", "sign-fix", "markdown", "merge"] {
            assert_eq!(op_by_id(id).unwrap().id(), ExtractorId(id.into()));
        }
        assert!(op_by_id("nope").is_none());
    }

    #[test]
    fn layout_makes_one_region_per_marked_region() {
        let (doc, dh) = fixture();
        let anchor = SourceAnchor::Pdf { doc: dh, page: 1, bbox: BBox::new(0.0, 0.0, 600.0, 800.0) };
        let out = RegionLayout
            .extract(ExtractInput::DocumentRegion { doc: dh, anchor }, &ctx(&doc))
            .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].kind(), ArtifactKind::Region);
        let r = out[0].as_any().downcast_ref::<Region>().unwrap();
        assert_eq!(r.bbox(), BBox::new(0.0, 0.0, 200.0, 80.0));
        assert!(matches!(r.provenance(), Provenance::Source(_)));
    }

    #[test]
    fn the_native_chain_runs_region_to_table() {
        let (doc, dh) = fixture();
        let cx = ctx(&doc);
        let anchor = SourceAnchor::Pdf { doc: dh, page: 1, bbox: BBox::new(0.0, 0.0, 600.0, 800.0) };

        let regions = RegionLayout.extract(ExtractInput::DocumentRegion { doc: dh, anchor }, &cx).unwrap();
        let grids = TextGridExtractor.extract(ExtractInput::Artifacts(&refs(&regions)), &cx).unwrap();
        assert_eq!(grids.len(), 1);
        let tg = grids[0].as_any().downcast_ref::<TextGrid>().unwrap();
        assert_eq!(tg.words.len(), 8);
        assert!(!tg.text.is_empty());
        assert!(matches!(tg.provenance(), Provenance::Derived { parents, .. } if parents.len() == 1));

        let tables = Structure.extract(ExtractInput::Artifacts(&refs(&grids)), &cx).unwrap();
        let t = tables[0].as_any().downcast_ref::<HtmlTable>().unwrap();
        assert_eq!(t.n_rows, 4);
        assert_eq!(t.n_cols, 2);
        assert_eq!(header_rows_of(t), 1);
        // every cell carries a resolved anchor in this document
        assert!(t.cells.iter().all(|c| matches!(&c.anchor, SourceAnchor::Pdf { doc, .. } if *doc == dh)));
        // and it reconciles: 100 + 220 == 320
        let grid = t.grid();
        assert_eq!(grid[3], vec!["Total".to_string(), "320".into()]);
    }

    #[test]
    fn structure_table_reconciles_via_the_arithmetic_check() {
        use crate::analysis::{TableModel, reconcile};
        let (doc, dh) = fixture();
        let cx = ctx(&doc);
        let anchor = SourceAnchor::Pdf { doc: dh, page: 1, bbox: BBox::new(0.0, 0.0, 600.0, 800.0) };
        let regions = RegionLayout.extract(ExtractInput::DocumentRegion { doc: dh, anchor }, &cx).unwrap();
        let grids = TextGridExtractor.extract(ExtractInput::Artifacts(&refs(&regions)), &cx).unwrap();
        let tables = Structure.extract(ExtractInput::Artifacts(&refs(&grids)), &cx).unwrap();
        let t = tables[0].as_any().downcast_ref::<HtmlTable>().unwrap();
        let recon = reconcile(&TableModel::from_table(t), 0.01, 0.06);
        assert!(recon.iter().any(|tr| tr.all_ok()), "the structured total should reconcile");
    }

    #[test]
    fn sign_fix_transforms_a_table_and_keeps_anchors() {
        // a tiny HtmlTable with a parenthesised negative
        let dh = DocHash::of(b"d");
        let anchor = SourceAnchor::Pdf { doc: dh, page: 1, bbox: BBox::new(0.0, 0.0, 10.0, 10.0) };
        let cell = |r: u32, c: u32, t: &str, h: bool| Cell {
            row: r,
            col: c,
            text: t.into(),
            anchor: SourceAnchor::Pdf { doc: dh, page: 1, bbox: BBox::new(c as f32, r as f32, c as f32 + 1.0, r as f32 + 1.0) },
            is_header: h,
        };
        let parent = HtmlTable {
            meta: meta(DocHash::of(b"p"), Provenance::Source(anchor), Generation(0), RiskMarkers::default()),
            n_rows: 2,
            n_cols: 2,
            cells: vec![
                cell(0, 0, "Item", true),
                cell(0, 1, "Amt", true),
                cell(1, 0, "A", false),
                cell(1, 1, "(902)", false),
            ],
            html: String::new(),
        };
        let doc = QDoc { format: DocFormat::Pdf, pages: vec![] };
        let arts: Vec<&dyn Artifact> = vec![&parent];
        let out = SignFix.extract(ExtractInput::Artifacts(&arts), &ctx(&doc)).unwrap();
        let t = out[0].as_any().downcast_ref::<HtmlTable>().unwrap();
        assert_eq!(t.cell(1, 1).unwrap().text, "-902");
        // header preserved, anchor carried from the parent cell
        assert!(t.cell(0, 0).unwrap().is_header);
        assert_eq!(t.cell(1, 1).unwrap().anchor, parent.cell(1, 1).unwrap().anchor);
        assert!(matches!(t.provenance(), Provenance::Derived { parents, .. } if parents == &vec![parent.id()]));
    }

    #[test]
    fn merge_returns_the_median_region() {
        let dh = DocHash::of(b"d");
        let mk = |bbox: BBox| -> Box<dyn Artifact> {
            let anchor = SourceAnchor::Pdf { doc: dh, page: 2, bbox };
            Box::new(Region {
                meta: meta(hash_str(&format!("{bbox:?}")), Provenance::Source(anchor), Generation(0), RiskMarkers::default()),
                label: "Table".into(),
                confidence: 1.0,
            })
        };
        // two near-identical boxes + one left-clipped outlier
        let regions = vec![
            mk(BBox::new(50.0, 100.0, 540.0, 300.0)),
            mk(BBox::new(52.0, 102.0, 538.0, 302.0)),
            mk(BBox::new(224.0, 101.0, 539.0, 301.0)),
        ];
        let doc = QDoc { format: DocFormat::Pdf, pages: vec![] };
        let out = Merge.extract(ExtractInput::Artifacts(&refs(&regions)), &ctx(&doc)).unwrap();
        let r = out[0].as_any().downcast_ref::<Region>().unwrap();
        // x0 median = 52 (the clipped 224 is outvoted)
        assert_eq!(r.bbox().x0, 52.0);
        assert!(matches!(r.provenance(), Provenance::Derived { parents, .. } if parents.len() == 3));
    }

    #[test]
    fn merge_needs_at_least_two_regions() {
        let dh = DocHash::of(b"d");
        let anchor = SourceAnchor::Pdf { doc: dh, page: 1, bbox: BBox::new(0.0, 0.0, 1.0, 1.0) };
        let one: Box<dyn Artifact> = Box::new(Region {
            meta: meta(hash_str("x"), Provenance::Source(anchor), Generation(0), RiskMarkers::default()),
            label: "Table".into(),
            confidence: 1.0,
        });
        let doc = QDoc { format: DocFormat::Pdf, pages: vec![] };
        assert!(Merge.extract(ExtractInput::Artifacts(&refs(&[one])), &ctx(&doc)).is_err());
    }

    /// Intent of `text-grid`: it reads the source scoped to the REGION — words
    /// outside the located box (a footnote below the table) must not leak in.
    #[test]
    fn text_grid_reads_only_words_inside_the_region() {
        let page = Page {
            page: 1,
            width: 600.0,
            height: 800.0,
            spans: vec![
                span("In", 0.0, 0.0, 20.0, 10.0),
                span("box", 100.0, 0.0, 140.0, 10.0),
                span("footnote", 0.0, 500.0, 80.0, 510.0), // well below the region
            ],
            table_regions: vec![TableRegion { bbox: [0.0, 0.0, 200.0, 60.0], note: None, figure_score: 0.0 }],
        };
        let doc = QDoc { format: DocFormat::Pdf, pages: vec![page] };
        let dh = DocHash::of(b"scoped");
        let cx = ctx(&doc);
        let anchor = SourceAnchor::Pdf { doc: dh, page: 1, bbox: BBox::new(0.0, 0.0, 600.0, 800.0) };
        let regions = RegionLayout.extract(ExtractInput::DocumentRegion { doc: dh, anchor }, &cx).unwrap();
        let grids = TextGridExtractor.extract(ExtractInput::Artifacts(&refs(&regions)), &cx).unwrap();
        let tg = grids[0].as_any().downcast_ref::<TextGrid>().unwrap();
        let words: Vec<&str> = tg.words.iter().map(|w| w.text.as_str()).collect();
        assert!(words.contains(&"In") && words.contains(&"box"));
        assert!(!words.contains(&"footnote"), "words outside the region must be excluded");
    }

    /// Intent of `merge`: when detections AGREE, the consensus region is their
    /// shared box (agreement → one region, not an argument).
    #[test]
    fn merge_collapses_agreeing_detections_to_their_shared_box() {
        let dh = DocHash::of(b"d");
        let mk = |bbox: BBox| -> Box<dyn Artifact> {
            let anchor = SourceAnchor::Pdf { doc: dh, page: 1, bbox };
            Box::new(Region {
                meta: meta(hash_str(&format!("{bbox:?}")), Provenance::Source(anchor), Generation(0), RiskMarkers::default()),
                label: "Table".into(),
                confidence: 1.0,
            })
        };
        let same = BBox::new(50.0, 100.0, 540.0, 300.0);
        let regions = vec![mk(same), mk(same)];
        let doc = QDoc { format: DocFormat::Pdf, pages: vec![] };
        let out = Merge.extract(ExtractInput::Artifacts(&refs(&regions)), &ctx(&doc)).unwrap();
        assert_eq!(out[0].as_any().downcast_ref::<Region>().unwrap().bbox(), same);
    }

    /// Intent of the graph as a whole: every op APPENDS a derived artifact that
    /// names its parent(s) and carries a RESOLVED source anchor — so the table a
    /// claim cites traces back to a box in the original bytes in O(1).
    #[test]
    fn the_chain_forms_a_provenance_dag_with_resolved_anchors() {
        let (doc, dh) = fixture();
        let cx = ctx(&doc);
        let anchor = SourceAnchor::Pdf { doc: dh, page: 1, bbox: BBox::new(0.0, 0.0, 600.0, 800.0) };
        let regions = RegionLayout.extract(ExtractInput::DocumentRegion { doc: dh, anchor }, &cx).unwrap();
        let region_id = regions[0].id();
        let grids = TextGridExtractor.extract(ExtractInput::Artifacts(&refs(&regions)), &cx).unwrap();
        let tg_id = grids[0].id();
        let tables = Structure.extract(ExtractInput::Artifacts(&refs(&grids)), &cx).unwrap();

        // text-grid derives from the region
        assert!(matches!(grids[0].provenance(), Provenance::Derived { parents, .. } if parents == &vec![region_id]));
        // table derives from the text-grid, and its anchor is a resolved PDF box
        match tables[0].provenance() {
            Provenance::Derived { parents, anchor } => {
                assert_eq!(parents, &vec![tg_id]);
                assert!(matches!(anchor, SourceAnchor::Pdf { doc, .. } if *doc == dh));
            }
            _ => panic!("structured table should be Derived"),
        }
    }

    #[test]
    fn stored_artifact_round_trips_region_and_textgrid() {
        let dh = DocHash::of(b"d");
        let anchor = SourceAnchor::Pdf { doc: dh, page: 1, bbox: BBox::new(0.0, 0.0, 1.0, 1.0) };
        let region = Region {
            meta: meta(hash_str("r"), Provenance::Source(anchor.clone()), Generation(0), RiskMarkers::default()),
            label: "Table".into(),
            confidence: 0.9,
        };
        let tg = TextGrid {
            meta: meta(hash_str("t"), Provenance::Source(anchor), Generation(0), RiskMarkers::default()),
            text: "a b".into(),
            words: vec![Word { text: "a".into(), bbox: BBox::new(0.0, 0.0, 1.0, 1.0) }],
        };
        for a in [
            StoredArtifact::from_dyn(&region).unwrap(),
            StoredArtifact::from_dyn(&tg).unwrap(),
        ] {
            let json = serde_json::to_string(&a).unwrap();
            let back: StoredArtifact = serde_json::from_str(&json).unwrap();
            assert_eq!(back.meta().id, a.meta().id);
            // and the dyn round-trips with the right kind
            assert_eq!(back.into_dyn().kind(), a.into_dyn().kind());
        }
    }
}
