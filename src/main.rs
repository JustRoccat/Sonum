mod art;
mod config;
mod duplicates;
mod events;
mod groups;
mod handlers;
mod lyrics;
mod metrics;
mod ratelimit;
mod scan;
mod state;
mod tags;
mod transcode;
mod tree;
mod util;

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, atomic::AtomicU64},
    time::{Duration, Instant, SystemTime},
};

use axum::{
    Router,
    routing::{get, post},
};
use axum_server::tls_rustls::RustlsConfig;
use tokio::sync::{RwLock, broadcast};
use tower_http::{
    catch_panic::CatchPanicLayer, compression::CompressionLayer, cors::CorsLayer,
    services::ServeDir, trace::TraceLayer,
};

use crate::ratelimit::RateLimiter;
use crate::state::{AppState, ScanMeta, compute_music_dir_labels};

const EVENTS_CHANNEL_CAPACITY: usize = 256;

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
    for dir in &config.music_dirs {
        tracing::info!("Music dir: {}", dir.display());
    }

    if config.api_token.is_none() {
        tracing::warn!(
            "api_token not set in {} - server is running without auth. \
             Fine for localhost, do NOT expose this to a network/the internet like that.",
            config.conf_path.display()
        );
    }

    let (tracks, scan_duration) = scan::timed_scan(&config.music_dirs);
    tracing::info!(
        "Indexed {} tracks from {} music dir(s) (in {} ms)",
        tracks.len(),
        config.music_dirs.len(),
        scan_duration.as_millis()
    );

    let ffmpeg_available = transcode::detect_ffmpeg();
    if ffmpeg_available {
        tracing::info!("ffmpeg found - on-the-fly transcoding is available");
    } else {
        tracing::warn!(
            "ffmpeg not found on PATH - /tracks/:id/stream?format=... will return 501 Not Implemented"
        );
    }

    if config.rate_limit_per_min == 0 {
        tracing::warn!("rate_limit_per_min = 0 - rate limiting is disabled");
    } else {
        tracing::info!(
            "Rate limiting: {} requests/min per IP",
            config.rate_limit_per_min
        );
    }

    let (events_tx, _) = broadcast::channel(EVENTS_CHANNEL_CAPACITY);
    let music_dir_labels = compute_music_dir_labels(&config.music_dirs);

    let state = Arc::new(AppState {
        tracks: RwLock::new(tracks),
        music_dirs: config.music_dirs.clone(),
        music_dir_labels,
        api_token: config.api_token.clone(),
        start_time: Instant::now(),
        scan_meta: RwLock::new(ScanMeta {
            last_scan: SystemTime::now(),
            duration_ms: scan_duration.as_millis() as u64,
        }),
        request_count: AtomicU64::new(0),
        lyrics_cache: RwLock::new(HashMap::new()),
        art_cache: RwLock::new(HashMap::new()),
        rate_limiter: RateLimiter::new(config.rate_limit_per_min, Duration::from_secs(60)),
        ffmpeg_available,
        events: events_tx,
    });

    let _watcher = scan::spawn_music_dir_watcher(state.clone())?;
    spawn_rate_limiter_pruner(state.clone());

    let mut app = Router::new()
        .route("/tracks", get(handlers::list_tracks))
        .route(
            "/tracks/:id",
            get(handlers::get_track).patch(tags::patch_track_tags),
        )
        .route("/tracks/:id/stream", get(handlers::stream_track))
        .route("/tracks/:id/lyrics", get(lyrics::get_lyrics))
        .route("/tracks/:id/art", get(art::get_art))
        .route("/art/:id", get(art::get_art))
        .route("/albums", get(groups::list_albums))
        .route("/artists", get(groups::list_artists))
        .route("/duplicates", get(duplicates::list_duplicates))
        .route("/tree", get(tree::get_tree))
        .route("/events", get(events::stream_events))
        .route("/rescan", post(handlers::rescan))
        .route("/health", get(handlers::health))
        .route("/metrics", get(metrics::metrics));

    for (index, dir) in config.music_dirs.iter().enumerate() {
        app = app.nest_service(&format!("/files/{index}"), ServeDir::new(dir.clone()));
    }

    if let [only_dir] = config.music_dirs.as_slice() {
        app = app.nest_service("/files", ServeDir::new(only_dir.clone()));
    }

    let app = app
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            handlers::auth_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            handlers::rate_limit_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            handlers::count_requests_middleware,
        ))
        .layer(CompressionLayer::new())
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .layer(CatchPanicLayer::new())
        .with_state(state)
        .into_make_service_with_connect_info::<SocketAddr>();

    let handle = axum_server::Handle::new();
    {
        let handle = handle.clone();
        tokio::spawn(async move {
            shutdown_signal().await;
            handle.graceful_shutdown(Some(Duration::from_secs(10)));
        });
    }

    match (&config.tls_cert_path, &config.tls_key_path) {
        (Some(cert), Some(key)) => {
            let tls_config = RustlsConfig::from_pem_file(cert, key).await.map_err(|e| {
                anyhow::anyhow!(
                    "couldn't load TLS cert/key ({} / {}): {e}",
                    cert.display(),
                    key.display()
                )
            })?;
            tracing::info!("Listening on https://{}", config.bind_addr);
            axum_server::bind_rustls(config.bind_addr, tls_config)
                .handle(handle)
                .serve(app)
                .await?;
        }
        _ => {
            tracing::info!("Listening on http://{}", config.bind_addr);
            axum_server::bind(config.bind_addr)
                .handle(handle)
                .serve(app)
                .await?;
        }
    }

    tracing::info!("Server stopped.");
    Ok(())
}

fn spawn_rate_limiter_pruner(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(300));
        loop {
            interval.tick().await;
            state.rate_limiter.prune_stale();
        }
    });
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
