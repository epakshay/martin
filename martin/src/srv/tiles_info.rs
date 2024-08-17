use actix_web::error::ErrorInternalServerError;
use actix_web::web;
use crate::pg::pool::PgPool;
use std::string::ToString;

use actix_web::error::ErrorBadRequest;
use actix_web::http::Uri;
use actix_web::web::Path;
use actix_web::{middleware, route, HttpRequest, HttpResponse, Result as ActixResult};

use itertools::Itertools as _;
use serde::Deserialize;
use tilejson::{tilejson, TileJSON};

use crate::source::{Source, TileSources};
use crate::srv::SrvConfig;
use std::sync::{Arc, Mutex};
use actix_web::web::Data;

#[derive(Deserialize)]
pub struct SourceIDsRequest {
    pub source_ids: String,
}

#[route(
    "/{source_ids}",
    method = "GET",
    method = "HEAD",
    wrap = "middleware::Compress::default()"
)]
#[allow(clippy::unused_async)]
async fn get_source_info(
    req: HttpRequest,
    path: Path<SourceIDsRequest>,
    sources: Data<Arc<Mutex<TileSources>>>,
    srv_config: Data<SrvConfig>,
) -> ActixResult<HttpResponse> {
    let sources = sources.lock().unwrap();
    let sources = sources.get_sources(&path.source_ids, None)?.0;

    let tiles_path = if let Some(base_path) = &srv_config.base_path {
        format!("{base_path}/{}", path.source_ids)
    } else {
        req.headers()
            .get("x-rewrite-url")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<Uri>().ok())
            .map_or_else(|| req.path().to_string(), |v| v.path().to_string())
    };

    let query_string = req.query_string();
    let path_and_query = if query_string.is_empty() {
        format!("{tiles_path}/{{z}}/{{x}}/{{y}}")
    } else {
        format!("{tiles_path}/{{z}}/{{x}}/{{y}}?{query_string}")
    };

    // Construct a tiles URL from the request info, including the query string if present.
    let info = req.connection_info();
    let tiles_url = Uri::builder()
        .scheme(info.scheme())
        .authority(info.host())
        .path_and_query(path_and_query)
        .build()
        .map(|tiles_url| tiles_url.to_string())
        .map_err(|e| ErrorBadRequest(format!("Can't build tiles URL: {e}")))?;

    Ok(HttpResponse::Ok().json(merge_tilejson(&sources, tiles_url)))
}

#[must_use]
pub fn merge_tilejson(sources: &[&dyn Source], tiles_url: String) -> TileJSON {
    if sources.len() == 1 {
        let mut tj = sources[0].get_tilejson().clone();
        tj.tiles = vec![tiles_url];
        return tj;
    }

    let mut attributions = vec![];
    let mut descriptions = vec![];
    let mut names = vec![];
    let mut result = tilejson! {
        tiles: vec![tiles_url],
    };

    for src in sources {
        let tj = src.get_tilejson();

        if let Some(vector_layers) = &tj.vector_layers {
            if let Some(ref mut a) = result.vector_layers {
                a.extend(vector_layers.iter().cloned());
            } else {
                result.vector_layers = Some(vector_layers.clone());
            }
        }

        if let Some(v) = &tj.attribution {
            if !attributions.contains(&v) {
                attributions.push(v);
            }
        }

        if let Some(bounds) = tj.bounds {
            if let Some(a) = result.bounds {
                result.bounds = Some(a + bounds);
            } else {
                result.bounds = tj.bounds;
            }
        }

        if result.center.is_none() {
            // Use first found center. Averaging multiple centers might create a center in the middle of nowhere.
            result.center = tj.center;
        }

        if let Some(v) = &tj.description {
            if !descriptions.contains(&v) {
                descriptions.push(v);
            }
        }

        if let Some(maxzoom) = tj.maxzoom {
            if let Some(a) = result.maxzoom {
                if a < maxzoom {
                    result.maxzoom = tj.maxzoom;
                }
            } else {
                result.maxzoom = tj.maxzoom;
            }
        }

        if let Some(minzoom) = tj.minzoom {
            if let Some(a) = result.minzoom {
                if a > minzoom {
                    result.minzoom = tj.minzoom;
                }
            } else {
                result.minzoom = tj.minzoom;
            }
        }

        if let Some(name) = &tj.name {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }

    if !attributions.is_empty() {
        result.attribution = Some(attributions.into_iter().join("\n"));
    }

    if !descriptions.is_empty() {
        result.description = Some(descriptions.into_iter().join("\n"));
    }

    if !names.is_empty() {
        result.name = Some(names.into_iter().join(","));
    }

    result
}

#[derive(Deserialize)]
struct NewSourceRequest {
    schema_name: String,
    source_name: String,
}

#[route(
    "/add_source",
    method = "POST",
    method = "HEAD",
    wrap = "middleware::Compress::default()"
)]
#[allow(clippy::unused_async)]
async fn add_source(
    req: HttpRequest,
    data: web::Json<NewSourceRequest>,
    sources: Data<Arc<Mutex<TileSources>>>,
    pool: Data<PgPool>,
) -> ActixResult<HttpResponse> {
    let source_name = &data.source_name;
    let schema_name = &data.schema_name;

    // Check if the table exists in the PostgreSQL database
    let conn = pool.get().await.map_err(|e| {
        ErrorInternalServerError(format!("Error getting database connection: {}", e))
    })?;
    
    let source_exists = conn.query_opt(
        "SELECT 1 FROM information_schema.tables WHERE table_schema = $1 AND table_name = $2",
        &[&schema_name, &source_name],
    ).await.map_err(|e| {
        ErrorBadRequest(format!("Error querying database: {}", e))
    })?;

    if source_exists.is_none() {
        return Err(ErrorBadRequest(format!(
            "Source {}.{} does not exist in the database",
            schema_name, source_name
        )));
    }

    // Lock the mutex before accessing sources
    let mut sources_guard = sources.lock().unwrap();
    sources_guard.add_source(schema_name, source_name, &*pool).await.map_err(|e| {
        ErrorInternalServerError(format!("Error adding source: {}", e))
    })?;

    Ok(HttpResponse::Ok().json(format!(
        "Source {}.{} added successfully",
        schema_name, source_name
    )))
}