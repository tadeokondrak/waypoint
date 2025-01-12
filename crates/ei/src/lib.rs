pub extern crate rustix;

use circbuf::CircBuf;
use rustix::{
    cmsg_space,
    event::{PollFd, PollFlags},
    fd::{AsFd, BorrowedFd, OwnedFd},
    io::Errno,
    net::{
        connect_unix, recvmsg, sendmsg, AddressFamily, RecvAncillaryBuffer, RecvAncillaryMessage,
        RecvFlags, SendAncillaryBuffer, SendAncillaryMessage, SendFlags, SocketAddrUnix,
        SocketType,
    },
};
use std::{
    collections::VecDeque,
    fmt::Debug,
    io::{self, IoSlice, IoSliceMut, Read, Write},
    os::unix::prelude::OsStringExt,
};

pub fn client_socket_from_env() -> Result<Option<OwnedFd>, Errno> {
    fn socket_path_from_wayland_display_env() -> Option<Vec<u8>> {
        let display = std::env::var_os("LIBEI_SOCKET")?;
        let display = display.into_vec();
        if display[0] == b'/' {
            return Some(display);
        }
        let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") else {
            eprintln!(
                "warning: LIBEI_SOCKET was not an absolute path and XDG_RUNTIME_PATH is unset"
            );
            return None;
        };
        let mut path = runtime_dir.into_vec();
        path.push(b'/');
        path.extend_from_slice(&display);
        Some(path)
    }

    fn socket_fd_from_socket_path(path: Vec<u8>) -> Result<OwnedFd, Errno> {
        let fd = rustix::net::socket(AddressFamily::UNIX, SocketType::STREAM, None)?;
        let addr = SocketAddrUnix::new(path)?;
        connect_unix(&fd, &addr)?;
        Ok(fd)
    }

    socket_path_from_wayland_display_env()
        .map(socket_fd_from_socket_path)
        .transpose()
}

fn read_from_socket<'fds>(
    buf: &mut CircBuf,
    socket: BorrowedFd<'_>,
    fds: &mut impl Extend<OwnedFd>,
) -> Result<bool, Errno> {
    let mut cmsg_data = vec![0; cmsg_space!(ScmRights(32))];
    let mut ctl = RecvAncillaryBuffer::new(&mut cmsg_data);
    let [first_half, second_half] = buf.get_avail();
    let rustix::net::RecvMsgReturn { bytes: n, .. } = recvmsg(
        &socket,
        &mut [IoSliceMut::new(first_half), IoSliceMut::new(second_half)],
        &mut ctl,
        RecvFlags::DONTWAIT | RecvFlags::CMSG_CLOEXEC,
    )?;
    buf.advance_write_raw(n);
    for msg in ctl.drain() {
        let RecvAncillaryMessage::ScmRights(fd_iter) = msg else {
            continue;
        };
        fds.extend(fd_iter);
    }
    Ok(n > 0)
}

fn write_to_socket(
    buf: &mut CircBuf,
    socket: BorrowedFd<'_>,
    fds: &[BorrowedFd<'_>],
) -> Result<bool, Errno> {
    let mut cmsg_data = vec![0; cmsg_space!(ScmRights(fds.len()))];
    let mut ctl = SendAncillaryBuffer::new(&mut cmsg_data);
    ctl.push(SendAncillaryMessage::ScmRights(fds));
    let [first_half, second_half] = buf.get_bytes();
    let n = sendmsg(
        &socket,
        &[IoSlice::new(first_half), IoSlice::new(second_half)],
        &mut ctl,
        SendFlags::DONTWAIT,
    )?;
    buf.advance_read_raw(n);
    Ok(n > 0)
}

#[derive(Debug)]
pub struct Connection {
    socket: OwnedFd,
    read_buf: CircBuf,
    write_buf: CircBuf,
    read_fds: VecDeque<OwnedFd>,
    write_fds: VecDeque<OwnedFd>,
}

impl AsFd for Connection {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.socket.as_fd()
    }
}

impl Connection {
    pub fn new(fd: OwnedFd) -> Connection {
        Connection {
            socket: fd,
            write_buf: CircBuf::new(),
            read_buf: CircBuf::new(),
            read_fds: VecDeque::new(),
            write_fds: VecDeque::new(),
        }
    }

