//! [`DmxReceiver`] — background receive thread for sACN and Art-Net input.
//!
//! The RX mirror of [`crate::DmxSender`]: one thread polls the protocol
//! sockets, parses datagrams via [`crate::e131::parse_sacn`] /
//! [`crate::artnet::parse_artdmx`], and pushes [`RxPacket`]s into a bounded
//! channel the consumer drains at its leisure. Change detection, merge, and
//! recording live upstream — this module only gets universes off the wire.
//!
//! sACN unicast and broadcast arrive with no configuration; multicast
//! universes must be joined explicitly ([`DmxReceiver::join_sacn_multicast`])
//! since joining all 512 possible groups up front would be IGMP noise.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam::channel::{bounded, Receiver, Sender, TryRecvError};

use crate::dmx::{Universe, DMX_UNIVERSE_SIZE};
use crate::{artnet, e131, socket};

/// Which wire protocol a packet arrived on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RxProtocol {
    Sacn,
    ArtNet,
}

/// One received universe of DMX.
#[derive(Debug, Clone)]
pub struct RxPacket {
    pub protocol: RxProtocol,
    pub universe: u16,
    /// sACN per-universe priority; Art-Net has none, reported as 100.
    pub priority: u8,
    pub data: Universe,
}

/// Receiver configuration. `Default` listens for both protocols on their
/// standard ports.
#[derive(Debug, Clone)]
pub struct RxConfig {
    pub sacn: bool,
    pub artnet: bool,
    /// Port overrides for loopback tests; 0 binds an ephemeral port.
    pub sacn_port: u16,
    pub artnet_port: u16,
    /// sACN universes whose multicast groups to join at spawn.
    pub sacn_multicast: Vec<u16>,
}

impl Default for RxConfig {
    fn default() -> Self {
        Self {
            sacn: true,
            artnet: true,
            sacn_port: e131::SACN_PORT,
            artnet_port: artnet::ARTNET_PORT,
            sacn_multicast: Vec::new(),
        }
    }
}

pub struct DmxReceiver {
    packets: Receiver<RxPacket>,
    /// Clone of the sACN socket kept for post-spawn multicast joins.
    sacn_socket: Option<UdpSocket>,
    sacn_port: u16,
    artnet_port: u16,
    shutdown: Sender<()>,
    handle: Option<JoinHandle<()>>,
}

