//! No forwarding: backends on 127.0.0.1 only, for tests with vanilla servers (§3.1).

use std::sync::Arc;

use pumbo_core::forwarding::ForwardingMode;
use pumbo_core::registry::{ModuleConfig, ModuleError};

pub const NAME: &str = "none";

#[derive(Debug)]
pub struct NoForwarding;

impl ForwardingMode for NoForwarding {
    fn name(&self) -> &str {
        NAME
    }

    fn requires_local_backend(&self) -> bool {
        true
    }
}

pub fn factory(_: &ModuleConfig) -> Result<Arc<dyn ForwardingMode>, ModuleError> {
    Ok(Arc::new(NoForwarding))
}
