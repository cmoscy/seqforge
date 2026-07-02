//! The concrete render tracks (T2). Position-owned tracks — [`ruler`],
//! [`cut_sites`], [`translation`] — are migrated to the [`Track`](super::track::Track)
//! trait; [`sequence`] and [`features`] delegate to legacy core paint until
//! T3/T4. Each track owns its block height, paint, and hit rects from one
//! geometry (co-location invariant).

pub(crate) mod cut_sites;
pub(crate) mod features;
pub(crate) mod ruler;
pub(crate) mod sequence;
pub(crate) mod translation;
