// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The source of time. Tests use a fixed clock to expire claims.

use std::fmt::Debug;
use std::sync::atomic::{AtomicI64, Ordering};

use lenka_plugin_api::Timestamp;

/// A source of the current time.
pub trait Clock: Send + Sync + Debug {
    /// Returns the current time.
    fn now(&self) -> Timestamp;
}

/// The system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// A clock that moves only when a test moves it.
#[derive(Debug, Default)]
pub struct FixedClock(AtomicI64);

impl FixedClock {
    /// Creates a clock at a time in milliseconds after the Unix epoch.
    #[must_use]
    pub fn new(unix_ms: i64) -> Self {
        Self(AtomicI64::new(unix_ms))
    }

    /// Moves the clock forward.
    pub fn advance_ms(&self, ms: i64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_unix_ms(self.0.load(Ordering::SeqCst))
    }
}
