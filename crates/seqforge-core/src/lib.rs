// seqforge-core: data model + typed command surface, no GUI deps.
//
//  - `document`  — legacy Document/Feature types still used by the
//                   BioOps::load adapter (`seqforge-bio` returns
//                   `Document`; the app shells it into a Buffer +
//                   Annotations).
//  - `model`     — editor-ready types: Buffer, Annotations, View,
//                   ViewKind plus id newtypes. The canonical state
//                   shape after Stage 2.5a.
//  - `commands`  — ViewerRequest, ViewerResponse, FileCommand,
//                   dispatch(), DispatchError.

pub mod commands;
pub mod document;
pub mod model;

pub use commands::{
    BioOps, DispatchError, EnzymeOp, FileCommand, Selection, ViewerRequest, ViewerResponse,
    dispatch, dispatch_file,
};
pub use document::{CutSite, Document, Feature, FeatureKind, SearchHit, Strand, Topology};
pub use model::{Annotations, Buffer, BufferId, View, ViewId, ViewKind};
