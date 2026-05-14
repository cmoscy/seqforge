// seqforge-core: Document, ViewerState, Command, dispatch — no GUI deps

pub mod commands;
pub mod document;

pub use commands::{
    dispatch_file, dispatch_viewer, CommandOutput, DispatchError, FileCommand, Selection,
    SideEffect, ViewerCli, ViewerCommand, ViewerState,
};
pub use document::{CutSite, Document, Feature, FeatureKind, SearchHit, Strand, Topology};
