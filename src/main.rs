mod bond;
mod pubkey;
mod rpc;

use anyhow::Context;
use axum::{extract::State, routing::get};
use prometheus::core::Collector;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, RwLock},
};
use tracing::info;

use crate::bond::{fetch_bond_funding, lamports_to_sol, BondFunding};
use crate::pubkey::Pubkey;
use crate::rpc::RpcClient;

const METRICS_PREFIX: &str = "marinade_bond_monitor";

#[derive(Debug, serde::Deserialize)]
pub struct Config {
    /// Bond or vote account addresses to monitor
    pub addresses: Vec<Address>,
    pub fetch_interval: std::time::Duration,
    pub rpc_url: String,
    pub listen_addr: SocketAddr,
}

#[derive(Debug, serde::Deserialize, Hash, Eq, PartialEq, Clone)]
pub struct Address {
    pub address: String,
    pub name: String,
}

fn main() -> anyhow::Result<()> {
    let subscriber = tracing_subscriber::fmt::SubscriberBuilder::default()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                // Use INFO level as default
                .add_directive(tracing::Level::INFO.into()),
        )
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("failed to initialize logger");

    let config_path = std::env::args()
        .nth(1)
        .expect("Usage: marinade-bond-monitor <config_path>");
    let config_str = std::fs::read_to_string(config_path).context("Failed to read config file")?;
    let config: Config = toml::from_str(&config_str).context("Failed to parse config file")?;

    // Validate addresses upfront so misconfigured entries fail fast.
    let parsed_addresses: Vec<(Address, Pubkey)> = config
        .addresses
        .into_iter()
        .map(|a| {
            let pk = Pubkey::from_str(&a.address)
                .with_context(|| format!("invalid address '{}' for {}", a.address, a.name))?;
            Ok((a, pk))
        })
        .collect::<anyhow::Result<_>>()?;

    let rpc = RpcClient::new(config.rpc_url).context("Failed to build RPC client")?;

    let bonds_state = Arc::new(RwLock::new(BondsState {
        bond_by_addr: HashMap::new(),
    }));
    let api_context = Arc::new(ApiContext::new(bonds_state.clone()));

    let fetch_interval = config.fetch_interval;
    let monitor_handle = std::thread::spawn(move || {
        monitor_bonds(parsed_addresses, fetch_interval, rpc, bonds_state);
    });

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("Failed to create Tokio runtime")?
        .block_on(run_server(api_context, config.listen_addr))
        .context("Failed to run server")?;

    monitor_handle
        .join()
        .expect("Failed to join monitor thread");

    Ok(())
}

pub struct ApiContext {
    bonds_state: Arc<RwLock<BondsState>>,
    bond_value_active_gauge: prometheus::GaugeVec,
    metrics_encoder: prometheus::TextEncoder,
}

impl ApiContext {
    pub fn new(bonds_state: Arc<RwLock<BondsState>>) -> Self {
        let bond_value_active_gauge = prometheus::GaugeVec::new(
            prometheus::Opts::new(
                format!("{}_bond_value_active_sol", METRICS_PREFIX),
                "Active bond value in SOL",
            ),
            &["name", "address", "vote_account", "bond_account"],
        )
        .expect("creating valid metric should not fail");

        Self {
            bonds_state,
            bond_value_active_gauge,
            metrics_encoder: prometheus::TextEncoder::new(),
        }
    }
}

pub async fn run_server(api_context: Arc<ApiContext>, addr: SocketAddr) -> anyhow::Result<()> {
    let app = axum::Router::new()
        .route("/metrics", get(metrics_handler))
        .with_state(api_context.clone());

    let tcp_listener = tokio::net::TcpListener::bind(&addr).await?;
    info!(addr = addr.to_string(), "Starting internal API server");
    axum::serve(tcp_listener, app).await?;

    Ok(())
}

async fn metrics_handler(
    State(api_context): State<Arc<ApiContext>>,
) -> Result<String, (axum::http::StatusCode, String)> {
    tracing::debug!("Handling metrics request");
    let bonds_state = api_context.bonds_state.read().unwrap();

    api_context.bond_value_active_gauge.reset();
    for (addr, funding) in &bonds_state.bond_by_addr {
        let active_bond_sol = lamports_to_sol(funding.amount_active_lamports);

        api_context
            .bond_value_active_gauge
            .with_label_values(&[
                &addr.name,
                &addr.address,
                &funding.vote_account.to_base58(),
                &funding.bond_account.to_base58(),
            ])
            .set(active_bond_sol);
    }

    let metrics = api_context
        .metrics_encoder
        .encode_to_string(&api_context.bond_value_active_gauge.collect())
        .map_err(|err| {
            tracing::error!("Failed to encode metrics: {}", err);
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to encode metrics".to_string(),
            )
        })?;

    Ok(metrics)
}

pub struct BondsState {
    bond_by_addr: HashMap<Address, BondFunding>,
}

fn monitor_bonds(
    addresses: Vec<(Address, Pubkey)>,
    interval: std::time::Duration,
    rpc: RpcClient,
    bonds_state: Arc<RwLock<BondsState>>,
) {
    loop {
        tracing::debug!("Retrieving bond data for {} addresses", addresses.len());
        let mut updated = 0;

        for (addr, pubkey) in &addresses {
            let result = fetch_with_retries(&rpc, pubkey, 4);
            let mut bond_state_lock = bonds_state.write().unwrap();

            match result {
                Ok(funding) => {
                    bond_state_lock.bond_by_addr.insert(addr.clone(), funding);
                    updated += 1;
                    tracing::debug!("Updated bond data for {}", addr.address);
                }
                Err(err) => {
                    tracing::error!(
                        "Failed to get bond data with max attempts for address {}: {:#}",
                        addr.address,
                        err
                    );
                    // If the bond data retrieval fails, we remove it so that metrics will be missing
                    bond_state_lock.bond_by_addr.remove(addr);
                }
            }
        }

        tracing::info!(
            "Fetched data for {} addresses. Sleeping for {:?} before next bond data retrieval",
            updated,
            interval
        );
        std::thread::sleep(interval);
    }
}

fn fetch_with_retries(
    rpc: &RpcClient,
    addr: &Pubkey,
    max_attempts: u32,
) -> anyhow::Result<BondFunding> {
    let mut attempt = 0;
    loop {
        attempt += 1;
        match fetch_bond_funding(rpc, addr) {
            Ok(funding) => return Ok(funding),
            Err(err) => {
                if attempt >= max_attempts {
                    return Err(err);
                }
                // Linear backoff (exponential would grow too fast for our 60s interval).
                let sleep_time = std::time::Duration::from_secs(1) * attempt;
                tracing::warn!(
                    "Failed to get bond data for {}: {:#}. Attempt {}/{}. Will retry after {}s...",
                    addr,
                    err,
                    attempt,
                    max_attempts,
                    sleep_time.as_secs()
                );
                std::thread::sleep(sleep_time);
            }
        }
    }
}
