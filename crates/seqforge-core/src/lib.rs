// seqforge-core: Document, ViewerState, Command, dispatch — no GUI deps

pub mod commands;
pub mod document;
pub mod model;

pub use commands::{
    dispatch, dispatch_file, BioOps, DispatchError, FileCommand, Selection, ViewerRequest,
    ViewerResponse, ViewerState,
};
pub use document::{CutSite, Document, Feature, FeatureKind, SearchHit, Strand, Topology};
pub use model::{Annotations, Buffer, BufferId, View, ViewId, ViewKind};
