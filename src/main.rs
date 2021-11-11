#![recursion_limit = "256"]

use std::error::Error;
use std::net::SocketAddr;
use std::os::raw::{c_char, c_int, c_short, c_uchar, c_ulong, c_ushort};
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::sync::Arc;

use libc::*;
use nix::sys::socket::InetAddr;

use tokio::io::AsyncBufReadExt;
use tokio::{
    fs::File,
    io::{stdin, AsyncReadExt, AsyncWriteExt, BufReader, BufWriter},
    net::UdpSocket,
    select,
    // sync::mpsc,
};

macro_rules! ioctl(
	($fd:expr, $flags:expr, $value:expr) => ({
		let rc = libc::ioctl($fd, $flags, $value);
		if rc < 0 {
			Err(std::io::Error::last_os_error())
		} else {
			Ok(())
		}
	})
);

type IfName = [c_char; IFNAMSIZ];

#[repr(C)]
#[derive(Copy, Clone, Debug)]
pub struct ifmap {
    pub mem_start: c_ulong,
    pub mem_end: c_ulong,
    pub base_addr: c_ushort,
    pub irq: c_uchar,
    pub dma: c_uchar,
    pub port: c_uchar,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub union ifr_ifru {
    pub ifr_addr: libc::sockaddr,
    pub ifr_dstaddr: libc::sockaddr,
    pub ifr_broadaddr: libc::sockaddr,
    pub ifr_netmask: libc::sockaddr,
    pub ifr_hwaddr: libc::sockaddr,
    pub ifr_flags: c_short,
    pub ifr_ifindex: c_int,
    pub ifr_metric: c_int,
    pub ifr_mtu: c_int,
    pub ifr_map: ifmap,
    pub ifr_slave: IfName,
    pub ifr_newname: IfName,
    pub ifr_data: *mut c_char,
}

#[repr(C)]
#[derive(Copy, Clone)]
pub struct ifreq {
    pub ifr_name: IfName,
    pub ifr_ifru: ifr_ifru,
}

impl ifreq {
    pub fn with_if_name(iface: &str) -> ifreq {
        let mut ifr = ifreq::default();
        for (a, c) in ifr.ifr_name.iter_mut().zip(iface.bytes()) {
            *a = c as i8;
        }
        ifr
    }
}

impl Default for ifreq {
    fn default() -> ifreq {
        unsafe { std::mem::zeroed() }
    }
}

const IFF_UP: i16 = 1;
const IFF_RUNNING: i16 = 1 << 6;

/* TUNSETIFF ifr flags */
const IFF_TUN: i16 = 0x0001;
const IFF_NO_PI: i16 = 0x1000;
const IFF_MULTI_QUEUE: i16 = 0x0100;

/* Ioctl defines */
const TUNSETIFF: u64 = 0x4004_54ca;

/* Socket configuration controls. */
const SIOCGIFFLAGS: u64 = 0x8914; /* get flags */
const SIOCSIFFLAGS: u64 = 0x8914; /* set flags */
const SIOCSIFADDR: u64 = 0x8916; /* set PA address */
const SIOCSIFNETMASK: u64 = 0x891c; /* set network PA mask */

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let tun_file = File::open("/dev/net/tun").await?;
    let rawfd = tun_file.as_raw_fd();

    // iface up
    let mut req = ifreq::with_if_name("");
    req.ifr_ifru.ifr_flags = IFF_TUN | IFF_NO_PI | IFF_MULTI_QUEUE;
    unsafe { ioctl!(rawfd, TUNSETIFF, &req) }?;

    // set ip
    const IPPROTO_IP: c_int = 0;
    let sock4 = unsafe { socket(AF_INET, SOCK_DGRAM, IPPROTO_IP) };
    unsafe {
        ioctl!(sock4, SIOCGIFFLAGS, &req)?;
        req.ifr_ifru.ifr_flags |= IFF_UP | IFF_RUNNING;
        ioctl!(sock4, SIOCSIFFLAGS, &req)?;
    }

    let cidr: ipnet::IpNet = "10.0.5.1/24".parse().unwrap();
    let addr = InetAddr::from_std(&(cidr.addr(), 0).into());
    match addr {
        InetAddr::V4(sockaddr_in) => unsafe {
            req.ifr_ifru.ifr_addr = std::mem::transmute(sockaddr_in);
            ioctl!(sock4, SIOCSIFADDR, &req)?;
        },
        InetAddr::V6(_) => {}
    };

    // set mask
    let netmask = InetAddr::from_std(&(cidr.netmask(), 0).into());
    match netmask {
        InetAddr::V4(sockaddr_in) => unsafe {
            req.ifr_ifru.ifr_netmask = std::mem::transmute(sockaddr_in);
            ioctl!(sock4, SIOCSIFNETMASK, &req)?;
        },
        InetAddr::V6(_) => (),
    };

