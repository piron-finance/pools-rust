//! PDA seeds, in one place because two languages derive from them.
//!
//! On EVM an address is handed to you. Here it is computed, and the backend has to compute
//! the same one to build a transaction — every instruction carries the accounts it touches,
//! so a seed that drifts between the Rust and the TypeScript produces an address the
//! program rejects, at runtime, against a real cluster.
//!
//! Anything deriving a Piron address reads these. Nothing re-spells them.

use anchor_lang::prelude::*;

/// `[POOL, pool_id]` — pool terms and running state, and the authority over everything
/// else the pool owns.
pub const POOL: &[u8] = b"pool";

/// `[POSITION, pool, owner]` — per-holder accounting that shares alone cannot carry:
/// when they entered, what coupons they have taken.
pub const POSITION: &[u8] = b"position";

/// `[VAULT, pool]` — the token account holding the asset. The Soroban escrow contract,
/// except a Solana program cannot hold tokens directly; it owns a token account.
pub const VAULT: &[u8] = b"vault";

/// `[SHARE_MINT, pool]` — the share token. A plain SPL mint whose authority is the pool.
pub const SHARE_MINT: &[u8] = b"share_mint";

pub fn pool(pool_id: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[POOL, &pool_id.to_le_bytes()], &crate::ID)
}

pub fn position(pool: &Pubkey, owner: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[POSITION, pool.as_ref(), owner.as_ref()], &crate::ID)
}

pub fn vault(pool: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[VAULT, pool.as_ref()], &crate::ID)
}

pub fn share_mint(pool: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SHARE_MINT, pool.as_ref()], &crate::ID)
}
