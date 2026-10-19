//! Semantic diff, noise rules and 3-way merge for Unity YAML files, plus the
//! special-file mergers (DESIGN.md §12–§14).

pub mod describe;
pub mod diff;
pub mod json;
pub mod matrix;
pub mod merge;
pub mod names;
pub mod render;
pub mod restore;
mod transform;
