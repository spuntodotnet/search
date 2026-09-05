use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ferrite::api::{router, AppState};
use ferrite::engine::Catalog;

/// Intervalle de rafraichissement de fond, equivalent du
/// `index.refresh_interval` d'ES : les ecritures sans `refresh=true` deviennent
/// visibles au plus tard apres ce delai.
const REFRESH_INTERVAL: Duration = Duration::from_secs(1);

/// A quelle frequence les contextes de `scroll` et de `pit` expires sont
/// oublies.
const PURGE_SCROLL_INTERVAL: Duration = Duration::from_secs(30);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind: SocketAddr = env_or("FERRITE_BIND", "0.0.0.0:9200").parse()?;
    let data_dir = PathBuf::from(env_or("FERRITE_DATA", "./data"));
    let cluster_name = env_or("FERRITE_CLUSTER_NAME", "ferrite");
    let node_name = env_or("FERRITE_NODE_NAME", "ferrite-0");

    let catalog = Catalog::open(data_dir.clone(), cluster_name, node_name)?;
    let scrolls = Arc::new(ferrite::scroll::Registre::default());
    let pits = Arc::new(ferrite::pit::Registre::default());
    let state = Arc::new(AppState {
        catalog: catalog.clone(),
        started: Instant::now(),
        scrolls: scrolls.clone(),
        pits: pits.clone(),
    });

    {
        let catalog = catalog.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(REFRESH_INTERVAL);
            loop {
                ticker.tick().await;
                let catalog = catalog.clone();
                let _ = tokio::task::spawn_blocking(move || catalog.refresh_dirty()).await;
            }
        });
    }

    {
        // Un contexte de `scroll` ou de `pit` retient un instantane de
        // l'index : un client qui disparait sans appeler
        // `DELETE /_search/scroll` ou `DELETE /_pit` ne doit pas pouvoir en
        // garder un ouvert pour toujours.
        let scrolls = scrolls.clone();
        let pits = pits.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(PURGE_SCROLL_INTERVAL);
            loop {
                ticker.tick().await;
                scrolls.purger();
                pits.purger();
            }
        });
    }

    let listener = tokio::net::TcpListener::bind(bind).await?;
    println!(
        "ferrite {} — API Elasticsearch {} — ecoute sur http://{bind} — donnees dans {}",
        ferrite::FERRITE_VERSION,
        ferrite::ES_VERSION,
        data_dir.display()
    );

    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown())
        .await?;

    // Derniere chance de rendre durables les ecritures en attente.
    tokio::task::spawn_blocking(move || catalog.refresh_dirty()).await?;
    Ok(())
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}