    pub fn flush_nonblocking(&mut self) -> Result<bool, Errno> {
        if self.write_buf.is_empty() {
            return Ok(true);
        }
        let fds = self
            .write_fds
            .make_contiguous()
            .iter()
            .map(|fd| fd.as_fd())
            .collect::<Vec<_>>();
        let r = write_to_socket(&mut self.write_buf, self.socket.as_fd(), &fds)?;
        self.write_fds.clear();
        Ok(r)
    }

    pub fn flush_blocking(&mut self) -> Result<bool, Errno> {
        loop {
            match self.flush_nonblocking() {
                Ok(v) => break Ok(v),
                Err(Errno::WOULDBLOCK) => {
                    rustix::event::poll(
                        &mut [PollFd::from_borrowed_fd(
                            self.socket.as_fd(),
                            PollFlags::OUT | PollFlags::HUP | PollFlags::ERR,
                        )],
                        -1,
                    )?;
                }
                Err(e) => break Err(e),
            };
        }
    }

    pub fn read_blocking(&mut self) -> Result<bool, Errno> {
        loop {
            match self.read_nonblocking() {
                Ok(v) => break Ok(v),
                Err(Errno::WOULDBLOCK) => {
                    rustix::event::poll(
                        &mut [PollFd::from_borrowed_fd(
                            self.socket.as_fd(),
                            PollFlags::IN | PollFlags::HUP | PollFlags::ERR,
                        )],
                        -1,
                    )?;
                }
                Err(e) => break Err(e),
            }
        }
    }

    pub fn read_nonblocking(&mut self) -> Result<bool, Errno> {
        read_from_socket(&mut self.read_buf, self.socket.as_fd(), &mut self.read_fds)
    }

    pub fn write_message<'a>(
        &mut self,
        obj: u64,
        op: u32,
        args: &[Arg<'a>],
        fds: impl IntoIterator<Item = OwnedFd>,
    ) {
        let bytes_len = args
            .iter()
            .map(|it| match it {
                Arg::Int32(_) | Arg::Uint32(_) | Arg::Float(_) => 4,
                Arg::Int64(_) | Arg::Uint64(_) => 8,
                Arg::String(Some(s)) => 4 + (s.len() + 4) / 4 * 4,
                Arg::String(None) => 4,
                Arg::Array(s) => 4 + (s.len() + 3) / 4 * 4,
            })
            .sum::<usize>();
        self.write_fds.extend(fds);
        assert!(bytes_len < (u32::MAX - 16) as usize);
        let size = u32::from(16 + bytes_len as u32);
        while self.write_buf.avail() < size as usize {
            self.write_buf.grow().unwrap();
        }
        self.write_buf.write_all(&obj.to_ne_bytes()).unwrap();
        self.write_buf.write_all(&size.to_ne_bytes()).unwrap();
        self.write_buf.write_all(&op.to_ne_bytes()).unwrap();
        for &arg in args {
            match arg {
                Arg::Float(v) => self.write_buf.write_all(&v.to_ne_bytes()).unwrap(),
                Arg::Int32(v) => self.write_buf.write_all(&v.to_ne_bytes()).unwrap(),
                Arg::Uint32(v) => self.write_buf.write_all(&v.to_ne_bytes()).unwrap(),
                Arg::Int64(v) => self.write_buf.write_all(&v.to_ne_bytes()).unwrap(),
                Arg::Uint64(v) => self.write_buf.write_all(&v.to_ne_bytes()).unwrap(),
                Arg::String(None) => self.write_buf.write_all(&0u32.to_ne_bytes()).unwrap(),
                Arg::String(Some(s)) => {
                    let s_len = u32::try_from(s.len() + 1).unwrap();
                    self.write_buf.write_all(&s_len.to_ne_bytes()).unwrap();
                    self.write_buf.write_all(&s.as_bytes()).unwrap();
                    let padding_len = (s.len() + 4) / 4 * 4 - s.len();
                    let zeros = [0; 4];
                    self.write_buf.write_all(&zeros[0..padding_len]).unwrap();
                }
                Arg::Array(s) => {
                    let s_len = u32::try_from(s.len() + 1).unwrap();
                    self.write_buf.write_all(&s_len.to_ne_bytes()).unwrap();
                    self.write_buf.write_all(s).unwrap();
                    let padding_len = (s.len() + 3) / 4 * 4 - s.len();
                    let zeros = [0; 3];
                    self.write_buf.write_all(&zeros[0..padding_len]).unwrap();
                }
            }
        }
    }

