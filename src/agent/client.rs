use super::Agent;
use std::{path::Path, time::Duration};
pub enum Frame { Output(Vec<u8>), Exit(i32) }
pub struct Session { pub agent: String, pub id: String, pub cwd: String, pub live: bool, pub active: bool }
pub struct Attachment;
impl Attachment {
    pub fn read(&mut self, _t: Duration) -> Result<Option<Frame>, String> { Err("todo".into()) }
    pub fn input(&mut self, _b: &[u8]) -> Result<(), String> { Err("todo".into()) }
}
pub fn open(_d: &Path, _a: Agent, _r: Option<&str>, _c: &Path, _w: u16, _h: u16) -> Result<String, String> { Err("todo".into()) }
pub fn attach(_d: &Path, _i: &str, _w: u16, _h: u16) -> Result<Attachment, String> { Err("todo".into()) }
pub fn list(_d: &Path) -> Result<Vec<Session>, String> { Err("todo".into()) }
pub fn send(_d: &Path, _i: Option<&str>, _t: &str, _f: impl FnMut(&str)) -> Result<String, String> { Err("todo".into()) }