impl DmxReceiver {
    pub fn spawn(config: RxConfig) -> std::io::Result<Self> {
        let bind = |port| socket::rx_socket(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), port));
        let sacn = if config.sacn { Some(bind(config.sacn_port)?) } else { None };
        let art = if config.artnet { Some(bind(config.artnet_port)?) } else { None };

        let sacn_port = sacn.as_ref().map(|s| s.local_addr().unwrap().port()).unwrap_or(0);
        let artnet_port = art.as_ref().map(|s| s.local_addr().unwrap().port()).unwrap_or(0);

        let sacn_clone = sacn.as_ref().map(|s| s.try_clone()).transpose()?;
        if let Some(s) = &sacn {
            for &u in &config.sacn_multicast {
                join_multicast(s, u);
            }
        }

        // ~8.5 MB of backlog at 512 bytes/packet; the poll loop drops (with a
        // warn) rather than blocking the socket drain if the consumer stalls.
        let (pkt_tx, pkt_rx) = bounded::<RxPacket>(16 * 1024);
        let (sd_tx, sd_rx) = bounded::<()>(1);

        let handle = std::thread::spawn(move || {
            let mut buf = [0u8; 1024];
            let mut dropped: u64 = 0;
            loop {
                match sd_rx.try_recv() {
                    Ok(()) | Err(TryRecvError::Disconnected) => break,
                    Err(TryRecvError::Empty) => {}
                }
                let mut idle = true;
                for sock in [&sacn, &art].into_iter().flatten() {
                    // Drain everything queued on this socket before sleeping.
                    while let Ok((n, _)) = sock.recv_from(&mut buf) {
                        idle = false;
                        if let Some(pkt) = parse_packet(&buf[..n])
                            && pkt_tx.try_send(pkt).is_err()
                        {
                            dropped += 1;
                            if dropped.is_power_of_two() {
                                log::warn!("DmxReceiver: consumer stalled, {dropped} packets dropped");
                            }
                        }
                    }
                }
                if idle {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        });

        Ok(Self {
            packets: pkt_rx,
            sacn_socket: sacn_clone,
            sacn_port,
            artnet_port,
            shutdown: sd_tx,
            handle: Some(handle),
        })
    }

    /// Channel of received packets — drain with `try_iter()` per tick or
    /// `recv_timeout` on a dedicated thread.
    pub fn packets(&self) -> &Receiver<RxPacket> {
        &self.packets
    }

    /// Join the multicast group for an sACN universe (`239.255.hi.lo`).
    /// No-op if sACN is disabled; joining an already-joined group only warns.
    pub fn join_sacn_multicast(&self, universe: u16) {
        if let Some(s) = &self.sacn_socket {
            join_multicast(s, universe);
        }
    }

    /// Actual bound ports (differs from config when a port override was 0).
    pub fn ports(&self) -> (u16, u16) {
        (self.sacn_port, self.artnet_port)
    }

    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        let _ = self.shutdown.send(());
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for DmxReceiver {
    fn drop(&mut self) {
        self.stop();
    }
}

fn join_multicast(sock: &UdpSocket, universe: u16) {
    let group = e131::multicast_addr(universe);
    if let Err(e) = sock.join_multicast_v4(&group, &Ipv4Addr::UNSPECIFIED) {
        log::warn!("sACN multicast join {group} (universe {universe}) failed: {e}");
    }
}

/// Try both protocol parsers on a datagram; the magic bytes disambiguate.
fn parse_packet(buf: &[u8]) -> Option<RxPacket> {
    if let Some((universe, priority, data)) = e131::parse_sacn(buf) {
        return Some(RxPacket {
            protocol: RxProtocol::Sacn,
            universe,
            priority,
            data: pad(data),
        });
    }
    if let Some((universe, _seq, data)) = artnet::parse_artdmx(buf) {
        return Some(RxPacket {
            protocol: RxProtocol::ArtNet,
            universe,
            priority: 100,
            data: pad(data),
        });
    }
    None
}

/// Widen a possibly short on-wire slot list to a full 512-slot universe.
fn pad(data: &[u8]) -> Universe {
    let mut u = [0u8; DMX_UNIVERSE_SIZE];
    let n = data.len().min(DMX_UNIVERSE_SIZE);
    u[..n].copy_from_slice(&data[..n]);
    u
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dmx::DmxFrame;
    use crate::transport::{ArtNetTransport, Dest, DmxTransport, SacnTransport};

    fn recv_packet(rx: &DmxReceiver) -> RxPacket {
        rx.packets()
            .recv_timeout(Duration::from_secs(2))
            .expect("packet should arrive on loopback")
    }

    /// Existing sACN TX → DmxReceiver on an ephemeral port.
    #[test]
    fn sacn_tx_to_rx_loopback() {
        let rx = DmxReceiver::spawn(RxConfig {
            artnet: false,
            sacn_port: 0,
            ..Default::default()
        })
        .unwrap();
        let (sacn_port, _) = rx.ports();

        let mut tx = SacnTransport::new(Dest::Unicast(Ipv4Addr::LOCALHOST), 120, "rec-test")
            .unwrap()
            .with_dest_port(sacn_port);

        let mut frame = DmxFrame::new();
        let u = frame.universe_mut(7);
        u[0] = 11;
        u[511] = 99;
        tx.send(&frame);

        let pkt = recv_packet(&rx);
        assert_eq!(pkt.protocol, RxProtocol::Sacn);
        assert_eq!(pkt.universe, 7);
        assert_eq!(pkt.priority, 120);
        assert_eq!(pkt.data[0], 11);
        assert_eq!(pkt.data[511], 99);
        rx.shutdown();
    }

    /// Existing Art-Net TX → DmxReceiver on an ephemeral port.
    #[test]
    fn artnet_tx_to_rx_loopback() {
        let rx = DmxReceiver::spawn(RxConfig {
            sacn: false,
            artnet_port: 0,
            ..Default::default()
        })
        .unwrap();
        let (_, artnet_port) = rx.ports();

        let mut tx = ArtNetTransport::new(Dest::Unicast(Ipv4Addr::LOCALHOST))
            .unwrap()
            .with_dest_port(artnet_port);

        let mut frame = DmxFrame::new();
        frame.universe_mut(3)[5] = 42;
        tx.send(&frame);

        let pkt = recv_packet(&rx);
        assert_eq!(pkt.protocol, RxProtocol::ArtNet);
        assert_eq!(pkt.universe, 3);
        assert_eq!(pkt.priority, 100, "Art-Net reports default priority");
        assert_eq!(pkt.data[5], 42);
        rx.shutdown();
    }

    /// Both sockets live in one receiver; each protocol lands with its tag.
    #[test]
    fn dual_protocol_receiver() {
        let rx = DmxReceiver::spawn(RxConfig {
            sacn_port: 0,
            artnet_port: 0,
            ..Default::default()
        })
        .unwrap();
        let (sacn_port, artnet_port) = rx.ports();

        let mut sacn_tx = SacnTransport::new(Dest::Unicast(Ipv4Addr::LOCALHOST), 100, "x")
            .unwrap()
            .with_dest_port(sacn_port);
        let mut art_tx = ArtNetTransport::new(Dest::Unicast(Ipv4Addr::LOCALHOST))
            .unwrap()
            .with_dest_port(artnet_port);

        let mut frame = DmxFrame::new();
        frame.universe_mut(1)[0] = 1;
        sacn_tx.send(&frame);
        art_tx.send(&frame);

        let mut protocols = vec![recv_packet(&rx).protocol, recv_packet(&rx).protocol];
        protocols.sort_by_key(|p| *p == RxProtocol::ArtNet);
        assert_eq!(protocols, vec![RxProtocol::Sacn, RxProtocol::ArtNet]);
        rx.shutdown();
    }

    /// End-to-end phase 1: TX → RX → change-detect → .dmxrec → read back.
    #[test]
    fn capture_to_dmxrec_roundtrip() {
        let rx = DmxReceiver::spawn(RxConfig {
            artnet: false,
            sacn_port: 0,
            ..Default::default()
        })
        .unwrap();
        let mut tx = SacnTransport::new(Dest::Unicast(Ipv4Addr::LOCALHOST), 100, "x")
            .unwrap()
            .with_dest_port(rx.ports().0);

        // Two frames: ch0 ramps 10 → 20, ch1 stays put.
        for v in [10u8, 20] {
            let mut frame = DmxFrame::new();
            let u = frame.universe_mut(1);
            u[0] = v;
            u[1] = 7;
            tx.send(&frame);
            // Recorder-style change detection against last seen state.
            std::thread::sleep(Duration::from_millis(10));
        }

        let path = std::env::temp_dir()
            .join(format!("rustjay-rx-e2e-{}.dmxrec", std::process::id()));
        let mut w = crate::rec::RecWriter::create(&path).unwrap();
        // Recorder-style change detection: baseline is implicit zeros, so a
        // fresh universe only records its nonzero channels (keeps files sparse).
        let mut last: Universe = [0; DMX_UNIVERSE_SIZE];
        let mut t_ms = 0u32;
        for _ in 0..2 {
            let pkt = recv_packet(&rx);
            for (ch, &val) in pkt.data.iter().enumerate() {
                if last[ch] != val {
                    w.write(crate::rec::RecEvent {
                        t_ms,
                        universe: pkt.universe,
                        channel: ch as u16,
                        value: val,
                    })
                    .unwrap();
                }
            }
            last = pkt.data;
            t_ms += 16;
        }
        w.finish().unwrap();
        rx.shutdown();

        let events = crate::rec::read_rec(&path).unwrap();
        // Frame 1: ch0=10, ch1=7 (all changes from nothing). Frame 2: ch0=20 only.
        assert_eq!(events.len(), 3);
        assert_eq!((events[0].channel, events[0].value), (0, 10));
        assert_eq!((events[1].channel, events[1].value), (1, 7));
        assert_eq!((events[2].channel, events[2].value, events[2].t_ms), (0, 20, 16));
        std::fs::remove_file(&path).ok();
    }
}
