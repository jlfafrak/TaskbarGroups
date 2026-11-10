//! Recovery records for taskbar buttons removed while a group is active.
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowKey {
    pub hwnd: isize,
    pub pid: u32,
    pub process_started: u64,
    pub marker: String,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct Data {
    windows: Vec<WindowKey>,
}

pub struct RecoveryJournal {
    path: PathBuf,
    data: Data,
}
impl RecoveryJournal {
    pub fn load(path: &Path) -> io::Result<Self> {
        let data = match fs::read(path) {
            Ok(data) => serde_json::from_slice(&data).map_err(io::Error::other)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Data::default(),
            Err(error) => return Err(error),
        };
        Ok(Self {
            path: path.to_owned(),
            data,
        })
    }
    pub fn entries(&self) -> &[WindowKey] {
        &self.data.windows
    }
    fn save(&self) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        if self.data.windows.is_empty() {
            match fs::remove_file(&self.path) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(error),
            }
        }
        let temporary = self.path.with_extension("json.tmp");
        let bytes = serde_json::to_vec(&self.data).map_err(io::Error::other)?;
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, &self.path)
    }
    pub fn hide(
        &mut self,
        window: &WindowKey,
        hide: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        if !self.data.windows.contains(window) {
            self.data.windows.push(window.clone());
            if let Err(error) = self.save() {
                self.data.windows.pop();
                return Err(error);
            }
        }
        hide()
    }
    pub fn restore_all(
        &mut self,
        mut restore: impl FnMut(&WindowKey) -> io::Result<()>,
    ) -> io::Result<()> {
        let windows = self.data.windows.clone();
        let mut failure = None;
        for window in windows {
            if let Err(error) = restore(&window) {
                failure = Some(error);
                continue;
            }
            self.data.windows.retain(|item| item != &window);
            self.save()?;
        }
        failure.map_or(Ok(()), Err)
    }
    pub fn restore_one(
        &mut self,
        window: &WindowKey,
        restore: impl FnOnce(&WindowKey) -> io::Result<()>,
    ) -> io::Result<()> {
        if self.data.windows.contains(window) {
            restore(window)?;
            self.data.windows.retain(|item| item != window);
            self.save()?;
        }
        Ok(())
    }
}
