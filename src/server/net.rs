//! TCP sockets whose handles child processes cannot inherit.
//!
//! tokio creates sockets through mio, which on Windows leaves them
//! inheritable. An `[[apps]]` process spawned while such a socket is open
//! receives a copy of it, keeps the connection alive after FlashLin closes
//! its own handle, and resets it when the app later exits. socket2 creates
//! sockets non-inheritable on Windows (and close-on-exec on Unix, as mio
//! already does there). Listeners are built with socket2 in `listener.rs`
//! for the same reason; Hyper's HTTP connector, used for proxying, also
//! creates its sockets with socket2.

use std::io;
use std::net::SocketAddr;

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::{TcpSocket, TcpStream, ToSocketAddrs};

/// Like `tokio::net::TcpStream::connect`, trying each resolved address.
pub async fn connect_tcp(addrs: impl ToSocketAddrs) -> io::Result<TcpStream> {
    let mut last_error = None;
    for addr in tokio::net::lookup_host(addrs).await? {
        match connect_addr(addr).await {
            Ok(stream) => return Ok(stream),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "no addresses to connect to")
    }))
}

async fn connect_addr(addr: SocketAddr) -> io::Result<TcpStream> {
    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
    socket.set_nonblocking(true)?;
    let stream = TcpSocket::from_std_stream(socket.into())
        .connect(addr)
        .await?;
    stream.set_nodelay(true)?;
    Ok(stream)
}

/// Like `tokio::net::TcpListener::bind`, for tests that run alongside
/// tests spawning app processes.
#[cfg(test)]
pub fn bind_tcp(addr: &str) -> io::Result<tokio::net::TcpListener> {
    let addr: SocketAddr = addr
        .parse()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
    socket.set_nonblocking(true)?;
    socket.bind(&addr.into())?;
    socket.listen(1024)?;
    tokio::net::TcpListener::from_std(socket.into())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn connects_and_accepts() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = super::bind_tcp("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (client, server) = tokio::join!(super::connect_tcp(addr), listener.accept());
        let (mut client, (mut server, _)) = (client.unwrap(), server.unwrap());
        client.write_all(b"hi").await.unwrap();
        let mut buffer = [0; 2];
        server.read_exact(&mut buffer).await.unwrap();
        assert_eq!(&buffer, b"hi");
    }

    /// The property this module exists for.
    #[cfg(windows)]
    #[tokio::test]
    async fn sockets_are_not_inheritable() {
        use std::os::windows::io::AsRawSocket;
        #[link(name = "kernel32")]
        extern "system" {
            fn GetHandleInformation(handle: isize, flags: *mut u32) -> i32;
        }
        let inheritable = |socket: u64| {
            let mut flags = 0u32;
            // SAFETY: queries flags of a live socket handle we own.
            assert_ne!(
                unsafe { GetHandleInformation(socket as isize, &mut flags) },
                0
            );
            flags & 1 != 0 // HANDLE_FLAG_INHERIT
        };
        let listener = super::bind_tcp("127.0.0.1:0").unwrap();
        let client = super::connect_tcp(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (accepted, _) = listener.accept().await.unwrap();
        assert!(!inheritable(listener.as_raw_socket()));
        assert!(!inheritable(client.as_raw_socket()));
        assert!(!inheritable(accepted.as_raw_socket()));
    }
}
