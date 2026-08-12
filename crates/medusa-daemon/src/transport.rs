//! Platform-specific local IPC transport for the daemon protocol.

#[cfg(unix)]
mod platform {
    use std::{
        fs,
        io::{self, Read, Write},
        os::unix::{
            fs::PermissionsExt,
            net::{UnixListener, UnixStream},
        },
        path::{Path, PathBuf},
        time::Duration,
    };

    pub struct LocalListener {
        inner: UnixListener,
        endpoint: PathBuf,
    }

    impl LocalListener {
        pub fn bind(endpoint: &Path) -> io::Result<Self> {
            let parent = endpoint.parent().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "daemon endpoint must have a parent directory",
                )
            })?;
            fs::create_dir_all(parent)?;
            let metadata = fs::symlink_metadata(parent)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "daemon directory must be a real directory",
                ));
            }
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;

            match fs::symlink_metadata(endpoint) {
                Ok(metadata) => {
                    if metadata.is_dir() || metadata.file_type().is_symlink() {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "daemon endpoint must be a replaceable socket path",
                        ));
                    }
                    fs::remove_file(endpoint)?;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }

            let inner = UnixListener::bind(endpoint)?;
            if let Err(error) = fs::set_permissions(endpoint, fs::Permissions::from_mode(0o600)) {
                let _ = fs::remove_file(endpoint);
                return Err(error);
            }
            inner.set_nonblocking(true)?;
            Ok(Self {
                inner,
                endpoint: endpoint.to_path_buf(),
            })
        }

        pub fn accept(&self) -> io::Result<LocalStream> {
            let (stream, _) = self.inner.accept()?;
            stream.set_nonblocking(false)?;
            Ok(LocalStream(stream))
        }

        pub fn cleanup(&self) {
            let _ = fs::remove_file(&self.endpoint);
        }
    }

    pub struct LocalStream(UnixStream);

    impl LocalStream {
        pub fn try_clone(&self) -> io::Result<Self> {
            self.0.try_clone().map(Self)
        }

        pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
            self.0.set_read_timeout(timeout)
        }

        pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
            self.0.set_write_timeout(timeout)
        }
    }

    impl Read for LocalStream {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.0.read(buffer)
        }
    }

    impl Write for LocalStream {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0.write(buffer)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }

    pub fn connect(endpoint: &Path) -> io::Result<LocalStream> {
        UnixStream::connect(endpoint)
            .map(LocalStream)
            .map_err(socket_error)
    }

    pub fn wake(endpoint: &Path) -> io::Result<()> {
        UnixStream::connect(endpoint)
            .map(|_| ())
            .map_err(socket_error)
    }

    fn socket_error(error: io::Error) -> io::Error {
        io::Error::new(error.kind(), format!("daemon socket error: {error}"))
    }
}

#[cfg(windows)]
mod platform {
    use std::{
        fs::{self, OpenOptions},
        io::{self, Read, Write},
        net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
        os::windows::fs::MetadataExt,
        path::{Path, PathBuf},
        time::Duration,
    };

    use medusa_process_containment::{secure_current_user_only, verify_current_user_only};
    use serde::{Deserialize, Serialize};

    const CAPABILITY_BYTES: usize = 32;
    const CAPABILITY_HEX_LENGTH: usize = CAPABILITY_BYTES * 2;
    const AUTHENTICATION_TIMEOUT: Duration = Duration::from_secs(5);
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

    #[derive(Deserialize, Serialize)]
    struct EndpointDescriptor {
        address: String,
        capability: String,
    }

    pub struct LocalListener {
        inner: TcpListener,
        endpoint: PathBuf,
        capability: String,
    }

