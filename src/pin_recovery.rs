//! Durable copies of taskbar shortcuts for interrupted pin transfers.

use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct PendingPin {
    pub name: String,
    pub identity: String,
    pub packaged: bool,
    pub original: PathBuf,
    directory: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct Metadata {
    name: String,
    identity: String,
    packaged: bool,
    original: PathBuf,
}

impl PendingPin {
    pub fn stage_and_detach(
        root: &Path,
        source: &Path,
        name: &str,
        identity: &str,
        packaged: bool,
        detach: impl FnOnce(&Path) -> Result<(), String>,
    ) -> io::Result<(Self, Result<(), String>)> {
        let pending = Self::stage(root, source, name, identity, packaged)?;
        let result = detach(source);
        Ok((pending, result))
    }

    pub fn restore_and_clear(
        self,
        restore: impl FnOnce(&Self) -> Result<bool, String>,
    ) -> io::Result<()> {
        match restore(&self).map_err(io::Error::other)? {
            true => self.clear(),
            false => Err(io::Error::other(
                "Windows did not confirm that the pin was restored",
            )),
        }
    }

    pub fn stage(
        root: &Path,
        source: &Path,
        name: &str,
        identity: &str,
        packaged: bool,
    ) -> io::Result<Self> {
        if !source
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Expected a shortcut",
            ));
        }
        fs::create_dir_all(root)?;
        let directory = root.join(Uuid::new_v4().to_string());
        fs::create_dir(&directory)?;
        let pending = Self {
            name: name.to_owned(),
            identity: identity.to_owned(),
            packaged,
            original: source.to_owned(),
            directory,
        };
        let result = (|| {
            let mut source = fs::File::open(source)?;
            let mut copy = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(pending.shortcut())?;
            io::copy(&mut source, &mut copy)?;
            copy.sync_all()?;
            let data = serde_json::to_vec(&Metadata {
                name: pending.name.clone(),
                identity: pending.identity.clone(),
                packaged: pending.packaged,
                original: pending.original.clone(),
            })
            .map_err(io::Error::other)?;
            let mut metadata = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(pending.directory.join("pending.json.tmp"))?;
            metadata.write_all(&data)?;
            metadata.sync_all()?;
            drop(metadata);
            fs::rename(
                pending.directory.join("pending.json.tmp"),
                pending.directory.join("pending.json"),
            )
        })();
        if let Err(error) = result {
            let _ = fs::remove_file(pending.directory.join("pending.json.tmp"));
            let _ = fs::remove_file(pending.directory.join("pending.json"));
            let _ = fs::remove_file(pending.shortcut());
            let _ = fs::remove_dir(&pending.directory);
            return Err(error);
        }
        Ok(pending)
    }

    pub fn shortcut(&self) -> PathBuf {
        self.directory.join("source.lnk")
    }

    pub fn load_all(root: &Path) -> io::Result<Vec<Self>> {
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut records = Vec::new();
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let directory = entry.path();
            let data = match fs::read(directory.join("pending.json")) {
                Ok(data) => data,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            let metadata: Metadata = serde_json::from_slice(&data).map_err(io::Error::other)?;
            let pending = Self {
                name: metadata.name,
                identity: metadata.identity,
                packaged: metadata.packaged,
                original: metadata.original,
                directory,
            };
            if !pending.shortcut().is_file() {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "Recovery shortcut is missing",
                ));
            }
            records.push(pending);
        }
        Ok(records)
    }

    /// Remove a recovery record only after its transfer or restoration is verified.
    pub fn clear(self) -> io::Result<()> {
        fs::remove_file(self.directory.join("pending.json"))?;
        fs::remove_file(self.shortcut())?;
        fs::remove_dir(self.directory)
    }
}
