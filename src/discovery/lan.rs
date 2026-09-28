use std::env;
use std::sync::Arc;

use anyhow::{bail, Result};

use super::directory::{DirectoryLan, LAN_DIR_ENV};
use super::{interval, LanDiscovery, SourceContext, SourceStatus};

pub async fn run(_context: SourceContext, _status: SourceStatus) {}

pub fn backend() -> Result<Arc<dyn LanDiscovery>> {
    match env::var_os(LAN_DIR_ENV) {
        Some(dir) => Ok(DirectoryLan::start(dir.into(), interval()?)?),
        None => bail!("mDNS discovery is not available yet"),
    }
}
