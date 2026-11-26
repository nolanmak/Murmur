use std::{path::PathBuf, time::Duration};
pub const SOCKET: &str = "agents.sock";
pub struct Config { pub dir: PathBuf, pub home: PathBuf, pub codex: PathBuf, pub claude: PathBuf, pub poll: Duration, pub deadline: Duration }
pub struct Handle;
impl Handle { pub fn shutdown(self) {} }
pub fn start(_c: Config) -> Result<Handle, String> { Err("todo".into()) }
