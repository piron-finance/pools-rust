//! Stable Yield pool — Solana binding layer.
//!
//! The rolling pool: open-ended, holders enter and leave at NAV, and the SPV invests on
//! its own schedule. Every protocol decision comes from `piron-core`.
//!
//! Three things this pool needs that the others do not, and what each costs here:
//!
//! - **NAV is stored, not computed on read.** Solana has no view functions, so `total_nav`
//!   is a field written after every mutation. A client recomputing it would eventually
//!   disagree with what the program believed when it moved money.
//! - **Capital in flight counts toward NAV.** Cash drawn by the SPV has left the vault but
//!   still belongs to the pool. Instruments, vault and in-flight sum to NAV, and every
//!   transition moves value between those three lines rather than creating any.
//! - **The withdrawal queue lives in its own account.** An account created through a CPI
//!   caps at 10,240 bytes and 256 requests need 20,801, so the queue is created at the cap
//!   and grown by realloc. That limit exists on no other chain Piron runs on.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;
use anchor_spl::token::{self, Burn, Mint, MintTo, Token, TokenAccount, Transfer};

pub mod seeds;
pub mod state;

use piron_core::stable_yield::{nav, queue as core_queue, validate};
use state::{to_u64, StableYieldPool, StoredInstrument, StoredRequest, WithdrawalQueue, MAX_INSTRUMENTS};

declare_id!("PironStab1e11111111111111111111111111111111");

fn map_err(err: piron_core::Error) -> Error {
    Error::from(ProgramError::Custom(err.code()))
}
fn clock_now() -> Result<u64> {
    Ok(Clock::get()?.unix_timestamp.max(0) as u64)
}
fn add(a: u64, b: u64) -> Result<u64> {
    a.checked_add(b).ok_or_else(|| map_err(piron_core::Error::Overflow))
}

#[program]
pub mod piron_stable_yield {
    use super::*;

    #[allow(clippy::too_many_arguments)]
    pub fn initialize_pool(
        ctx: Context<InitializePool>,
        pool_id: u64,
        spv: Pubkey,
        treasury: Pubkey,
        min_investment: u64,
        holding_period: i64,
        withdrawal_fee_bps: u32,
        min_absolute_reserve: u64,
        reserve_ratio_bps: u32,
    ) -> Result<()> {
        let pool_key = ctx.accounts.pool.key();
        let queue_key = ctx.accounts.queue.key();
        {
            let mut queue = ctx.accounts.queue.load_init()?;
            queue.pool = pool_key;
        }
        {
            // `load_init` rather than `load_mut`: the account was just created and its
            // discriminator is not written yet.
            let mut pool = ctx.accounts.pool.load_init()?;
            pool.pool_id = pool_id;
            pool.authority = ctx.accounts.authority.key();
            pool.spv = spv;
            pool.treasury = treasury;
            pool.asset_mint = ctx.accounts.asset_mint.key();
            pool.vault = ctx.accounts.vault.key();
            pool.share_mint = ctx.accounts.share_mint.key();
            pool.queue = queue_key;
            pool.min_investment = min_investment;
            pool.holding_period = holding_period;
            pool.withdrawal_fee_bps = withdrawal_fee_bps;
            pool.min_absolute_reserve = min_absolute_reserve;
            pool.reserve_ratio_bps = reserve_ratio_bps;
            pool.is_active = 1;
            pool.bump = ctx.bumps.pool;
        }
        emit!(PoolInitialized { pool: pool_key, pool_id });
        Ok(())
    }

