#![allow(unused)]
use std::{
    net::{Ipv4Addr, SocketAddrV4},
    sync::atomic::{AtomicU16, Ordering},
};

use tracing_test::traced_test;

use crate::{
    host::LocalNetworking, meta::FrameSerializationFormat, VirtualConnectedSocketExt,
    VirtualTcpListenerExt,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

#[cfg_attr(windows, ignore)]
#[traced_test]
#[tokio::test]
#[serial_test::serial]
async fn test_google_poll() {
    use futures_util::Future;

    // Resolve the address
    tracing::info!("resolving www.google.com");
    let networking = LocalNetworking::new();
    let peer_addr = networking
        .resolve("www.google.com", None, None)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("IP address should be returned");
    tracing::info!("www.google.com = {}", peer_addr);

    // Start the connection
    tracing::info!("connecting to {}:80", peer_addr);
    let mut socket = networking
        .connect_tcp(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            SocketAddr::new(peer_addr, 80),
        )
        .await
        .unwrap();
    tracing::info!("setting nodelay");
    socket.set_nodelay(true).unwrap();
    tracing::info!("setting keepalive");
    socket.set_keepalive(true).unwrap();

    // Wait for it to be ready to send packets
    tracing::info!("waiting for write_ready");
    struct Poller<'a> {
        socket: &'a mut Box<dyn VirtualTcpSocket + Sync>,
    }
    impl<'a> Future for Poller<'a> {
        type Output = Result<usize>;
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            self.socket.poll_write_ready(cx)
        }
    }
    Poller {
        socket: &mut socket,
    }
    .await;

    // Send the data (GET http request)
    let data =
        b"GET / HTTP/1.1\r\nHost: www.google.com\r\nUser-Agent: curl/7.81.0\r\nAccept: */*\r\nConnection: Close\r\n\r\n";
    tracing::info!("sending {} bytes", data.len());
    let sent = socket.send(data).await.unwrap();
    assert_eq!(sent, data.len());

    // Enter a loop that will return all the data
    loop {
        // Wait for the next bit of data
        tracing::info!("waiting for read ready");
        struct Poller<'a> {
            socket: &'a mut Box<dyn VirtualTcpSocket + Sync>,
        }
        impl<'a> Future for Poller<'a> {
            type Output = Result<usize>;
            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                self.socket.poll_read_ready(cx)
            }
        }
        Poller {
            socket: &mut socket,
        }
        .await;

        // Now read the data
        let mut buf = [0u8; 4096];
        match socket.read(&mut buf).await {
            Ok(0) => break,
            Ok(amt) => {
                tracing::info!("received {amt} bytes");
                continue;
            }
            Err(err) => {
                tracing::info!("failed - {}", err);
                panic!("failed to receive data");
            }
        }
    }

    tracing::info!("done");
}

#[cfg_attr(windows, ignore)]
#[traced_test]
#[tokio::test]
#[serial_test::serial]
async fn test_google_epoll() {
    use futures_util::Future;
    use virtual_mio::SharedWakerInterestHandler;

    // Resolve the address
    tracing::info!("resolving www.google.com");
    let networking = LocalNetworking::new();
    let peer_addr = networking
        .resolve("www.google.com", None, None)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("IP address should be returned");
    tracing::info!("www.google.com = {}", peer_addr);

    // Start the connection
    tracing::info!("connecting to {}:80", peer_addr);
    let mut socket = networking
        .connect_tcp(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            SocketAddr::new(peer_addr, 80),
        )
        .await
        .unwrap();
    tracing::info!("setting nodelay");
    socket.set_nodelay(true).unwrap();
    tracing::info!("setting keepalive");
    socket.set_keepalive(true).unwrap();

    // Wait for it to be ready to send packets
    tracing::info!("waiting for writability");
    struct Poller<'a> {
        handler: Option<Box<SharedWakerInterestHandler>>,
        socket: &'a mut Box<dyn VirtualTcpSocket + Sync>,
    }
    impl<'a> Future for Poller<'a> {
        type Output = Result<()>;
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            if self.handler.is_none() {
                self.handler
                    .replace(SharedWakerInterestHandler::new(cx.waker()));
                let handler = self.handler.as_ref().unwrap().clone();
                self.socket.set_handler(handler);
            }
            if self
                .handler
                .as_mut()
                .unwrap()
                .pop_interest(InterestType::Writable)
            {
                return Poll::Ready(Ok(()));
            }
            Poll::Pending
        }
    }
    Poller {
        handler: None,
        socket: &mut socket,
    }
    .await;

    // Send the data (GET http request)
    let data =
        b"GET / HTTP/1.1\r\nHost: www.google.com\r\nUser-Agent: curl/7.81.0\r\nAccept: */*\r\nConnection: Close\r\n\r\n";
    tracing::info!("sending {} bytes", data.len());
    let sent = socket.try_send(data).unwrap();
    assert_eq!(sent, data.len());

    // We detect if there are lots of false positives, that means something has gone
    // wrong with the epoll implementation
    let mut false_interest = 0usize;

    // Enter a loop that will return all the data
    loop {
        // Wait for the next bit of data
        tracing::info!("waiting for readability");
        struct Poller<'a> {
            handler: Option<Box<SharedWakerInterestHandler>>,
            socket: &'a mut Box<dyn VirtualTcpSocket + Sync>,
        }
        impl<'a> Future for Poller<'a> {
            type Output = Result<()>;
            fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                if self.handler.is_none() {
                    self.handler
                        .replace(SharedWakerInterestHandler::new(cx.waker()));
                    let handler = self.handler.as_ref().unwrap().clone();
                    self.socket.set_handler(handler);
                }
                if self
                    .handler
                    .as_mut()
                    .unwrap()
                    .pop_interest(InterestType::Readable)
                {
                    return Poll::Ready(Ok(()));
                }
                Poll::Pending
            }
        }
        Poller {
            handler: None,
            socket: &mut socket,
        }
        .await;

        // Now read the data until we block
        let mut done = false;
        for n in 0.. {
            let mut buf: [MaybeUninit<u8>; 4096] = [MaybeUninit::uninit(); 4096];
            match socket.try_recv(&mut buf) {
                Ok(0) => {
                    done = true;
                    break;
                }
                Ok(amt) => {
                    tracing::info!("received {amt} bytes");
                    continue;
                }
                Err(NetworkError::WouldBlock) => {
                    if n == 0 {
                        false_interest += 1;
                    }
                    break;
                }
                Err(err) => {
                    tracing::info!("failed - {}", err);
                    panic!("failed to receive data");
                }
            }
        }
        if done {
            break;
        }
    }

    if false_interest > 20 {
        panic!("too many false positives on the epoll ({false_interest}), something has likely gone wrong")
    }

    tracing::info!("done");
}
