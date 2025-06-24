use anyhow::Context;
use axum::{extract::State, routing::get};
use prometheus::core::Collector;
use serde_json::Error as SerdeError;
use std::{
    collections::HashMap,
    net::SocketAddr,
    process::Command,
    sync::{Arc, RwLock},
};
use tracing::info;

const METRICS_PREFIX: &str = "marinade_bond_monitor";

#[derive(Debug, serde::Deserialize)]
pub struct Config {
    /// Bond or vote account addresses to monitor
    pub addresses: Vec<Address>,
    pub fetch_interval: std::time::Duration,
    pub bonds_cli_bin_path: String,
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

    let bonds_state = Arc::new(RwLock::new(BondsState {
        bond_by_addr: HashMap::new(),
    }));
    let api_context = Arc::new(ApiContext::new(bonds_state.clone()));

    let addresses = config.addresses.clone();
    let fetch_interval = config.fetch_interval;
    let bonds_cli_bin_path = config.bonds_cli_bin_path.clone();

    let monitor_handle = std::thread::spawn(move || {
        monitor_bonds(addresses, fetch_interval, &bonds_cli_bin_path, bonds_state);
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
    for (addr, bond_data) in &bonds_state.bond_by_addr {
        let active_bond_sol = match bond_data.active_amount_sol() {
            Ok(value) => value,
            Err(err) => {
                tracing::error!(
                    "Failed to parse active bond amount '{}' as SOL for {}: {}",
                    bond_data.amount_active,
                    addr.address,
                    err
                );
                // Skip this address if parsing fails
                // Metrics will be missing so it is easy to alert for this
                continue;
            }
        };

        api_context
            .bond_value_active_gauge
            .with_label_values(&[
                &addr.name,
                &addr.address,
                &bond_data.vote_account.node_pubkey,
                &bond_data.public_key,
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
    bond_by_addr: HashMap<Address, BondData>,
}

fn monitor_bonds(
    addresses: Vec<Address>,
    interval: std::time::Duration,
    cmd_path: &str,
    bonds_state: Arc<RwLock<BondsState>>,
) {
    loop {
        tracing::debug!("Retrieving bond data for {} addresses", addresses.len());
        let mut updated = 0;

        for addr in &addresses {
            let bond_data_res = get_bond_value(cmd_path, &addr.address);
            let mut bond_state_lock = bonds_state.write().unwrap();

            match bond_data_res {
                Ok(bond_data) => {
                    bond_state_lock.bond_by_addr.insert(addr.clone(), bond_data);
                    updated += 1;
                    tracing::debug!("Updated bond data for {}", addr.address);
                }
                Err(err) => {
                    tracing::error!(
                        "Failed to get bond data for address {}: {}",
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

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct BondData {
    program_id: String,
    public_key: String,
    account: Account,
    vote_account: VoteAccount,
    amount_owned: String,
    amount_active: String,
    number_active_stake_accounts: i32,
    amount_at_settlements: String,
    number_settlement_stake_accounts: i32,
    amount_to_withdraw: String,
    withdraw_request: String,
    bond_mint: String,
}

impl BondData {
    pub fn active_amount_sol(&self) -> anyhow::Result<f64> {
        // I do not know if there are any other suffixes, but not having just
        // a field with number looks terrible...
        let value = self
            .amount_active
            .strip_suffix(" SOLs")
            .context("Failed to strip ' SOLs' suffix from amount_active")?;
        value
            .parse()
            .context("Failed to parse amount_active as f64")
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct Account {
    config: String,
    vote_account: String,
    authority: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct VoteAccount {
    node_pubkey: String,
    authorized_withdrawer: String,
    commission: i32,
}

fn get_bond_value(cmd_path: &str, addr: &str) -> Result<BondData, Box<dyn std::error::Error>> {
    let output = Command::new(cmd_path)
        .args(["show-bond", addr, "--with-funding"])
        .output()?;

    if !output.status.success() {
        return Err(format!(
            "Failed to run show-bond command: stdout: {}, stderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }

    let bond_data: BondData =
        serde_json::from_slice(&output.stdout).map_err(|err: SerdeError| {
            format!(
                "Failed to unmarshal bond data: {}. Raw output: {}",
                err,
                String::from_utf8_lossy(&output.stdout)
            )
        })?;

    if bond_data.public_key != addr && bond_data.account.vote_account != addr {
        return Err(format!(
            "Bond data does not match the provided address: {}. Did something change?",
            addr
        )
        .into());
    }

    Ok(bond_data)
}
