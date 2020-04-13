use std::fs;
use std::io::{Error, ErrorKind};
use std::time::{SystemTime, UNIX_EPOCH};

use crypto::digest::Digest;
use crypto::sha1::Sha1;
use directories::UserDirs;

pub fn get_target_path(name: &str) -> Result<String, Error> {
    match UserDirs::new() {
        Some(dirs) => match dirs.download_dir() {
            Some(path) => {
                let now = SystemTime::now();
                let timestamp = now.duration_since(UNIX_EPOCH).expect("Time failed");
                let name = format!("{}_{}", timestamp.as_secs(), name);
                let p = path.join(name);
                let result = p.into_os_string().into_string();
                match result {
                    Ok(value) => Ok(value),
                    Err(_) => Err(Error::new(
                        ErrorKind::InvalidData,
                        "Could not return Downloads path as string",
                    )),
                }
            }
            None => Err(Error::new(
                ErrorKind::NotFound,
                "Downloads directory could not be found",
            )),
        },
        None => Err(Error::new(ErrorKind::NotFound, "Could not check user dirs")),
    }
}

pub fn add_row(value: &str) -> Vec<u8> {
    format!("{}\n", value).into_bytes()
}

pub fn hash_contents(contents: &Vec<u8>) -> String {
    let mut hasher = Sha1::new();
    hasher.input(&contents);
    hasher.result_str()
}

pub fn check_size(path: &str) -> Result<String, Error> {
    let meta = fs::metadata(path)?;
    Ok(meta.len().to_string())
}
