use std::{
    fs,
    io::{self, Write},
    path::Path,
};

pub fn copy_runtime(source: &Path, destination: &Path) -> io::Result<()> {
    let bytes = fs::read(source)?;
    match fs::read(destination) {
        Ok(existing) if existing == bytes => return Ok(()),
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let parent = destination.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "runtime destination has no parent",
        )
    })?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(&bytes)?;
    staged
        .as_file()
        .set_permissions(fs::metadata(source)?.permissions())?;
    // Replacing the inode keeps libraries mapped by running processes intact.
    staged.persist(destination).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn rebuilding_preserves_open_runtime_contents() {
        let temp = tempfile::tempdir().expect("directory");
        let source = temp.path().join("source");
        let destination = temp.path().join("runtime");
        fs::write(&source, b"old runtime").expect("source");
        copy_runtime(&source, &destination).expect("initial copy");
        let mut loaded = fs::File::open(&destination).expect("loaded runtime");
        fs::write(&source, b"new runtime with different contents").expect("updated source");
        copy_runtime(&source, &destination).expect("replacement");
        let mut original = Vec::new();
        loaded.read_to_end(&mut original).expect("open runtime");
        assert_eq!(original, b"old runtime");
        assert_eq!(
            fs::read(&destination).expect("new runtime"),
            b"new runtime with different contents"
        );
    }
}
