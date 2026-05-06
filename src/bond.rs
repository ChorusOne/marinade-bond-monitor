use std::sync::OnceLock;
use std::time::SystemTime;

use anyhow::{anyhow, bail, Context};

use crate::pubkey::{find_program_address, Pubkey};
use crate::rpc::{EpochInfo, RpcClient};

const VALIDATOR_BONDS_PROGRAM_ID_STR: &str = "vBoNdEvzMrSai7is21XgVYik65mqtaKXuSdMBJ1xkW4";
const MARINADE_INSTITUTIONAL_CONFIG_STR: &str = "VbinSTyUEC8JXtzFteC4ruKSfs6dkQUUcY6wB1oJyjE";
const STAKE_PROGRAM_ID_STR: &str = "Stake11111111111111111111111111111111111111";

fn validator_bonds_program_id() -> &'static Pubkey {
    static CELL: OnceLock<Pubkey> = OnceLock::new();
    CELL.get_or_init(|| Pubkey::from_str(VALIDATOR_BONDS_PROGRAM_ID_STR).unwrap())
}

fn marinade_institutional_config() -> &'static Pubkey {
    static CELL: OnceLock<Pubkey> = OnceLock::new();
    CELL.get_or_init(|| Pubkey::from_str(MARINADE_INSTITUTIONAL_CONFIG_STR).unwrap())
}

fn stake_program_id() -> &'static Pubkey {
    static CELL: OnceLock<Pubkey> = OnceLock::new();
    CELL.get_or_init(|| Pubkey::from_str(STAKE_PROGRAM_ID_STR).unwrap())
}

// Anchor account discriminators (first 8 bytes of each Anchor account).
const BOND_DISCRIMINATOR: [u8; 8] = [224, 128, 48, 251, 182, 246, 111, 196];
const WITHDRAW_REQUEST_DISCRIMINATOR: [u8; 8] = [186, 239, 174, 191, 189, 13, 47, 196];

// Stake account layout offsets (post the 4-byte enum discriminator).
const STAKE_STAKER_OFFSET: usize = 12;
const STAKE_WITHDRAWER_OFFSET: usize = 44;
const STAKE_LOCKUP_UNIX_TS_OFFSET: usize = 76;
const STAKE_LOCKUP_EPOCH_OFFSET: usize = 84;
const STAKE_LOCKUP_CUSTODIAN_OFFSET: usize = 92;

#[derive(Debug, Clone)]
pub struct BondFunding {
    pub bond_account: Pubkey,
    pub vote_account: Pubkey,
    pub amount_active_lamports: u64,
}

/// Replicates `validator-bonds-institutional show-bond <addr> --with-funding`.
/// Accepts either a bond account address or a vote account address.
pub fn fetch_bond_funding(rpc: &RpcClient, input: &Pubkey) -> anyhow::Result<BondFunding> {
    let (bond_account, vote_account) = resolve_bond_and_vote(rpc, input)?;

    let withdrawer_authority = derive_bonds_withdrawer_authority()?;
    let stake_accounts =
        rpc.get_bond_stake_accounts(stake_program_id(), &withdrawer_authority, &vote_account)?;

    let epoch_info = rpc.get_epoch_info()?;
    let now_unix = unix_now()?;

    let mut amount_funded_at_bond: u64 = 0;
    for acct in &stake_accounts {
        if is_locked_up(&acct.data, &epoch_info, now_unix)? {
            continue;
        }
        let staker = read_pubkey(&acct.data, STAKE_STAKER_OFFSET)?;
        let withdrawer = read_pubkey(&acct.data, STAKE_WITHDRAWER_OFFSET)?;
        // staker == withdrawer => bond-funded; otherwise the stake is "at a settlement".
        if staker == withdrawer {
            amount_funded_at_bond = amount_funded_at_bond
                .checked_add(acct.lamports)
                .context("overflow summing bond stake lamports")?;
        }
    }

    let pending_withdrawal = fetch_pending_withdrawal(rpc, &bond_account)?;
    let amount_active = amount_funded_at_bond.saturating_sub(pending_withdrawal);

    Ok(BondFunding {
        bond_account,
        vote_account,
        amount_active_lamports: amount_active,
    })
}

/// If `input` is a Bond account, read its vote_account from the data.
/// Otherwise treat `input` as a vote account and derive the Bond PDA.
fn resolve_bond_and_vote(rpc: &RpcClient, input: &Pubkey) -> anyhow::Result<(Pubkey, Pubkey)> {
    if let Some(acct) = rpc.get_account(input)? {
        if &acct.owner == validator_bonds_program_id() {
            let vote = parse_bond_vote_account(&acct.data)
                .context("input account is owned by validator-bonds but not a Bond")?;
            return Ok((*input, vote));
        }
    }

    let (bond, _bump) = find_program_address(
        &[
            b"bond_account",
            marinade_institutional_config().as_bytes(),
            input.as_bytes(),
        ],
        validator_bonds_program_id(),
    )
    .map_err(|e| anyhow!("failed to derive bond PDA: {e}"))?;

    Ok((bond, *input))
}

