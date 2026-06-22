//! # Quarry — lazy, iterative document parsing for LLM agents
//!
//! An example implementation of the Phase-0 slice of the design brief: the parts
//! needed to answer the riskiest question — *are silent parse failures
//! detectable by something other than the agent?* — and nothing more.
//!
//! What's here (brief §6 Phase 0):
//! - the object-safe [`artifact::Artifact`] core + `Text`/`HtmlTable` payloads,
//! - one [`extract::PdfTextLayerReconstructor`] doing naive geometric table
//!   reconstruction from an already-extracted text layer (so it produces
//!   *realistic* silent failures); note the actual PDF-byte parsing lives in the
//!   pdfplumber bridge `scripts/pdf_to_qdoc.py`, not in this crate,
//! - three detectors: [`check::IntrinsicArithmetic`], [`check::StructuralValidity`],
//!   and claim-time [`check::AnswerSupport`],
//! - a default [`adjudicate::Adjudicator`], a flat [`store::FlatStore`] fronted by
//!   one current-view function, and the [`eval`] catch-rate harness.
//!
//! Deliberately deferred (stubbed or absent): element-identity matching across
//! re-parses, the derivation DAG + staleness, the append-only registry, PPTX/XLSX,
//! the async job queue, and the agents/indexes themselves (brief §6).

pub mod adjudicate;
pub mod analysis;
pub mod artifact;
pub mod check;
pub mod columns;
pub mod coords;
pub mod core;
pub mod doc;
pub mod docling;
pub mod evidence;
pub mod eval;
pub mod extract;
pub mod grid;
pub mod ops;
pub mod pipeline;
pub mod region_check;
pub mod route;
pub mod segment;
pub mod structure;
pub mod triage;
pub mod sidecar;
pub mod store;