    pub fn read_message<F, Msg>(&mut self, decoder: F) -> Option<Msg>
    where
        for<'a> F: Fn(Message<'a>) -> Option<Msg>,
    {
        if self.read_buf.len() < 4 {
            return None;
        }
        let mut buf = [0u8; 16];
        self.read_buf.reader_peek().read_exact(&mut buf).unwrap();
        let obj = u64::from_ne_bytes(buf[0..8].try_into().unwrap());
        let size = u32::from_ne_bytes(buf[8..12].try_into().unwrap());
        let op = u32::from_ne_bytes(buf[12..16].try_into().unwrap());
        if self.read_buf.len() < usize::try_from(size).unwrap() {
            return None;
        }
        let buf_bytes = self.read_buf.get_bytes_upto_size(size as usize);
        let mut data = SplitSlice(buf_bytes);
        data.advance(16);
        let msg = decoder(Message {
            object: obj,
            opcode: op,
            data,
            fds: &mut self.read_fds,
        })
        .expect("decoder failed!");
        self.read_buf.advance_read_raw(size as usize);
        Some(msg)
    }
}

#[derive(Debug)]
struct SplitSlice<'a>([&'a [u8]; 2]);

impl SplitSlice<'_> {
    fn len(&self) -> usize {
        self.0.iter().map(|x| x.len()).sum()
    }

    fn advance(&mut self, n: usize) {
        let [s0, s1] = &mut self.0;
        if n > s0.len() {
            *s1 = &s1[n - s0.len()..];
        }
        *s0 = &s0[n.min(s0.len())..];
    }
}

impl Read for SplitSlice<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = buf.len().min(self.len());
        let [s0, s1] = &mut self.0;
        buf[..s0.len().min(n)].copy_from_slice(&s0[..n.min(s0.len())]);
        if n > s0.len() {
            buf[s0.len()..].copy_from_slice(&s1[..n - s0.len()]);
        }
        if n > s0.len() {
            *s1 = &s1[n - s0.len()..];
        }
        *s0 = &s0[n.min(s0.len())..];
        Ok(n)
    }
}

#[derive(Debug)]
pub struct Message<'a> {
    object: u64,
    opcode: u32,
    data: SplitSlice<'a>,
    fds: &'a mut VecDeque<OwnedFd>,
}

impl<'a> Message<'a> {
    pub fn read_int32(&mut self) -> Option<i32> {
        self.read_uint32().map(|i| i as i32)
    }

    pub fn read_uint32(&mut self) -> Option<u32> {
        let mut buf = [0u8; 4];
        self.data.read_exact(&mut buf).ok()?;
        Some(u32::from_ne_bytes(buf))
    }

    pub fn read_int64(&mut self) -> Option<i64> {
        self.read_uint64().map(|i| i as i64)
    }

    pub fn read_uint64(&mut self) -> Option<u64> {
        let mut buf = [0u8; 8];
        self.data.read_exact(&mut buf).ok()?;
        Some(u64::from_ne_bytes(buf))
    }

    pub fn read_float(&mut self) -> Option<f32> {
        self.read_uint32().map(f32::from_bits)
    }

    pub fn read_string(&mut self) -> Option<Option<String>> {
        let length = self.read_uint32()?;
        if length == 0 {
            Some(None)
        } else {
            let mut buf = vec![0u8; usize::try_from((length + 3) / 4 * 4).unwrap()];
            self.data.read_exact(&mut buf).ok()?;
            buf.truncate(usize::try_from(length - 1).unwrap());
            Some(Some(String::from_utf8(buf).unwrap()))
        }
    }

    pub fn read_array(&mut self) -> Option<Vec<u8>> {
        let length = self.read_uint32()?;
        let mut buf = vec![0u8; usize::try_from(length / 4 * 4).unwrap()];
        self.data.read_exact(&mut buf).ok()?;
        buf.truncate(usize::try_from(length).unwrap());
        Some(buf)
    }

    pub fn read_fd(&mut self) -> Option<OwnedFd> {
        self.fds.pop_back()
    }

    pub fn object(&self) -> u64 {
        self.object
    }

    pub fn opcode(&self) -> u32 {
        self.opcode
    }
}

pub trait Object<I>: Debug + Copy {
    const INTERFACE: I;
    type Request<'a>: Debug;
    type Event<'a>: Debug;
    fn new(id: u64) -> Self;
    fn id(self) -> u64;
}

#[derive(Debug, Clone, Copy)]
pub enum Arg<'a> {
    Int32(i32),
    Uint32(u32),
    Int64(i64),
    Uint64(u64),
    Float(f32),
    Array(&'a [u8]),
    String(Option<&'a str>),
}
