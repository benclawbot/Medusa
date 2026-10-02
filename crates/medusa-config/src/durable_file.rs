use std::{io, path::Path};

pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    medusa_process_containment::atomic_write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_existing_configuration() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("provider.toml");
        atomic_write(&path, b"first").expect("first");
        atomic_write(&path, b"second").expect("second");
        assert_eq!(std::fs::read(&path).expect("read"), b"second");
    }
}
