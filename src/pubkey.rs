use std::fmt;

use curve25519_dalek::edwards::CompressedEdwardsY;
use sha2::{Digest, Sha256};

#[derive(Debug)]
pub enum PubkeyError {
    InvalidBase58(bs58::decode::Error),
    WrongLength(usize),
    NoViableBump,
}

impl fmt::Display for PubkeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBase58(e) => write!(f, "invalid base58: {e}"),
            Self::WrongLength(n) => write!(f, "expected 32 bytes, got {n}"),
            Self::NoViableBump => write!(f, "no viable PDA bump found"),
        }
    }
}

impl std::error::Error for PubkeyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidBase58(e) => Some(e),
            _ => None,
        }
    }
}

impl From<bs58::decode::Error> for PubkeyError {
    fn from(e: bs58::decode::Error) -> Self {
        Self::InvalidBase58(e)
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Hash)]
pub struct Pubkey(pub [u8; 32]);

impl Pubkey {
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn from_str(s: &str) -> Result<Self, PubkeyError> {
        let v = bs58::decode(s).into_vec()?;
        let arr: [u8; 32] = v
            .as_slice()
            .try_into()
            .map_err(|_| PubkeyError::WrongLength(v.len()))?;
        Ok(Self(arr))
    }

    pub fn to_base58(&self) -> String {
        bs58::encode(self.0).into_string()
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for Pubkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_base58())
    }
}

impl fmt::Debug for Pubkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Pubkey({})", self.to_base58())
    }
}

const PDA_MARKER: &[u8] = b"ProgramDerivedAddress";

/// Solana PDA derivation. Replicates `Pubkey::find_program_address`:
/// scan bumps from 255 down to 0; the first hash that is *not* a valid
/// Ed25519 curve point is the canonical PDA.
pub fn find_program_address(
    seeds: &[&[u8]],
    program_id: &Pubkey,
) -> Result<(Pubkey, u8), PubkeyError> {
    for bump in (0u8..=255).rev() {
        let mut hasher = Sha256::new();
        for seed in seeds {
            hasher.update(seed);
        }
        hasher.update([bump]);
        hasher.update(program_id.as_bytes());
        hasher.update(PDA_MARKER);
        let hash: [u8; 32] = hasher.finalize().into();

        if !is_on_curve(&hash) {
            return Ok((Pubkey(hash), bump));
        }
    }
    Err(PubkeyError::NoViableBump)
}

fn is_on_curve(bytes: &[u8; 32]) -> bool {
    CompressedEdwardsY(*bytes).decompress().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Known fixture: derive the bond account PDA for the institutional config
    // and the Chorus One main vote account, and check it matches the address
    // recorded in config.toml.local.
    #[test]
    fn bond_account_pda_matches_known_value() {
        let program_id =
            Pubkey::from_str("vBoNdEvzMrSai7is21XgVYik65mqtaKXuSdMBJ1xkW4").unwrap();
        let config = Pubkey::from_str("VbinSTyUEC8JXtzFteC4ruKSfs6dkQUUcY6wB1oJyjE").unwrap();
        let vote = Pubkey::from_str("Chorus6Kis8tFHA7AowrPMcRJk3LbApHTYpgSNXzY5KE").unwrap();

        let (pda, _bump) = find_program_address(
            &[b"bond_account", config.as_bytes(), vote.as_bytes()],
            &program_id,
        )
        .unwrap();

        assert_eq!(
            pda.to_base58(),
            "3ZSAa4xX1b8zc2kbDGrbWN9RACYJB5LMmkgzKqKrZ5p3"
        );
    }
}