fn fetch_pending_withdrawal(rpc: &RpcClient, bond: &Pubkey) -> anyhow::Result<u64> {
    let (wr_pda, _bump) = find_program_address(
        &[b"withdraw_account", bond.as_bytes()],
        validator_bonds_program_id(),
    )
    .map_err(|e| anyhow!("failed to derive withdraw_request PDA: {e}"))?;

    let Some(acct) = rpc.get_account(&wr_pda)? else {
        return Ok(0);
    };
    if &acct.owner != validator_bonds_program_id() {
        bail!(
            "withdraw_request {wr_pda} owned by unexpected program {}",
            acct.owner
        );
    }

    if acct.data.len() < 8 || acct.data[..8] != WITHDRAW_REQUEST_DISCRIMINATOR {
        bail!("withdraw_request {wr_pda} has wrong discriminator");
    }
    // After the 8-byte Anchor discriminator:
    //   vote_account: 32 (offset 8)
    //   bond:         32 (offset 40)
    //   epoch:        u64 (offset 72)
    //   requested_amount: u64 (offset 80)
    //   withdrawn_amount: u64 (offset 88)
    let requested = read_u64(&acct.data, 80)?;
    let withdrawn = read_u64(&acct.data, 88)?;
    Ok(requested.saturating_sub(withdrawn))
}

fn parse_bond_vote_account(data: &[u8]) -> anyhow::Result<Pubkey> {
    if data.len() < 8 + 32 + 32 || data[..8] != BOND_DISCRIMINATOR {
        bail!("not a Bond account (bad discriminator or too short)");
    }
    // After the 8-byte Anchor discriminator: config (32), vote_account (32), authority (32), ...
    read_pubkey(data, 8 + 32)
}

fn derive_bonds_withdrawer_authority() -> anyhow::Result<Pubkey> {
    let (pda, _bump) = find_program_address(
        &[
            b"bonds_authority",
            marinade_institutional_config().as_bytes(),
        ],
        validator_bonds_program_id(),
    )
    .map_err(|e| anyhow!("failed to derive bonds_withdrawer_authority: {e}"))?;
    Ok(pda)
}

fn is_locked_up(data: &[u8], epoch_info: &EpochInfo, now_unix: i64) -> anyhow::Result<bool> {
    if data.len() < STAKE_LOCKUP_CUSTODIAN_OFFSET + 32 {
        // Not a fully-initialized stake account; treat as not locked.
        return Ok(false);
    }
    let custodian = read_pubkey(data, STAKE_LOCKUP_CUSTODIAN_OFFSET)?;
    if custodian == Pubkey::new([0u8; 32]) {
        return Ok(false);
    }
    let unix_ts = read_i64(data, STAKE_LOCKUP_UNIX_TS_OFFSET)?;
    let epoch = read_u64(data, STAKE_LOCKUP_EPOCH_OFFSET)?;
    Ok(epoch > epoch_info.epoch || unix_ts > now_unix)
}

fn read_pubkey(data: &[u8], offset: usize) -> anyhow::Result<Pubkey> {
    let end = offset + 32;
    let slice = data
        .get(offset..end)
        .ok_or_else(|| anyhow!("data too short to read pubkey at offset {offset}"))?;
    let arr: [u8; 32] = slice.try_into().expect("slice len is 32");
    Ok(Pubkey::new(arr))
}

fn read_u64(data: &[u8], offset: usize) -> anyhow::Result<u64> {
    let end = offset + 8;
    let slice = data
        .get(offset..end)
        .ok_or_else(|| anyhow!("data too short to read u64 at offset {offset}"))?;
    Ok(u64::from_le_bytes(slice.try_into().expect("slice len 8")))
}

fn read_i64(data: &[u8], offset: usize) -> anyhow::Result<i64> {
    let end = offset + 8;
    let slice = data
        .get(offset..end)
        .ok_or_else(|| anyhow!("data too short to read i64 at offset {offset}"))?;
    Ok(i64::from_le_bytes(slice.try_into().expect("slice len 8")))
}

fn unix_now() -> anyhow::Result<i64> {
    let dur = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .context("system clock before UNIX epoch")?;
    Ok(dur.as_secs() as i64)
}

pub fn lamports_to_sol(lamports: u64) -> f64 {
    lamports as f64 / 1_000_000_000f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_constants_decode_base58() {
        // Just exercise the OnceLocks; from_str panics here if any literal is wrong.
        assert_eq!(
            validator_bonds_program_id().to_base58(),
            VALIDATOR_BONDS_PROGRAM_ID_STR
        );
        assert_eq!(
            marinade_institutional_config().to_base58(),
            MARINADE_INSTITUTIONAL_CONFIG_STR
        );
        assert_eq!(stake_program_id().to_base58(), STAKE_PROGRAM_ID_STR);
    }
}
