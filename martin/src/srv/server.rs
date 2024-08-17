use std::string::ToString;
use std::time::Duration;

use actix_cors::Cors;
use actix_web::error::ErrorInternalServerError;
use actix_web::http::header::CACHE_CONTROL;
use actix_web::middleware::TrailingSlash;
use actix_web::web::Data;
use actix_web::{middleware, route, web, App, HttpResponse, HttpServer, Responder};
use actix_web::dev::Server; // Import Server type
use log::error;
use serde::{Deserialize, Serialize};

#[cfg(feature = "webui")]
use crate::args::WebUiMode;
use crate::config::ServerState;
use crate::source::TileCatalog;
use crate::srv::config::{SrvConfig, KEEP_ALIVE_DEFAULT, LISTEN_ADDRESSES_DEFAULT};
use crate::srv::tiles::get_tile;
use crate::srv::tiles_info::get_source_info;
use crate::MartinError::BindingError;
use crate::MartinResult;

use std::sync::Arc;
use tokio::sync::RwLock;

use crate::pg::pg_source::add_source_to_catalog;
use std::collections::HashMap;

// Define the SourceMetadata struct
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceMetadata {
    pub schema: String,
    pub table_or_function: String,
    // Add other fields as necessary
}

// Define the AddSourceInput struct
#[derive(Deserialize)]
pub struct AddSourceInput {
    pub schema: String,
    pub table_or_function: String,
}

// Define the Catalog struct with sources as a HashMap
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Catalog {
    pub tiles: TileCatalog,
    #[cfg(feature = "sprites")]
    pub sprites: crate::sprites::SpriteCatalog,
    #[cfg(feature = "fonts")]
    pub fonts: crate::fonts::FontCatalog,
    pub sources: HashMap<String, SourceMetadata>, // Add a field to store sources
}

// Implement methods for the Catalog struct
impl Catalog {
    pub fn new(state: &ServerState) -> MartinResult<Self> {
        Ok(Self {
            tiles: state.tiles.get_catalog(),
            #[cfg(feature = "sprites")]
            sprites: state.sprites.get_catalog()?,
            #[cfg(feature = "fonts")]
            fonts: state.fonts.get_catalog(),
            sources: HashMap::new(), // Initialize the sources field with an empty HashMap
        })
    }

    pub fn add_source(&mut self, metadata: SourceMetadata) {
        let key = format!("{}.{}", metadata.schema, metadata.table_or_function);
        self.sources.insert(key, metadata);
    }
}

// Map internal errors to actix_web::Error
pub fn map_internal_error<T: std::fmt::Display>(e: T) -> actix_web::Error {
    error!("{e}");
    ErrorInternalServerError(e.to_string())
}

// Define the health check endpoint
#[route("/health", method = "GET", method = "HEAD")]
#[allow(clippy::unused_async)]
async fn get_health() -> impl Responder {
    HttpResponse::Ok()
        .insert_header((CACHE_CONTROL, "no-cache"))
        .message_body("OK")
}

// Define the catalog endpoint
#[route(
    "/catalog",
    method = "GET",
    method = "HEAD",
    wrap = "middleware::Compress::default()"
)]
#[allow(clippy::unused_async)]
async fn get_catalog(catalog: Data<Catalog>) -> impl Responder {
    HttpResponse::Ok().json(catalog)
}

// Define the add source endpoint
#[route("/add_source", method = "POST")]
async fn post_add_source(
    catalog: web::Data<Arc<RwLock<Catalog>>>,
    input: web::Json<AddSourceInput>,
) -> impl Responder {
    match add_source_to_catalog(&catalog, &input).await {
        Ok(_) => HttpResponse::Ok().body("Source added"),
        Err(e) => {
            eprintln!("Error adding source: {:?}", e);
            HttpResponse::InternalServerError().body("Failed to add source")
        }
    }
}

// Configure the web service routes
pub fn router(cfg: &mut web::ServiceConfig, #[allow(unused_variables)] usr_cfg: &SrvConfig) {
    cfg.service(get_health)
        .service(get_catalog)
        .service(get_source_info)
        .service(get_tile)
        .service(post_add_source); // Add the new POST route here

    #[cfg(feature = "sprites")]
    cfg.service(crate::srv::sprites::get_sprite_json)
        .service(crate::srv::sprites::get_sprite_png);

    #[cfg(feature = "fonts")]
    cfg.service(crate::srv::fonts::get_font);

    #[cfg(not(feature = "webui"))]
    {
        cfg.service(web::resource("/").route(web::get().to(get_index_no_ui)));
    }
}

// Create a new server with Actix-web
pub fn new_server(config: SrvConfig, state: ServerState) -> MartinResult<(Server, String)> {
    let catalog = Catalog::new(&state)?;

    let keep_alive = Duration::from_secs(config.keep_alive.unwrap_or(KEEP_ALIVE_DEFAULT));
    let worker_processes = config.worker_processes.unwrap_or_else(num_cpus::get);
    let listen_addresses = config
        .listen_addresses
        .clone()
        .unwrap_or_else(|| LISTEN_ADDRESSES_DEFAULT.to_string());

    let factory = move || {
        let cors_middleware = Cors::default()
            .allow_any_origin()
            .allowed_methods(vec!["GET"]);

        let app = App::new()
            .app_data(Data::new(state.tiles.clone()))
            .app_data(Data::new(state.cache.clone()));

        #[cfg(feature = "sprites")]
        let app = app.app_data(Data::new(state.sprites.clone()));

        #[cfg(feature = "fonts")]
        let app = app.app_data(Data::new(state.fonts.clone()));

        app.app_data(Data::new(catalog.clone()))
            .app_data(Data::new(config.clone()))
            .wrap(cors_middleware)
            .wrap(middleware::NormalizePath::new(TrailingSlash::MergeOnly))
            .wrap(middleware::Logger::default())
            .configure(|c| router(c, &config))
    };

    let server = HttpServer::new(factory)
        .bind(listen_addresses.clone())
        .map_err(|e| BindingError(e, listen_addresses.clone()))?
        .keep_alive(keep_alive)
        .shutdown_timeout(0)
        .workers(worker_processes)
        .run();

    Ok((server, listen_addresses))
}

#[cfg(test)]
pub mod tests {
    use async_trait::async_trait;
    use martin_tile_utils::{Encoding, Format, TileCoord, TileInfo};
    use tilejson::TileJSON;

    use super::*;
    use crate::source::{Source, TileData};
    use crate::UrlQuery;

    #[derive(Debug, Clone)]
    pub struct TestSource {
        pub id: &'static str,
        pub tj: TileJSON,
        pub data: TileData,
    }

    #[async_trait]
    impl Source for TestSource {
        fn get_id(&self) -> &str {
            self.id
        }

        fn get_tilejson(&self) -> &TileJSON {
            &self.tj
        }

        fn get_tile_info(&self) -> TileInfo {
            TileInfo::new(Format::Mvt, Encoding::Uncompressed)
        }

        fn clone_source(&self) -> Box<dyn Source> {
            unimplemented!()
        }

        async fn get_tile(
            &self,
            _xyz: TileCoord,
            _url_query: Option<&UrlQuery>,
        ) -> MartinResult<TileData> {
            Ok(self.data.clone())
        }
    }
}
