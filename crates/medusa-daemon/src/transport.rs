//! Platform-specific local IPC transport for the daemon protocol.

#[cfg(unix)]
mod platform {
    use std::{
        fs,
        io::{self, Read, Write},
        os::unix::net::{UnixListener, UnixStream},
        path::{Path, PathBuf},
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
            self.inner.accept().map(|(stream, _)| LocalStream(stream))
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
    };

    pub struct LocalListener {
        inner: TcpListener,
        endpoint: PathBuf,
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
            let temporary = endpoint.with_extension("endpoint.tmp");
            fs::write(&temporary, address.to_string())?;
            fs::rename(&temporary, endpoint)?;
            Ok(Self {
                inner,
                endpoint: endpoint.to_path_buf(),
            })
        }

        pub fn accept(&self) -> io::Result<LocalStream> {
            self.inner.accept().map(|(stream, _)| LocalStream(stream))
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
        let address = read_address(endpoint).map_err(socket_error)?;
        TcpStream::connect(address)
            .map(LocalStream)
            .map_err(socket_error)
    }

    pub fn wake(endpoint: &Path) -> io::Result<()> {
        let address = read_address(endpoint).map_err(socket_error)?;
        TcpStream::connect(address)
            .map(|_| ())
            .map_err(socket_error)
    }

    fn read_address(endpoint: &Path) -> io::Result<SocketAddr> {
        let raw = fs::read_to_string(endpoint)?;
        let address = raw.trim().parse::<SocketAddr>().map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid daemon endpoint descriptor: {error}"),
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

    fn socket_error(error: io::Error) -> io::Error {
        io::Error::new(error.kind(), format!("daemon socket error: {error}"))
    }
}

pub use platform::{LocalListener, LocalStream, connect, wake};
