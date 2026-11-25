use std::{net::SocketAddr, sync::Arc, time::Duration};

use segnale_server::{
    api::{AppState, router},
    config::Config,
    oidc::{HttpOidcClient, OidcClient},
    pg::PgStore,
    store::{MemoryStore, Store},
};

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("[segnale_server] fatal: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env();

    let store: Arc<dyn Store> = match &config.database_url {
        Some(url) => {
            let s = PgStore::connect(url).await?;
            eprintln!("[segnale_server] connected to Postgres, schema migrated");
            Arc::new(s)
        }
        None if config.allow_memory_store => {
            eprintln!(
                "[segnale_server] WARNING: ALLOW_MEMORY_STORE=1 -- using IN-MEMORY store; all data is lost on restart"
            );
            Arc::new(MemoryStore::new())
        }
        None => {
            return Err("DATABASE_URL is missing or blank (set ALLOW_MEMORY_STORE=1 only for local dev)".into());
        }
    };

    let oidc: Option<Arc<dyn OidcClient>> = match &config.oidc {
        Some(settings) => {
            if settings.client_secret.is_none() {
                return Err("OIDC_CLIENT_ID is set but OIDC_CLIENT_SECRET is missing or blank".into());
            }
            Some(Arc::new(HttpOidcClient::new(settings.clone())))
        }
        None => {
            eprintln!("[segnale_server] OIDC_CLIENT_ID is not set -- login disabled, leaderboards accept guests only");
            None
        }
    };

    // Hourly purge of expired sessions.
    let purge_store = store.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        loop {
            tick.tick().await;
            match purge_store.purge_expired_sessions(chrono::Utc::now()).await {
                Ok(n) if n > 0 => eprintln!("[segnale_server] purged {n} expired sessions"),
                Ok(_) => {}
                Err(e) => eprintln!("[segnale_server] session purge failed: {e}"),
            }
        }
    });

    let bind = config.bind.clone();
    let app = router(AppState::new(config, store, oidc));
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    eprintln!("[segnale_server] listening on {}", listener.local_addr()?);
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    eprintln!("[segnale_server] shut down");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
    eprintln!("[segnale_server] shutdown signal received");
}
