use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{RaSvnClient, RaSvnSession, SvnError};

mod config;
mod grouped;
mod session;
#[cfg(test)]
mod tests;

pub use config::{SessionPoolConfig, SessionPoolHealthCheck};
pub use grouped::{SessionPoolKey, SessionPools};
pub use session::{PooledSession, SessionPool};
