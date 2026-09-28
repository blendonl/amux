use anyhow::{bail, Result};

use crate::discovery::Discovery;
use crate::protocol::{ClientMessage, Duplex, ServerMessage};

pub type PairingClient = Duplex<ClientMessage, ServerMessage>;

pub async fn open(
    _discovery: &Discovery,
    _new_key: bool,
    _client: &mut PairingClient,
) -> Result<()> {
    bail!("pairing is not available yet")
}

pub async fn join(
    _discovery: &Discovery,
    _code: String,
    _host: Option<String>,
    _new_key: bool,
    _client: &mut PairingClient,
) -> Result<()> {
    bail!("pairing is not available yet")
}
