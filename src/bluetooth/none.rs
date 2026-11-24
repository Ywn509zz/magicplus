//! Bluetooth non pris en charge sur ce système (Linux : pas encore).

use super::PairedDevice;
use crate::link::BoxLink;
use anyhow::{bail, Result};

pub(super) const SUPPORTED: bool = false;

pub(super) fn powered() -> bool {
    false
}

pub(super) fn listen(_on_link: Box<dyn Fn(BoxLink, String) + Send + Sync>) -> Result<()> {
    bail!("Bluetooth non pris en charge sur ce système")
}

pub(super) fn paired_computers() -> Vec<PairedDevice> {
    Vec::new()
}

pub(super) fn connect(_address: &str) -> Result<BoxLink> {
    bail!("Bluetooth non pris en charge sur ce système")
}
