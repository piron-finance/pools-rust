//! PDA seeds for locked pools, in one place because two languages derive from them.

use anchor_lang::prelude::*;

/// `[POOL, pool_id]` — tiers, metrics, and the authority over the vault.
pub const POOL: &[u8] = b"lpool";

/// `[POSITION, pool, position_id]` — one account per deposit.
///
/// A locked position is not fungible: the payout is fixed per position at deposit, so
/// there is no share token here and nothing to mint. That is the structural difference
/// from the single-asset and stable-yield pools, and it is why positions are keyed by a
/// counter rather than held as a balance.
pub const POSITION: &[u8] = b"lposition";

/// `[VAULT, pool]` — the token account holding principal and returned capital.
pub const VAULT: &[u8] = b"lvault";

pub fn pool(pool_id: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[POOL, &pool_id.to_le_bytes()], &crate::ID)
}

pub fn position(pool: &Pubkey, position_id: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[POSITION, pool.as_ref(), &position_id.to_le_bytes()],
        &crate::ID,
    )
}

pub fn vault(pool: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[VAULT, pool.as_ref()], &crate::ID)
}
