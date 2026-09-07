//! PDA seeds for stable-yield pools.

use anchor_lang::prelude::*;

pub const POOL: &[u8] = b"sypool";
pub const VAULT: &[u8] = b"syvault";
pub const SHARE_MINT: &[u8] = b"syshare";

/// `[QUEUE, pool]` — the withdrawal ring, in its own account.
///
/// Separate from the pool because of a limit that exists on no other chain Piron runs on:
/// an account created through a CPI caps at 10,240 bytes, and 256 requests need 20,944.
/// So the queue is created at the cap and grown with two reallocs, and keeping it out of
/// the pool account also stops every NAV read loading 20KB it does not need.
pub const QUEUE: &[u8] = b"syqueue";

/// `[HOLDER, pool, owner]` — when this holder last bought in.
///
/// The pool carries a holding period, and enforcing it needs a per-holder clock. Solana
/// has no mapping to hang it off, so it is its own small account.
pub const HOLDER: &[u8] = b"syholder";

pub fn holder(pool: &Pubkey, owner: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[HOLDER, pool.as_ref(), owner.as_ref()], &crate::ID)
}

pub fn pool(pool_id: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[POOL, &pool_id.to_le_bytes()], &crate::ID)
}
pub fn vault(pool: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[VAULT, pool.as_ref()], &crate::ID)
}
pub fn share_mint(pool: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SHARE_MINT, pool.as_ref()], &crate::ID)
}
pub fn queue(pool: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[QUEUE, pool.as_ref()], &crate::ID)
}