    impl LocalListener {
        pub fn bind(endpoint: &Path) -> io::Result<Self> {
            let parent = endpoint.parent().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "daemon endpoint must have a parent directory",
                )
            })?;
            fs::create_dir_all(parent)?;
            ensure_real_directory(parent)?;
            secure_current_user_only(parent, true)?;

            match fs::symlink_metadata(endpoint) {
                Ok(metadata) => {
                    if metadata.is_dir()
                        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
                    {
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "daemon endpoint must be a regular replaceable file",
                        ));
                    }
                    fs::remove_file(endpoint)?;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }

            let inner = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
            inner.set_nonblocking(true)?;
            let address = inner.local_addr()?;
            if !address.ip().is_loopback() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "daemon transport must bind to loopback",
                ));
            }

            let capability = generate_capability()?;
            let descriptor = EndpointDescriptor {
                address: address.to_string(),
                capability: capability.clone(),
            };
            let encoded = serde_json::to_vec(&descriptor).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("failed to encode daemon endpoint descriptor: {error}"),
                )
            })?;
            let temporary = endpoint.with_extension(format!("{}.tmp", &capability[..16]));
            let write_result = (|| -> io::Result<()> {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&temporary)?;
                file.write_all(&encoded)?;
                file.sync_all()?;
                secure_current_user_only(&temporary, false)?;
                fs::rename(&temporary, endpoint)?;
                secure_current_user_only(endpoint, false)
            })();
            if let Err(error) = write_result {
                let _ = fs::remove_file(&temporary);
                let _ = fs::remove_file(endpoint);
                return Err(error);
            }

            Ok(Self {
                inner,
                endpoint: endpoint.to_path_buf(),
                capability,
            })
        }

        pub fn accept(&self) -> io::Result<LocalStream> {
            loop {
                let (stream, _) = self.inner.accept()?;
                stream.set_nonblocking(false)?;
                match authenticate(&stream, &self.capability) {
                    Ok(true) => return Ok(LocalStream(stream)),
                    Ok(false) | Err(_) => continue,
                }
            }
        }

        pub fn cleanup(&self) {
            let _ = fs::remove_file(&self.endpoint);
        }
    }

    pub struct LocalStream(TcpStream);

    impl LocalStream {
        pub fn try_clone(&self) -> io::Result<Self> {
            self.0.try_clone().map(Self)
        }

        pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
            self.0.set_read_timeout(timeout)
        }

        pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
            self.0.set_write_timeout(timeout)
        }
    }

    impl Read for LocalStream {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.0.read(buffer)
        }
    }

    impl Write for LocalStream {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0.write(buffer)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }

    pub fn connect(endpoint: &Path) -> io::Result<LocalStream> {
        ensure_secure_descriptor(endpoint)?;
        let descriptor = read_descriptor(endpoint).map_err(socket_error)?;
        let address = validated_address(&descriptor).map_err(socket_error)?;
        let mut stream = TcpStream::connect(address).map_err(socket_error)?;
        stream
            .set_write_timeout(Some(AUTHENTICATION_TIMEOUT))
            .map_err(socket_error)?;
        stream
            .write_all(format!("{}\n", descriptor.capability).as_bytes())
            .map_err(socket_error)?;
        stream.flush().map_err(socket_error)?;
        Ok(LocalStream(stream))
    }

    pub fn wake(endpoint: &Path) -> io::Result<()> {
        connect(endpoint).map(|_| ())
    }

    fn ensure_real_directory(path: &Path) -> io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "daemon directory must be a real directory",
            ));
        }
        Ok(())
    }

    fn ensure_secure_descriptor(endpoint: &Path) -> io::Result<()> {
        let parent = endpoint.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "daemon endpoint must have a parent directory",
            )
        })?;
        ensure_real_directory(parent)?;
        let metadata = fs::symlink_metadata(endpoint)?;
        if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "daemon endpoint descriptor must be a regular file",
            ));
        }
        verify_current_user_only(parent, true)?;
        verify_current_user_only(endpoint, false)
    }

    fn read_descriptor(endpoint: &Path) -> io::Result<EndpointDescriptor> {
        let raw = fs::read_to_string(endpoint)?;
        let descriptor = serde_json::from_str::<EndpointDescriptor>(&raw).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid daemon endpoint descriptor: {error}"),
            )
        })?;
        if descriptor.capability.len() != CAPABILITY_HEX_LENGTH
            || !descriptor
                .capability
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid daemon endpoint capability",
            ));
        }
        Ok(descriptor)
    }

    fn validated_address(descriptor: &EndpointDescriptor) -> io::Result<SocketAddr> {
        let address = descriptor.address.parse::<SocketAddr>().map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid daemon endpoint address: {error}"),
            )
        })?;
        if !address.ip().is_loopback() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "daemon endpoint descriptor is not loopback-only",
            ));
        }
        Ok(address)
    }

    fn generate_capability() -> io::Result<String> {
        let mut bytes = [0_u8; CAPABILITY_BYTES];
        getrandom::fill(&mut bytes).map_err(|error| {
            io::Error::other(format!("failed to generate daemon capability: {error}"))
        })?;
        Ok(hex::encode(bytes))
    }

    fn authenticate(stream: &TcpStream, capability: &str) -> io::Result<bool> {
        stream.set_read_timeout(Some(AUTHENTICATION_TIMEOUT))?;
        let mut reader = stream.try_clone()?;
        let mut supplied = Vec::with_capacity(CAPABILITY_HEX_LENGTH);
        for _ in 0..=CAPABILITY_HEX_LENGTH {
            let mut byte = [0_u8; 1];
            match reader.read(&mut byte)? {
                0 => break,
                _ if byte[0] == b'\n' => break,
                _ => supplied.push(byte[0]),
            }
        }
        Ok(constant_time_eq(&supplied, capability.as_bytes()))
    }

    fn constant_time_eq(supplied: &[u8], expected: &[u8]) -> bool {
        let mut difference = supplied.len() ^ expected.len();
        for (index, expected_byte) in expected.iter().enumerate() {
            difference |=
                usize::from(supplied.get(index).copied().unwrap_or_default() ^ expected_byte);
        }
        difference == 0
    }

    fn socket_error(error: io::Error) -> io::Error {
        io::Error::new(error.kind(), format!("daemon socket error: {error}"))
    }
}

