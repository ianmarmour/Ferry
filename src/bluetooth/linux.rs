//! placeholder
use super::*;
pub struct BlueZ;
impl BlueZ { pub fn new() -> BlueZ { BlueZ } }
impl Backend for BlueZ {
    fn name(&self) -> String { String::new() }
    fn devices(&self) -> BoxFuture<'_, anyhow::Result<Vec<Device>>> { Box::pin(async { Ok(vec![]) }) }
    fn connect<'a>(&'a self, _d: &'a Device, _s: Service) -> BoxFuture<'a, anyhow::Result<Link>> { Box::pin(async { anyhow::bail!("x") }) }
    fn listen(&self) -> BoxFuture<'_, anyhow::Result<Listener>> { Box::pin(async { anyhow::bail!("x") }) }
}
