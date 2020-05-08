#![recursion_limit = "256"]

use std::os::raw::{c_char, c_int, c_short, c_uchar, c_ulong, c_ushort};

use async_std::{
    fs::OpenOptions,
    io::{self, stdin, BufReader},
    os::unix::io::AsRawFd,
    prelude::*,
    task,
};
use futures::{future::FutureExt, select};

use libc::*;
use nix::sys::socket::InetAddr;

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

fn main() -> io::Result<()> {
    task::block_on(async {
        let mut tun_file = OpenOptions::new()
            .read(true)
            .append(true)
            .open("/dev/net/tun")
            .await?;

        // iface up
        let mut req = ifreq::with_if_name("");
        req.ifr_ifru.ifr_flags = IFF_TUN | IFF_NO_PI | IFF_MULTI_QUEUE;
        unsafe { ioctl!(tun_file.as_raw_fd(), TUNSETIFF, &req) }?;

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

        let stdin = BufReader::new(stdin());
        let mut lines_from_stdin = futures::StreamExt::fuse(stdin.lines());
        let mut tun_buf = vec![0u8; 1500];
        loop {
            select! {
                r = tun_file.read(&mut tun_buf).fuse() => match r {
                    Ok(n) => {
                        if n > 0 {
                            println!("read {} bytes {:?} from tun", n, &tun_buf[..n]);
                        }
                    }
                    Err(e) => {
                        println!("read error: {}", e);
                    }
                },
                line = lines_from_stdin.next().fuse() => match line {
                    Some(line) => {
                        let line = line?;
                        println!("line: {}", line);
                        let buf: [u8; 32] = [0x45, 0x00, 0x00, 0x20, 0x91, 0xb3, 0x40, 0x00, 0x40, 0x11, 0x8d, 0x17, 0x0a, 0x00, 0x05, 0x01, 0x0a, 0x00, 0x05, 0x03, 0x83, 0x0d, 0x1f, 0x90, 0x00, 0x0c, 0x7c, 0xc9, 0x61, 0x62, 0x63, 0x0a];
                        match tun_file.write(&buf).await { // block here, not writable
                            Ok(n) => {
                                println!("write {} bytes to tun", n);
                            },
                            Err(e) => {
                                println!("write error: {}", e);
                            }
                        }
                        // the codes below works fine.
                        // let n = match unsafe { libc::write(tun_file.as_raw_fd(), buf.as_ptr() as _, buf.len() as _) } {
                        //     -1 => 0,
                        //     n => n,
                        // };
                        // println!("write {} bytes to tun", n);
                    }
                    None => break,
                }
            }
        }

        Ok(())
    })
}
