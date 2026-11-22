// SPDX-License-Identifier: MIT
// Linux kernel receive timestamps keep capture-loop scheduling out of the ACK measurement.
use std::io;
use std::net::UdpSocket;
use std::os::fd::AsRawFd;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(super) fn enable(sock: &UdpSocket) -> io::Result<()> {
    let on: libc::c_int = 1;
    let ret = unsafe {
        libc::setsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_TIMESTAMPNS,
            (&on as *const libc::c_int).cast(),
            size_of_val(&on) as _,
        )
    };
    if ret == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(super) fn receive(sock: &UdpSocket, buf: &mut [u8]) -> io::Result<(usize, Option<SystemTime>)> {
    let mut control = [0usize; 16];
    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr().cast(),
        iov_len: buf.len(),
    };
    // These buffers outlive recvmsg and the aligned ancillary-data traversal below.
    unsafe {
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = size_of_val(&control);
        let n = libc::recvmsg(sock.as_raw_fd(), &mut msg, 0);
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut c = libc::CMSG_FIRSTHDR(&msg);
        while !c.is_null() {
            if (*c).cmsg_level == libc::SOL_SOCKET
                && (*c).cmsg_type == libc::SCM_TIMESTAMPNS
                && (*c).cmsg_len >= libc::CMSG_LEN(size_of::<libc::timespec>() as _) as usize
            {
                let t = libc::CMSG_DATA(c).cast::<libc::timespec>().read_unaligned();
                if t.tv_sec >= 0 && (0..1_000_000_000).contains(&t.tv_nsec) {
                    return Ok((
                        n as usize,
                        Some(UNIX_EPOCH + Duration::new(t.tv_sec as u64, t.tv_nsec as u32)),
                    ));
                }
            }
            c = libc::CMSG_NXTHDR(&msg, c);
        }
        Ok((n as usize, None))
    }
}

pub(super) struct SentTiles {
    reports: std::collections::VecDeque<(u32, u32, SystemTime)>,
    slots: Vec<Option<(u16, SystemTime)>>,
}

impl SentTiles {
    pub(super) fn new() -> Self {
        Self {
            slots: vec![None; 4096],
            reports: Default::default(),
        }
    }
    pub(super) fn remember_report(&mut self, ssrc: u32, lsr: u32, at: SystemTime) {
        if self.reports.len() == 32 {
            self.reports.pop_front();
        }
        self.reports.push_back((ssrc, lsr, at));
    }
    pub(super) fn report_rtt(&self, ssrc: u32, lsr: u32, dlsr: u32, at: SystemTime) -> Option<f64> {
        if lsr == 0 {
            return None;
        }
        let (_, _, sent) = self
            .reports
            .iter()
            .rev()
            .find(|(s, l, _)| (*s, *l) == (ssrc, lsr))?;
        let elapsed = at.duration_since(*sent).ok()?;
        let peer_delay = Duration::from_secs_f64(f64::from(dlsr) / 65536.0);
        Some(elapsed.checked_sub(peer_delay)?.as_secs_f64() * 1000.0)
    }
    pub(super) fn sent(&mut self, don: u16, time: SystemTime) {
        let index = usize::from(don) % self.slots.len();
        self.slots[index] = Some((don, time));
    }
    pub(super) fn ack(&mut self, don: u16, arrival: SystemTime) -> Option<f64> {
        let index = usize::from(don) % self.slots.len();
        let (stored, sent) = self.slots[index]?;
        if stored != don {
            return None;
        }
        let delay = arrival.duration_since(sent).ok()?;
        self.slots[index] = None;
        Some(delay.as_secs_f64() * 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_arrival_even_if_read_late_and_ignores_duplicate_acks() {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        enable(&sock).unwrap();
        let mut tiles = SentTiles::new();
        tiles.sent(u16::MAX, SystemTime::now());
        sock.send_to(&[7], sock.local_addr().unwrap()).unwrap();
        std::thread::sleep(Duration::from_millis(50));
        let (len, arrival) = receive(&sock, &mut [0; 4]).unwrap();
        let arrival = arrival.unwrap();
        assert_eq!(len, 1);
        assert!(arrival.elapsed().unwrap() >= Duration::from_millis(40));
        assert!(tiles.ack(u16::MAX, arrival).is_some());
        assert!(tiles.ack(u16::MAX, arrival).is_none());
        tiles.sent(0, arrival);
        tiles.sent(4096, arrival);
        assert!(tiles.ack(0, arrival).is_none());
        assert_eq!(tiles.ack(4096, arrival), Some(0.0));
    }
}

#[cfg(test)]
mod report_tests {
    use super::*;
    #[test]
    fn matches_sent_report_and_subtracts_peer_delay() {
        let mut tiles = SentTiles::new();
        tiles.remember_report(2, 7, UNIX_EPOCH);
        let at = UNIX_EPOCH + Duration::from_millis(505);
        assert_eq!(tiles.report_rtt(2, 7, 32768, at), Some(5.0));
        assert_eq!(tiles.report_rtt(1, 7, 32768, at), None);
        assert_eq!(tiles.report_rtt(2, 8, 32768, at), None);
        assert_eq!(tiles.report_rtt(2, 7, 65536, at), None);
    }
}
