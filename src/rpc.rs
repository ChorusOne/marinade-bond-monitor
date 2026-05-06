use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::pubkey::Pubkey;

#[derive(Debug, Clone)]
pub struct RpcClient {
    url: String,
    http: reqwest::blocking::Client,
}

impl RpcClient {
    pub fn new(url: String) -> anyhow::Result<Self> {
        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .context("failed to build HTTP client")?;
        Ok(Self { url, http })
    }

    fn call<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> anyhow::Result<T> {
        let body = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });

        let resp = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .with_context(|| format!("RPC request failed: {method}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().unwrap_or_default();
            bail!("RPC HTTP error {status}: {body}");
        }

        let envelope: RpcEnvelope<T> = resp
            .json()
            .with_context(|| format!("failed to decode RPC response for {method}"))?;

        if let Some(err) = envelope.error {
            bail!("RPC error from {method}: code={} msg={}", err.code, err.message);
        }
        envelope
            .result
            .ok_or_else(|| anyhow!("RPC response for {method} had no result"))
    }

    pub fn get_account(&self, addr: &Pubkey) -> anyhow::Result<Option<RpcAccount>> {
        let resp: RpcResponse<Option<RpcAccountRaw>> = self.call(
            "getAccountInfo",
            json!([
                addr.to_base58(),
                { "encoding": "base64", "commitment": "confirmed" }
            ]),
        )?;
        resp.value.map(decode_account).transpose()
    }

    /// Stake program accounts whose `withdrawer` (offset 44) and `voter`
    /// (offset 124) match the given values. Returns base64-decoded data.
    pub fn get_bond_stake_accounts(
        &self,
        stake_program: &Pubkey,
        withdrawer_authority: &Pubkey,
        vote_account: &Pubkey,
    ) -> anyhow::Result<Vec<RpcAccount>> {
        let raw: Vec<ProgramAccount> = self.call(
            "getProgramAccounts",
            json!([
                stake_program.to_base58(),
                {
                    "encoding": "base64",
                    "commitment": "confirmed",
                    "filters": [
                        { "memcmp": { "offset": 44, "bytes": withdrawer_authority.to_base58() } },
                        { "memcmp": { "offset": 124, "bytes": vote_account.to_base58() } }
                    ]
                }
            ]),
        )?;

        raw.into_iter()
            .map(|pa| decode_account(pa.account))
            .collect()
    }

    pub fn get_epoch_info(&self) -> anyhow::Result<EpochInfo> {
        self.call("getEpochInfo", json!([{ "commitment": "confirmed" }]))
    }
}

#[derive(Deserialize)]
struct RpcEnvelope<T> {
    result: Option<T>,
    #[serde(default)]
    error: Option<RpcErrorBody>,
}

#[derive(Deserialize, Debug)]
struct RpcErrorBody {
    code: i64,
    message: String,
}

#[derive(Deserialize)]
struct RpcResponse<T> {
    value: T,
}

#[derive(Deserialize)]
struct ProgramAccount {
    account: RpcAccountRaw,
}

#[derive(Deserialize)]
struct RpcAccountRaw {
    /// `[base64_data, "base64"]`
    data: (String, String),
    lamports: u64,
    owner: String,
}

pub struct RpcAccount {
    pub data: Vec<u8>,
    pub lamports: u64,
    pub owner: Pubkey,
}

fn decode_account(raw: RpcAccountRaw) -> anyhow::Result<RpcAccount> {
    if raw.data.1 != "base64" {
        return Err(anyhow!("unexpected account data encoding: {}", raw.data.1));
    }
    let data = base64::engine::general_purpose::STANDARD
        .decode(&raw.data.0)
        .context("failed to base64-decode account data")?;
    let owner = Pubkey::from_str(&raw.owner)
        .map_err(|e| anyhow!("invalid owner pubkey '{}': {e}", raw.owner))?;
    Ok(RpcAccount {
        data,
        lamports: raw.lamports,
        owner,
    })
}

#[derive(Deserialize, Debug, Clone, Serialize)]
pub struct EpochInfo {
    pub epoch: u64,
    #[serde(rename = "absoluteSlot")]
    pub absolute_slot: u64,
}