    let mut tun_reader = BufReader::new(unsafe { File::from_raw_fd(rawfd) });
    let mut tun_writer = BufWriter::new(unsafe { File::from_raw_fd(rawfd) });
    // let mut stdin_reader = BufReader::new(stdin());

    let udp_socket = UdpSocket::bind("0.0.0.0:9090").await?;
    println!("Listening on {}", udp_socket.local_addr()?);
    let udp_receiver = Arc::new(udp_socket);
    let udp_sender = udp_receiver.clone();
    // let (tx, mut rx) = mpsc::channel::<(Vec<u8>, SocketAddr)>(1_000);

    // udp send
    /*tokio::spawn(async move {
        while let Some((data, addr)) = rx.recv().await {
            match udp_sender.send_to(&data, &addr).await {
                Ok(n) => {
                    println!("{:?} bytes sent", n);
                }
                Err(e) => {
                    println!("udp read error: {}", e);
                }
            }
        }
    });*/

    // udp receive
    tokio::spawn(
        async move {
            loop {
                let mut udp_buf = vec![0u8; 1024];
                match udp_receiver.recv_from(&mut udp_buf).await  {
                    Ok((n, peer)) => {
                        if n > 0 {
                            println!("received {} bytes {:?} from {}", n, &udp_buf[..n], peer);
                        }
                        match tun_writer.write(&udp_buf[..n]).await {
                            Ok(n) => {
                                println!("write {} bytes to tun", n);
                            },
                            Err(e) => {
                                println!("tun write error: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        println!("udp read error: {}", e);
                    }
                }
            }
        }
    );

    // stdin read
    /*tokio::spawn(async move {
        loop {
            let mut stdin_buf = String::new();
            match stdin_reader.read_line(&mut stdin_buf).await {
                Ok(n) => {
                    if n > 0 {
                        println!("read {} bytes {:?} from stdin", n, stdin_buf);
                    }
                    let buf: [u8; 84] = [
                        69, 0, 0, 84, 97, 87, 64, 0, 64, 1, 187, 79, 10, 0, 5, 1, 10, 0, 5, 2, 8,
                        0, 45, 248, 90, 168, 0, 1, 23, 64, 70, 96, 0, 0, 0, 0, 70, 235, 12, 0, 0,
                        0, 0, 0, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31,
                        32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50,
                        51, 52, 53, 54, 5,
                    ];
                    match tun_writer.write(&buf).await {
                        Ok(n) => {
                            println!("write {} bytes to tun", n);
                        }
                        Err(e) => {
                            println!("tun write error: {}", e);
                        }
                    }
                }
                Err(e) => {
                    println!("stdin read error: {}", e);
                }
            }
        }
    });*/

    loop {
        let mut tun_buf = vec![0u8; 1500];
        // let mut stdin_buf = String::new();

        select! {
            r = tun_reader.read(&mut tun_buf) => match r {
                Ok(n) => {
                    if n > 0 {
                        println!("read {} bytes {:?} from tun", n, &tun_buf[..n]);
                    }
                    let remote_addr:SocketAddr = "127.0.0.1:9091".parse().unwrap();
                    /*match tx.send((tun_buf, remote_addr)).await {
                        Ok(()) => {
                        }
                        Err(e) => {
                            println!("channel send error: {}", e);
                        }
                    }*/
                    match udp_sender.send_to(&tun_buf[..n], &remote_addr).await {
                        Ok(n) => {
                            println!("{:?} bytes sent", n);
                        }
                        Err(e) => {
                            println!("udp read error: {}", e);
                        }
                    }
                }
                Err(e) => {
                    println!("tun read error: {}", e);
                }
            },

            /*r = stdin_reader.read_line(&mut stdin_buf) => match r {
                Ok(n) => {
                    if n > 0 {
                        println!("read {} bytes {:?} from stdin", n, stdin_buf);
                    }
                    let buf: [u8; 84] = [69, 0, 0, 84, 97, 87, 64, 0, 64, 1, 187, 79, 10, 0, 5, 1, 10, 0, 5, 2, 8, 0, 45, 248, 90, 168, 0, 1, 23, 64, 70, 96, 0, 0, 0, 0, 70, 235, 12, 0, 0, 0, 0, 0, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 5];
                    match tun_writer.write(&buf).await {
                        Ok(n) => {
                            println!("write {} bytes to tun", n);
                        },
                        Err(e) => {
                            println!("tun write error: {}", e);
                        }
                    }
                }
                Err(e) => {
                    println!("stdin read error: {}", e);
                }
            },*/
        }
    }
}
