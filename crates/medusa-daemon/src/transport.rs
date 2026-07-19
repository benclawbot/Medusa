//! Platform-specific local IPC transport for the daemon protocol.

#[cfg(unix)]
mod platform {
    use std::{
        fs,
        io::{self, Read, Write},
        os::unix::net::{UnixListener, UnixStream},
        path::{Path, PathBuf},
        time::Duration,
    };

    pub struct LocalListener {
        inner: UnixListener,
        endpoint: PathBuf,
    }

    impl LocalListener {
        pub fn bind(endpoint: &Path) -> io::Result<Self> {
            if endpoint.exists() {
                fs::remove_file(endpoint)?;
            }
            let inner = UnixListener::bind(endpoint)?;
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
        fs,
        io::{self, Read, Write},
        net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
        path::{Path, PathBuf},
        time::Duration,
    };

    use serde::{Deserialize, Serialize};

    const CAPABILITY_BYTES: usize = 32;
    const CAPABILITY_HEX_LENGTH: usize = CAPABILITY_BYTES * 2;
    const AUTHENTICATION_TIMEOUT: Duration = Duration::from_secs(5);

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
            if let Some(parent) = endpoint.parent() {
                fs::create_dir_all(parent)?;
            }
            if endpoint.exists() {
                fs::remove_file(endpoint)?;
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
            let temporary = endpoint.with_extension("endpoint.tmp");
            let encoded = serde_json::to_vec(&descriptor).map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("failed to encode daemon endpoint descriptor: {error}"),
                )
            })?;
            fs::write(&temporary, encoded)?;
            fs::rename(&temporary, endpoint)?;
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

#[cfg(all(test, windows))]
mod tests {
    use std::{
        fs,
        io::{BufRead, BufReader, Write},
        net::{SocketAddr, TcpStream},
    };

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
