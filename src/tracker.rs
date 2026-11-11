//! Bounded in-flight request tracking for application-owned scheduling.
use crate::{Error, ErrorKind, rpc::Id};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reuse {
    AfterCompletion,
    Never,
}
#[derive(Clone, Copy, Debug)]
pub struct Slot<'a> {
    id: Option<Id<'a>>,
    active: bool,
    cancelled: bool,
    progress: Option<f64>,
}
impl Slot<'_> {
    pub const EMPTY: Self = Self {
        id: None,
        active: false,
        cancelled: false,
        progress: None,
    };
}

/// Retain the borrowed input and token tape until the corresponding slot is released.
/// Legacy peers may use `Never` to retain completed IDs until the connection closes.
pub struct Tracker<'a, 's> {
    slots: &'s mut [Slot<'a>],
    reuse: Reuse,
}
impl<'a, 's> Tracker<'a, 's> {
    pub fn new(slots: &'s mut [Slot<'a>], reuse: Reuse) -> Self {
        slots.fill(Slot::EMPTY);
        Self { slots, reuse }
    }
    fn index(&self, id: Id<'_>) -> Option<usize> {
        self.slots
            .iter()
            .position(|s| s.id.is_some_and(|v| v.equals(id)))
    }
    pub fn start(&mut self, id: Id<'a>) -> Result<usize, Error> {
        if self.index(id).is_some() {
            return Err(Error::new(ErrorKind::DuplicateId, 0));
        }
        let at = self
            .slots
            .iter()
            .position(|s| s.id.is_none())
            .ok_or(Error::capacity(
                ErrorKind::Buffer,
                0,
                self.slots.len().saturating_add(1),
            ))?;
        self.slots[at] = Slot {
            id: Some(id),
            active: true,
            cancelled: false,
            progress: None,
        };
        Ok(at)
    }
    pub fn cancel(&mut self, id: Id<'_>) -> bool {
        if let Some(at) = self.index(id)
            && self.slots[at].active
        {
            self.slots[at].cancelled = true;
            return true;
        }
        false
    }
    pub fn is_cancelled(&self, id: Id<'_>) -> bool {
        self.index(id).is_some_and(|at| self.slots[at].cancelled)
    }
    /// Progress is finite, nonnegative and strictly increases after the first event.
    pub fn progress(&mut self, id: Id<'_>, value: f64) -> Result<(), Error> {
        let at = self.index(id).ok_or(Error::new(ErrorKind::UnknownId, 0))?;
        let slot = &mut self.slots[at];
        if !slot.active
            || slot.cancelled
            || !value.is_finite()
            || value < 0.0
            || slot.progress.is_some_and(|n| value <= n)
        {
            return Err(Error::new(ErrorKind::State, 0));
        }
        slot.progress = Some(value);
        Ok(())
    }
    pub fn finish(&mut self, id: Id<'_>) -> Result<(), Error> {
        let at = self.index(id).ok_or(Error::new(ErrorKind::UnknownId, 0))?;
        if !self.slots[at].active {
            return Err(Error::new(ErrorKind::State, 0));
        }
        if self.reuse == Reuse::AfterCompletion {
            self.slots[at] = Slot::EMPTY;
        } else {
            self.slots[at].active = false;
        }
        Ok(())
    }
    pub fn active(&self) -> usize {
        self.slots.iter().filter(|s| s.active).count()
    }
    pub fn memory_bytes(&self) -> usize {
        core::mem::size_of::<Self>() + core::mem::size_of_val(self.slots)
    }
}
