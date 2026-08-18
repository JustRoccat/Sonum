mod art;
mod config;
mod handlers;
mod lyrics;
mod metrics;
mod scan;
mod state;
mod tree;
mod util;

use std::{
    collections::HashMap,
    sync::{atomic::AtomicU64, Arc},
    time::{Instant, SystemTime},
};

use axum::{
    routing::{get, post},
    Router,
};
use tokio::sync::RwLock;
use tower_http::{
    catch_panic::CatchPanicLayer, compression::CompressionLayer, cors::CorsLayer,
    services::ServeDir, trace::TraceLayer,
};

use crate::state::{AppState, ScanMeta};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "sonum=info,tower_http=info".into()),
        )
        .init();

    let config = config::load_or_create_config()?;
    tracing::info!("Config dir: {}", config.config_dir.display());
    tracing::info!("Config file: {}", config.conf_path.display());
    tracing::info!("Music dir: {}", config.music_dir.display());

    if config.api_token.is_none() {
        tracing::warn!(
            "api_token not set in {} - server is running without auth. \
             Fine for localhost, do NOT expose this to a network/the internet like that.",
            config.conf_path.display()
        );
    }

    let (tracks, scan_duration) = scan::timed_scan(&config.music_dir);
    tracing::info!(
        "Indexed {} tracks from {} (in {} ms)",
        tracks.len(),
        config.music_dir.display(),
        scan_duration.as_millis()
    );

    let state = Arc::new(AppState {
        tracks: RwLock::new(tracks),
        music_dir: config.music_dir.clone(),
        api_token: config.api_token.clone(),
        start_time: Instant::now(),
        scan_meta: RwLock::new(ScanMeta {
            last_scan: SystemTime::now(),
            duration_ms: scan_duration.as_millis() as u64,
        }),
        request_count: AtomicU64::new(0),
        lyrics_cache: RwLock::new(HashMap::new()),
        art_cache: RwLock::new(HashMap::new()),
    });

    let _watcher = scan::spawn_music_dir_watcher(state.clone())?;

    let app = Router::new()
        .route("/tracks", get(handlers::list_tracks))
        .route("/tracks/:id", get(handlers::get_track))
        .route("/tracks/:id/stream", get(handlers::stream_track))
        .route("/tracks/:id/lyrics", get(lyrics::get_lyrics))
        .route("/tracks/:id/art", get(art::get_art))
        .route("/art/:id", get(art::get_art))
        .route("/tree", get(tree::get_tree))
        .route("/rescan", post(handlers::rescan))
        .route("/health", get(handlers::health))
        .route("/metrics", get(metrics::metrics))
        .nest_service("/files", ServeDir::new(config.music_dir.clone()))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            handlers::auth_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            handlers::count_requests_middleware,
        ))
        .layer(CompressionLayer::new())
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .layer(CatchPanicLayer::new())
        .with_state(state);

    tracing::info!("Listening on http://{}", config.bind_addr);
    let listener = tokio::net::TcpListener::bind(config.bind_addr).await?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("Server stopped.");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("couldn't install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("couldn't install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("Got Ctrl+C, shutting down..."),
        _ = terminate => tracing::info!("Got SIGTERM, shutting down..."),
    }
}
