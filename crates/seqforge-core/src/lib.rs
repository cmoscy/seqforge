// seqforge-core: Document, ViewerState, Command, dispatch — no GUI deps

pub mod commands;
pub mod document;

pub use commands::{
    dispatch, dispatch_file, dispatch_viewer, BioOps, CommandOutput, DispatchError, FileCommand,
    Selection, SideEffect, ViewerCli, ViewerCommand, ViewerRequest, ViewerResponse, ViewerState,
};
pub use document::{CutSite, Document, Feature, FeatureKind, SearchHit, Strand, Topology};
