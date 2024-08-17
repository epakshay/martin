use crate::pg::pool::PgPool;
use crate::MartinError;
use async_trait::async_trait;
use deadpool_postgres::tokio_postgres::types::{ToSql, Type};
use log::debug;
use martin_tile_utils::Encoding::Uncompressed;
use martin_tile_utils::Format::Mvt;
use martin_tile_utils::{TileCoord, TileInfo};
use tilejson::TileJSON;
use std::collections::BTreeMap;
use serde_json::Value as JsonValue;

use crate::pg::utils::query_to_json;
use crate::pg::PgError::{GetTileError, GetTileWithQueryError, PrepareQueryError};
use crate::source::{Source, TileData, UrlQuery, TileSources};
use crate::MartinResult;

#[derive(Clone, Debug)]
pub struct PgSource {
    id: String,
    info: PgSqlInfo,
    pool: PgPool,
    tilejson: TileJSON,
}

impl PgSource {
    #[must_use]
    pub fn new(id: String, info: PgSqlInfo, tilejson: TileJSON, pool: PgPool) -> Self {
        Self {
            id,
            info,
            pool,
            tilejson,
        }
    }
}

#[async_trait]
impl Source for PgSource {
    fn get_id(&self) -> &str {
        &self.id
    }

    fn get_tilejson(&self) -> &TileJSON {
        &self.tilejson
    }

    fn get_tile_info(&self) -> TileInfo {
        TileInfo::new(Mvt, Uncompressed)
    }

    fn clone_source(&self) -> Box<dyn Source> {
        Box::new(self.clone())
    }

    fn support_url_query(&self) -> bool {
        self.info.use_url_query
    }

    async fn get_tile(
        &self,
        xyz: TileCoord,
        url_query: Option<&UrlQuery>,
    ) -> MartinResult<TileData> {
        let conn = self.pool.get().await?;
        let param_types: &[Type] = if self.support_url_query() {
            &[Type::INT2, Type::INT8, Type::INT8, Type::JSON]
        } else {
            &[Type::INT2, Type::INT8, Type::INT8]
        };

        let sql = &self.info.sql_query;
        let prep_query = conn
            .prepare_typed_cached(sql, param_types)
            .await
            .map_err(|e| {
                PrepareQueryError(
                    e,
                    self.id.to_string(),
                    self.info.signature.to_string(),
                    self.info.sql_query.to_string(),
                )
            })?;

        let tile = if self.support_url_query() {
            let json = query_to_json(url_query);
            debug!("SQL: {sql} [{xyz}, {json:?}]");
            let params: &[&(dyn ToSql + Sync)] = &[
                &i16::from(xyz.z),
                &i64::from(xyz.x),
                &i64::from(xyz.y),
                &json,
            ];
            conn.query_opt(&prep_query, params).await
        } else {
            debug!("SQL: {sql} [{xyz}]");
            conn.query_opt(
                &prep_query,
                &[&i16::from(xyz.z), &i64::from(xyz.x), &i64::from(xyz.y)],
            )
            .await
        };

        let tile = tile
            .map(|row| row.and_then(|r| r.get::<_, Option<TileData>>(0)))
            .map_err(|e| {
                if self.support_url_query() {
                    GetTileWithQueryError(e, self.id.to_string(), xyz, url_query.cloned())
                } else {
                    GetTileError(e, self.id.to_string(), xyz)
                }
            })?
            .unwrap_or_default();

        Ok(tile)
    }
}

#[derive(Clone, Debug)]
pub struct PgSqlInfo {
    pub sql_query: String,
    pub use_url_query: bool,
    pub signature: String,
}

impl PgSqlInfo {
    #[must_use]
    pub fn new(query: String, has_query_params: bool, signature: String) -> Self {
        Self {
            sql_query: query,
            use_url_query: has_query_params,
            signature,
        }
    }
}

impl TileSources {
    pub async fn add_source(
        &mut self,
        schema_name: &str,
        source_name: &str,
        pool: &PgPool,
    ) -> Result<(), MartinError> {
        let source_id = format!("{}.{}", schema_name, source_name);

        let tilejson = TileJSON {
            tilejson: "2.2.0".to_string(),
            name: Some(source_name.to_string()),
            description: Some(format!("Dynamic source added: {}.{}", schema_name, source_name)),
            version: Some("1.0.0".to_string()),
            tiles: vec![],
            grids: None,
            data: None,
            minzoom: Some(0),
            maxzoom: Some(22),
            bounds: None,
            center: None,
            attribution: None,
            template: None,
            legend: None,
            vector_layers: None,
            fillzoom: None,
            other: BTreeMap::new(),
            scheme: None,
        };

        let sql_query = format!("SELECT * FROM {}.{}", schema_name, source_name);
        let info = PgSqlInfo {
            sql_query,
            signature: "".to_string(),
            use_url_query: false,
        };

        let new_pg_source = PgSource::new(source_id, info, tilejson, pool.clone());

        // Use the new public method to insert the source
        self.insert_source(source_name.to_string(), Box::new(new_pg_source));

        Ok(())
    }
}