    /// Buys into the pool at today's price.
    pub fn deposit(ctx: Context<Deposit>, pool_id: u64, assets: u64) -> Result<u64> {
        let _ = pool_id;
        let (min_investment, active, total_nav, bump, id) = {
            let pool = ctx.accounts.pool.load()?;
            (pool.min_investment, pool.is_active != 0, pool.total_nav, pool.bump, pool.pool_id)
        };
        validate::validate_deposit(assets as i128, min_investment as i128, active).map_err(map_err)?;

        let supply = ctx.accounts.share_mint.supply as i128;
        let price = nav::nav_per_share(total_nav as i128, supply).map_err(map_err)?;
        let shares =
            to_u64(nav::shares_for_deposit(assets as i128, price).map_err(map_err)?).map_err(map_err)?;

        token::transfer(ctx.accounts.holder_to_vault(), assets)?;

        let id_bytes = id.to_le_bytes();
        let signer: &[&[&[u8]]] = &[&[seeds::POOL, id_bytes.as_ref(), &[bump]]];
        token::mint_to(ctx.accounts.mint_shares().with_signer(signer), shares)?;

        let mut pool = ctx.accounts.pool.load_mut()?;
        pool.total_nav = add(pool.total_nav, assets)?;

        emit!(Deposited {
            pool: ctx.accounts.pool.key(),
            investor: ctx.accounts.holder.key(),
            assets,
            shares,
        });
        Ok(shares)
    }

    /// Sends free cash to the SPV, leaving the reserve intact.
    ///
    /// NAV does not move: the cash leaves the vault and reappears as in-flight capital.
    pub fn deploy_capital(ctx: Context<SpvMovesFunds>, pool_id: u64, amount: u64) -> Result<()> {
        let _ = pool_id;
        let (total_nav, config, bump, id, spv) = {
            let pool = ctx.accounts.pool.load()?;
            (
                pool.total_nav as i128,
                piron_core::stable_yield::types::ReserveConfig {
                    min_absolute_reserve: pool.min_absolute_reserve as i128,
                    reserve_ratio_bps: pool.reserve_ratio_bps,
                },
                pool.bump,
                pool.pool_id,
                pool.spv,
            )
        };
        if ctx.accounts.spv.key() != spv {
            return Err(Error::from(ProgramError::MissingRequiredSignature));
        }
        validate::validate_deployment(
            amount as i128,
            ctx.accounts.vault.amount as i128,
            total_nav,
            &config,
        )
        .map_err(map_err)?;

        let id_bytes = id.to_le_bytes();
        let signer: &[&[&[u8]]] = &[&[seeds::POOL, id_bytes.as_ref(), &[bump]]];
        token::transfer(ctx.accounts.vault_to_spv().with_signer(signer), amount)?;

        let mut pool = ctx.accounts.pool.load_mut()?;
        pool.in_flight = add(pool.in_flight, amount)?;
        let in_flight = pool.in_flight;
        drop(pool);
        emit!(CapitalDeployed { pool: ctx.accounts.pool.key(), amount, in_flight });
        Ok(())
    }

    /// Returns capital or yield from the SPV.
    ///
    /// Returns retire in-flight capital first; anything beyond it is yield, which shows up
    /// as a NAV gain rather than driving the outstanding balance below zero.
    pub fn return_capital(ctx: Context<SpvMovesFunds>, pool_id: u64, amount: u64) -> Result<()> {
        let _ = pool_id;
        if amount == 0 {
            return Err(map_err(piron_core::Error::InvalidAmount));
        }
        let spv = ctx.accounts.pool.load()?.spv;
        if ctx.accounts.spv.key() != spv {
            return Err(Error::from(ProgramError::MissingRequiredSignature));
        }
        token::transfer(ctx.accounts.spv_to_vault(), amount)?;

        let mut pool = ctx.accounts.pool.load_mut()?;
        let retired = amount.min(pool.in_flight);
        pool.in_flight -= retired;
        pool.total_nav = add(pool.total_nav, amount - retired)?;
        let in_flight = pool.in_flight;
        drop(pool);
        emit!(CapitalReturned { pool: ctx.accounts.pool.key(), amount, in_flight });
        Ok(())
    }
}

// ---------------------------------------------------------------- contexts
//
// The pool and the queue are zero-copy, so they arrive as `AccountLoader` and are opened
// with `load()` / `load_mut()` inside the handler. That also means a seed constraint
// cannot read `pool.pool_id` in the attribute — nothing is deserialised yet — so pool_id
// comes in as an instruction argument and the seeds are checked against it.

