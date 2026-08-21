//! A fixed-capacity coupon schedule.
//!
//! The Solidity carries two parallel dynamic arrays. Neither runtime wants that: a Soroban
//! contract has no heap allocator, and a Solana account is sized at creation. Since
//! [`crate::MAX_COUPON_SCHEDULE`] already bounds the length, the schedule is stored inline
//! and `piron-core` needs no allocator at all.
//!
//! Holding dates and rates in one type also makes the "same length" invariant
//! unrepresentable-if-violated: it is checked once at construction rather than at every
//! use site.

use crate::error::Error;
use crate::MAX_COUPON_SCHEDULE;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CouponSchedule {
    dates: [u64; MAX_COUPON_SCHEDULE],
    /// Basis points of principal, positionally paired with `dates`.
    rates: [u32; MAX_COUPON_SCHEDULE],
    len: usize,
}

impl Default for CouponSchedule {
    fn default() -> Self {
        Self::empty()
    }
}

impl CouponSchedule {
    /// A discounted instrument's schedule: none.
    pub const fn empty() -> Self {
        Self {
            dates: [0; MAX_COUPON_SCHEDULE],
            rates: [0; MAX_COUPON_SCHEDULE],
            len: 0,
        }
    }

    /// Builds a schedule from parallel slices, as the Solidity stores them.
    pub fn from_parts(dates: &[u64], rates: &[u32]) -> Result<Self, Error> {
        if dates.len() != rates.len() {
            return Err(Error::CouponScheduleMismatch);
        }
        if dates.len() > MAX_COUPON_SCHEDULE {
            return Err(Error::CouponScheduleTooLong);
        }

        let mut schedule = Self::empty();
        for i in 0..dates.len() {
            schedule.dates[i] = dates[i];
            schedule.rates[i] = rates[i];
        }
        schedule.len = dates.len();
        Ok(schedule)
    }

    /// Appends one coupon. Used by bindings reading storage entry by entry.
    pub fn push(&mut self, date: u64, rate: u32) -> Result<(), Error> {
        if self.len == MAX_COUPON_SCHEDULE {
            return Err(Error::CouponScheduleTooLong);
        }
        self.dates[self.len] = date;
        self.rates[self.len] = rate;
        self.len += 1;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn dates(&self) -> &[u64] {
        &self.dates[..self.len]
    }

    pub fn rates(&self) -> &[u32] {
        &self.rates[..self.len]
    }

    /// The first scheduled date, if any.
    pub fn first_date(&self) -> Option<u64> {
        self.dates().first().copied()
    }

    /// Iterates `(date, rate)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (u64, u32)> + '_ {
        self.dates().iter().copied().zip(self.rates().iter().copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_slices_of_unequal_length_are_rejected() {
        assert_eq!(
            CouponSchedule::from_parts(&[1, 2], &[100]),
            Err(Error::CouponScheduleMismatch)
        );
    }

    #[test]
    fn a_schedule_longer_than_the_bound_is_rejected() {
        let dates = [0u64; MAX_COUPON_SCHEDULE + 1];
        let rates = [100u32; MAX_COUPON_SCHEDULE + 1];
        assert_eq!(
            CouponSchedule::from_parts(&dates, &rates),
            Err(Error::CouponScheduleTooLong)
        );
    }

    #[test]
    fn a_schedule_round_trips_its_pairs() {
        let schedule = CouponSchedule::from_parts(&[100, 200], &[250, 250]).unwrap();
        assert_eq!(schedule.len(), 2);
        assert_eq!(schedule.first_date(), Some(100));
        let pairs: [(u64, u32); 2] = [
            schedule.iter().next().unwrap(),
            schedule.iter().nth(1).unwrap(),
        ];
        assert_eq!(pairs, [(100, 250), (200, 250)]);
    }

    #[test]
    fn pushing_past_capacity_fails_rather_than_silently_dropping() {
        let mut schedule = CouponSchedule::empty();
        for i in 0..MAX_COUPON_SCHEDULE {
            assert_eq!(schedule.push(i as u64, 100), Ok(()));
        }
        assert_eq!(schedule.push(999, 100), Err(Error::CouponScheduleTooLong));
    }

    #[test]
    fn an_empty_schedule_has_no_first_date() {
        assert_eq!(CouponSchedule::empty().first_date(), None);
        assert!(CouponSchedule::empty().is_empty());
    }
}
