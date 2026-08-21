//! Protocol errors with stable numeric codes.
//!
//! These codes are API. The backend, the indexer and the Solidity implementation all
//! map onto them, so they are allocated in blocks by origin and are **append-only**.
//! Renumbering one silently changes what an error means to every consumer.
//!
//! | Range   | Module                          |
//! |---------|---------------------------------|
//! | 100–199 | core: math, validation, generic |
//! | 200–299 | single-asset pool               |
//! | 300–399 | locked pool                     |
//! | 400–499 | stable-yield pool               |
//! | 500–599 | escrow / custody                |
//! | 600–699 | registry, access, fees          |

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Error {
    // ---- 100–199: core ----
    Overflow = 100,
    DivisionByZero = 101,
    NegativeAmount = 102,
    InvalidAmount = 103,
    InvalidBps = 104,

    // ---- 200–299: single-asset pool ----
    NotFundingPhase = 200,
    FundingEnded = 201,
    BelowMinInvestment = 202,
    ExceedsTargetRaise = 203,
    WithdrawalsNotAllowed = 204,
    NotInvested = 205,
    NotMatured = 206,
    NotInterestBearing = 207,
    NotDiscounted = 208,
    NoSharesOutstanding = 209,
    NoShares = 210,
    NoCouponsToDistribute = 211,
    NoCouponsDistributed = 212,
    NoNewCoupons = 213,
    InvalidDiscountRate = 214,
    NotInEmergency = 215,
    MaturityNotReached = 216,
    CouponScheduleTooLong = 217,
    CouponScheduleMismatch = 218,
    InvalidMaturityDate = 219,
    TargetNotReached = 220,
    EpochNotEnded = 221,
    NotPendingInvestment = 222,
    InvestmentExceedsRaised = 223,
    CouponDateInPast = 224,
    SharesStillOutstanding = 225,
    InsufficientShares = 226,
    InsufficientPoolBalance = 227,
    ExceedsRefundAmount = 228,
    InvalidStatusForOperation = 229,

    // ---- 300–399: locked pool ----
    InvalidTier = 300,
    InvalidTierDuration = 301,
    TierApyTooHigh = 302,
    TierPenaltyTooHigh = 303,
    TierInactive = 304,
    InterestExceedsPrincipal = 305,
    PositionAlreadyRedeemed = 306,
    PositionAlreadyExited = 307,
    PositionNotMatured = 308,
    PositionNotRedeemable = 309,
    PositionAlreadyMatured = 310,
    PositionNotActive = 311,
    PositionNotFound = 312,

    // ---- 400–499: stable-yield pool ----
    InvalidCouponFrequency = 400,
    InvalidNavPerShare = 401,
    PoolInactive = 402,
    HoldingPeriodNotMet = 403,
    InsufficientLiquidity = 404,
    ReserveBreached = 405,
    RequestAlreadyProcessed = 406,
    RequestExpired = 407,
    QueueEmpty = 408,
    BatchTooLarge = 409,

    // ---- 500–599: escrow / custody ----
    //
    // Owned by the escrow contract, which does not depend on this crate. Listed for the
    // record so the ranges stay legible: 500 NotManager, 501 InvalidAmount,
    // 502 InsufficientBalance, 503 NotInitialized, 504 AlreadyBound.

    // ---- 600–699: registry, access, fees ----
    InvalidSplitRatio = 600,
    FeeTypeInactive = 601,
    RoleDelayNotElapsed = 602,
    ProposalNotFound = 603,
    ProposalAlreadyExecuted = 604,
    ProposalCancelled = 605,
    DeploymentFinalized = 606,
    DeploymentNotFinalized = 607,
    RoleNotHeld = 608,
    CannotRenounce = 609,
    PoolNotRegistered = 610,
    AssetNotApproved = 611,
    SpvNotAuthorized = 612,
    AlreadyRegistered = 613,
    ProtocolPaused = 614,
    UnknownRole = 615,
    UnknownFeeType = 616,
}

impl Error {
    /// The stable wire code. Contract layers map this onto their host error type.
    pub const fn code(self) -> u32 {
        self as u32
    }
}