pub use platform::{LocalListener, LocalStream, connect, wake};

#[cfg(all(test, unix))]
mod unix_tests {
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    use tempfile::tempdir;

    use super::LocalListener;

    #[test]
    fn daemon_directory_and_socket_are_owner_only() {
        let root = tempdir().expect("temporary directory");
        let directory = root.path().join("daemon");
        fs::create_dir(&directory).expect("create daemon directory");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o777))
            .expect("make fixture permissive");
        let endpoint = directory.join("medusa.sock");

        let _listener = LocalListener::bind(&endpoint).expect("bind listener");

        assert_eq!(
            fs::metadata(&directory)
                .expect("directory metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&endpoint)
                .expect("socket metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn symlinked_daemon_directory_is_rejected() {
        let root = tempdir().expect("temporary directory");
        let real = root.path().join("real");
        fs::create_dir(&real).expect("create real directory");
        let linked = root.path().join("linked");
        symlink(&real, &linked).expect("create directory symlink");

        let error = LocalListener::bind(&linked.join("medusa.sock"))
            .err()
            .expect("symlinked directory must fail");

        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn symlinked_endpoint_is_rejected() {
        let root = tempdir().expect("temporary directory");
        let directory = root.path().join("daemon");
        fs::create_dir(&directory).expect("create daemon directory");
        let target = root.path().join("target");
        fs::write(&target, b"do not replace").expect("write target");
        let endpoint = directory.join("medusa.sock");
        symlink(&target, &endpoint).expect("create endpoint symlink");

        let error = LocalListener::bind(&endpoint)
            .err()
            .expect("symlinked endpoint must fail");

        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read(&target).expect("read target"), b"do not replace");
    }
}

#[cfg(all(test, windows))]
mod tests {
    use std::{
        fs,
        io::{BufRead, BufReader, Write},
        net::{SocketAddr, TcpStream},
    };

    use medusa_process_containment::verify_current_user_only;
    use serde_json::Value;
    use tempfile::tempdir;

    use super::{LocalListener, connect};

    #[test]
    fn unauthenticated_tcp_connection_is_rejected() {
        let directory = tempdir().expect("temporary directory");
        let endpoint = directory.path().join("medusa.sock");
        let listener = LocalListener::bind(&endpoint).expect("bind listener");
        let raw = fs::read_to_string(&endpoint).expect("read endpoint descriptor");
        let address = descriptor_address(&raw);
        let mut attacker = TcpStream::connect(address).expect("connect without capability");
        attacker
            .write_all(b"forged-capability\n")
            .expect("send forged capability");

        let error = match listener.accept() {
            Ok(_) => panic!("unauthenticated connection must not be accepted"),
            Err(error) => error,
        };

        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    }

    #[test]
    fn descriptor_capability_authenticates_the_supported_client() {
        let directory = tempdir().expect("temporary directory");
        let endpoint = directory.path().join("medusa.sock");
        let listener = LocalListener::bind(&endpoint).expect("bind listener");

        let _client = connect(&endpoint).expect("connect with descriptor capability");
        let _server = listener.accept().expect("accept authenticated client");
    }

    #[test]
    fn authentication_does_not_consume_the_following_request() {
        let directory = tempdir().expect("temporary directory");
        let endpoint = directory.path().join("medusa.sock");
        let listener = LocalListener::bind(&endpoint).expect("bind listener");
        let raw = fs::read_to_string(&endpoint).expect("read endpoint descriptor");
        let address = descriptor_address(&raw);
        let capability = descriptor_capability(&raw);
        let mut client = TcpStream::connect(address).expect("connect client");
        client
            .write_all(format!("{capability}\nrequest-payload\n").as_bytes())
            .expect("send authentication and request together");

        let server = listener.accept().expect("accept authenticated client");
        let mut request = String::new();
        BufReader::new(server)
            .read_line(&mut request)
            .expect("read request payload");

        assert_eq!(request, "request-payload\n");
    }

    #[test]
    fn endpoint_acl_excludes_broad_principals() {
        let directory = tempdir().expect("temporary directory");
        let endpoint = directory.path().join("medusa.sock");
        let _listener = LocalListener::bind(&endpoint).expect("bind listener");

        verify_current_user_only(&endpoint, false)
            .expect("endpoint ACL must grant only the current user");
    }

    fn descriptor_address(raw: &str) -> SocketAddr {
        serde_json::from_str::<Value>(raw)
            .ok()
            .and_then(|value| {
                value
                    .get("address")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .unwrap_or_else(|| raw.trim().to_owned())
            .parse()
            .expect("valid loopback address")
    }

    fn descriptor_capability(raw: &str) -> String {
        serde_json::from_str::<Value>(raw)
            .expect("JSON endpoint descriptor")
            .get("capability")
            .and_then(Value::as_str)
            .expect("endpoint capability")
            .to_owned()
    }
}
