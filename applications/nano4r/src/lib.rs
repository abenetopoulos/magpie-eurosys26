// NOTE re-exporting definitions at the crate's root is required in order for namespacing through
// nandoize to work correctly, at least for now.
pub use definitions::*;
pub use kvs::*;
pub use search::dfs::*;
pub use triangle_counting::*;
pub use utils::*;

pub mod definitions;
pub mod resolver;
pub mod search;
pub mod triangle_counting;
pub mod utils;

pub(crate) const NAMESPACE: &'static str = "nano4r";