#[derive(Accounts)]
#[instruction(pool_id: u64)]
pub struct InitializePool<'info> {
    #[account(init, payer = authority, space = StableYieldPool::LEN,
              seeds = [seeds::POOL, &pool_id.to_le_bytes()], bump)]
    pub pool: AccountLoader<'info, StableYieldPool>,
    pub asset_mint: Box<Account<'info, Mint>>,
    #[account(init, payer = authority, seeds = [seeds::VAULT, pool.key().as_ref()], bump,
              token::mint = asset_mint, token::authority = pool)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(init, payer = authority, seeds = [seeds::SHARE_MINT, pool.key().as_ref()], bump,
              mint::decimals = asset_mint.decimals, mint::authority = pool)]
    pub share_mint: Box<Account<'info, Mint>>,
    /// A client-created keypair account, not a PDA.
    ///
    /// This is the one place the design bends to the runtime. A CPI can allocate at most
    /// 10,240 bytes and the queue needs 22,600, and a PDA cannot sign a top-level create,
    /// so it cannot be a PDA and be this large. The client creates it as an ordinary
    /// account owned by this program and the pool records its address; anything deriving
    /// the queue reads `pool.queue` rather than computing a seed.
    #[account(zero)]
    pub queue: AccountLoader<'info, WithdrawalQueue>,
    #[account(mut)]
    pub authority: Signer<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
#[instruction(pool_id: u64)]
pub struct Deposit<'info> {
    #[account(mut, seeds = [seeds::POOL, &pool_id.to_le_bytes()], bump)]
    pub pool: AccountLoader<'info, StableYieldPool>,
    #[account(mut, seeds = [seeds::VAULT, pool.key().as_ref()], bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, seeds = [seeds::SHARE_MINT, pool.key().as_ref()], bump)]
    pub share_mint: Box<Account<'info, Mint>>,
    #[account(mut, token::authority = holder)]
    pub holder_asset: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::authority = holder)]
    pub holder_shares: Box<Account<'info, TokenAccount>>,
    #[account(mut)]
    pub holder: Signer<'info>,
    pub token_program: Program<'info, Token>,
}

impl<'info> Deposit<'info> {
    fn holder_to_vault(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(self.token_program.key(), Transfer {
            from: self.holder_asset.to_account_info(),
            to: self.vault.to_account_info(),
            authority: self.holder.to_account_info(),
        })
    }
    fn mint_shares(&self) -> CpiContext<'_, '_, '_, 'info, MintTo<'info>> {
        CpiContext::new(self.token_program.key(), MintTo {
            mint: self.share_mint.to_account_info(),
            to: self.holder_shares.to_account_info(),
            authority: self.pool.to_account_info(),
        })
    }
}

#[derive(Accounts)]
#[instruction(pool_id: u64)]
pub struct SpvMovesFunds<'info> {
    #[account(mut, seeds = [seeds::POOL, &pool_id.to_le_bytes()], bump)]
    pub pool: AccountLoader<'info, StableYieldPool>,
    #[account(mut, seeds = [seeds::VAULT, pool.key().as_ref()], bump)]
    pub vault: Box<Account<'info, TokenAccount>>,
    #[account(mut, token::authority = spv)]
    pub spv_asset: Box<Account<'info, TokenAccount>>,
    pub spv: Signer<'info>,
    pub token_program: Program<'info, Token>,
}

impl<'info> SpvMovesFunds<'info> {
    fn vault_to_spv(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(self.token_program.key(), Transfer {
            from: self.vault.to_account_info(),
            to: self.spv_asset.to_account_info(),
            authority: self.pool.to_account_info(),
        })
    }
    fn spv_to_vault(&self) -> CpiContext<'_, '_, '_, 'info, Transfer<'info>> {
        CpiContext::new(self.token_program.key(), Transfer {
            from: self.spv_asset.to_account_info(),
            to: self.vault.to_account_info(),
            authority: self.spv.to_account_info(),
        })
    }
}

// ---------------------------------------------------------------- events

#[event] pub struct PoolInitialized { pub pool: Pubkey, pub pool_id: u64 }

#[event] pub struct Deposited { pub pool: Pubkey, pub investor: Pubkey, pub assets: u64, pub shares: u64 }
#[event] pub struct CapitalDeployed { pub pool: Pubkey, pub amount: u64, pub in_flight: u64 }
#[event] pub struct CapitalReturned { pub pool: Pubkey, pub amount: u64, pub in_flight: u64 }
