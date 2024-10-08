mod config;
pub use config::{SrvConfig, KEEP_ALIVE_DEFAULT, LISTEN_ADDRESSES_DEFAULT};

#[cfg(feature = "fonts")]
mod fonts;

pub mod server;
pub use server::{new_server, router, Catalog};

mod tiles;
pub use tiles::{DynTileSource, TileRequest};

mod tiles_info;
pub use tiles_info::{merge_tilejson, SourceIDsRequest};

#[cfg(feature = "sprites")]
mod sprites;
