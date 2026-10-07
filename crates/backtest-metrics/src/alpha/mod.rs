//! The Final Alpha Qualification System.
//!
//! One deterministic offline pipeline that takes a safely-finalized prospective
//! session and decides two things, separately:
//!
//! * **Session status** — VALID / INVALID / INDETERMINATE, from the capture's
//!   own completeness evidence. Whether the session is analysable at all.
//! * **Evidence status** — only when the session is VALID: whether Opportunity
//!   Intelligence V1 qualifies on the pre-registered contract.
//!
//! # Why the two are separate
//!
//! Conflating them is how an instrument failure becomes a model conclusion. On
//! 2026-09-16 the capture discarded 89% of the session; a pipeline that scored
//! the survivors would have produced a confident, precise and meaningless
//! answer about V1. The session gate runs first and can only stop the pipeline,
//! never soften it.
//!
//! # Why this is a new component rather than an extension
//!
//! `oi_evaluate` and `oi_attribute` deliberately compute no hit rate, precision
//! or effectiveness measure — their own doc comments say so, and say why: "a
//! tool that quietly grew those would be the easiest possible way to cross that
//! line." That boundary was correct when the evaluation belonged to someone
//! else.
//!
//! This assignment moves the evaluation in-house, so the line moves with it —
//! but deliberately, in a separate module, rather than by quietly growing the
//! join utilities. They still only join and report coverage. This evaluates.
//!
//! # The epistemic rule this module is built around
//!
//! Everything that decides *what counts as a good result* is frozen and
//! versioned before the first untouched prospective session is evaluated:
//! [`labels`] defines the independent reference opportunity, and `contract`
//! defines the qualification gates. Neither was chosen by looking at an
//! outcome, and both carry their version into every report so a result is only
//! ever comparable to another result produced by the same definitions.

pub mod labels;
pub mod dataset;
pub mod evaluate;
pub mod ladder;
pub mod sha256;
pub mod spec;
pub mod matrix;
pub mod pipeline;
pub mod report;
pub mod stats;
