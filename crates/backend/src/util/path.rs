use std::path::{Path, PathBuf};

pub fn canonicalize(path: PathBuf) -> PathBuf {
    dunce::canonicalize(path.clone()).unwrap_or(path)
}

#[cfg(not(target_os = "windows"))]
pub fn normalize(path: PathBuf) -> PathBuf {
    canonicalize(path)
}

#[cfg(target_os = "windows")]
pub fn normalize(path: PathBuf) -> PathBuf {
    match dunce::canonicalize(&path) {
        Ok(canonicalized) => PathBuf::from(
            canonicalized
                .to_string_lossy()
                .to_string()
                .replace('\\', "/"),
        ),
        Err(_) => path,
    }
}

#[inline]
pub fn has_extension<P: AsRef<Path>, S: AsRef<str>>(path: P, extension: S) -> bool {
    path.as_ref().extension().is_some_and(|x| {
        x.to_str()
            .is_some_and(|x| x.eq_ignore_ascii_case(extension.as_ref()))
    })
}

pub fn open<P: AsRef<Path>>(path: P) -> std::io::Result<()> {
    let path = path.as_ref();
    match opener::open(path) {
        Ok(()) => Ok(()),
        Err(error) => {
            log::error!("Failed to open {}: {error}", path.display());
            Err(std::io::Error::other(error))
        }
    }
}

pub fn open_file_location<P: AsRef<Path>>(path: P) -> std::io::Result<()> {
    let path = dunce::canonicalize(path.as_ref()).unwrap_or_else(|_| path.as_ref().to_path_buf());

    match opener::reveal(&path) {
        Ok(()) => Ok(()),
        Err(error) => {
            log::error!("Failed to reveal {}: {error}", path.display());
            Err(std::io::Error::other(error))
        }
    }
}
