//! The band above the prompt: notes, the held digit, the note lifecycle at a prompt.

use crate::companion::relay::dispatch::Table;

pub fn register(_t: &mut Table) {}

/// The entries that run after other features' on the same key (`app::table`).
pub fn register_late(_t: &mut Table) {}
