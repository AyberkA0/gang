//! Lossless parser and writer for Unity's YAML format: scenes, prefabs and
//! other text-serialized assets (DESIGN.md §11).
//!
//! Unity's YAML is a small, regular subset of YAML 1.1 with its own document
//! headers (`--- !u!<classID> &<fileID>`). Generic YAML libraries cannot
//! reproduce it byte for byte, so this crate parses it into a tree where every
//! entry keeps the exact text it was read from. Writing a tree back gives the
//! original bytes; only entries that were changed are formatted anew, the way
//! Unity formats them.

mod guids;
pub mod hierarchy;
mod model;
mod parse;
mod scalar;

pub use guids::GuidIndex;
pub use hierarchy::{Hierarchy, Object, ObjectKind};
pub use model::{Doc, Entry, File, FileId, Item, Map, ObjRef, Value};
pub use parse::{ParseError, parse};
pub use scalar::Scalar;
