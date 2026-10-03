//! Per-store and aggregate guest resource accounting.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use openlogi_core::peripheral::PeripheralError;
use wasmtime::ResourceLimiter;

pub(super) const CALL_FUEL: u64 = 1_000_000;
const MEMORY: usize = 64 * 1024 * 1024;
const TOTAL_MEMORY: usize = 256 * 1024 * 1024;

#[derive(Default)]
pub(super) struct Admission {
    stores: AtomicUsize,
    memory: AtomicUsize,
}

pub(super) struct Quota {
    shared: Arc<Admission>,
    memory: usize,
    growing: usize,
    window: Instant,
    fuel: u64,
    io: u16,
    diagnostics: u16,
}

impl Quota {
    pub(super) fn new(shared: Arc<Admission>) -> Result<Self, PeripheralError> {
        shared
            .stores
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < 16).then_some(n + 1)
            })
            .map_err(|_| limit("16 concurrent plugin stores"))?;
        Ok(Self {
            shared,
            memory: 0,
            growing: 0,
            window: Instant::now(),
            fuel: 0,
            io: 0,
            diagnostics: 0,
        })
    }

    pub(super) fn before_call(&mut self) -> Result<(), PeripheralError> {
        self.window();
        if self.fuel >= 10 * CALL_FUEL {
            return Err(limit("10,000,000 fuel units per second"));
        }
        Ok(())
    }

    pub(super) fn charge_fuel(&mut self, fuel: u64) -> Result<(), PeripheralError> {
        self.fuel += fuel;
        if self.fuel > 10 * CALL_FUEL {
            return Err(limit("10,000,000 fuel units per second"));
        }
        Ok(())
    }

    pub(super) fn io(&mut self) -> Result<(), PeripheralError> {
        self.window();
        self.io += 1;
        if self.io > 100 {
            return Err(limit("100 host submissions per second"));
        }
        Ok(())
    }

    pub(super) fn diagnostic(&mut self) -> Result<(), PeripheralError> {
        self.window();
        self.diagnostics += 1;
        if self.diagnostics > 32 {
            return Err(limit("32 diagnostics per second"));
        }
        Ok(())
    }

    fn window(&mut self) {
        if self.window.elapsed() >= Duration::from_secs(1) {
            self.window = Instant::now();
            self.fuel = 0;
            self.io = 0;
            self.diagnostics = 0;
        }
    }
}

impl ResourceLimiter for Quota {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        let growth = desired.saturating_sub(current);
        if self.memory + growth > MEMORY || maximum.is_some_and(|max| desired > max) {
            return Err(limit("64 MiB store memory or module maximum").into());
        }
        self.shared
            .memory
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n + growth <= TOTAL_MEMORY).then_some(n + growth)
            })
            .map_err(|_| limit("256 MiB aggregate guest memory"))?;
        self.memory += growth;
        self.growing = growth;
        Ok(true)
    }

    fn memory_grow_failed(&mut self, error: wasmtime::Error) -> wasmtime::Result<()> {
        self.shared.memory.fetch_sub(self.growing, Ordering::AcqRel);
        self.memory -= self.growing;
        self.growing = 0;
        Err(error)
    }

    fn table_growing(
        &mut self,
        _: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if desired > 8192 || maximum.is_some_and(|max| desired > max) {
            return Err(limit("8192 table entries or module maximum").into());
        }
        Ok(true)
    }

    fn instances(&self) -> usize {
        32
    }
    fn tables(&self) -> usize {
        4
    }
    fn memories(&self) -> usize {
        4
    }
}

impl Drop for Quota {
    fn drop(&mut self) {
        self.shared.memory.fetch_sub(self.memory, Ordering::AcqRel);
        self.shared.stores.fetch_sub(1, Ordering::AcqRel);
    }
}

fn limit(message: &str) -> PeripheralError {
    PeripheralError::ResourceLimit(message.into())
}
