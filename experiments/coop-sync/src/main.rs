use std::collections::HashMap;
use std::env;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::thread;
use std::time::{Duration, Instant};

use coop_sync_experiment::{
    sequence_is_newer, Packet, PlayerId, PlayerInput, PlayerPose, MAX_PACKET_BYTES, MAX_PLAYERS,
};

const DEMO_SCENE: u64 = 0;
const TICK: Duration = Duration::from_millis(50);
const HELLO_INTERVAL: Duration = Duration::from_millis(500);
const PEER_TIMEOUT: Duration = Duration::from_millis(1500);
const SPEED_UNITS_PER_SECOND: f32 = 180.0;

struct Peer {
    player: PlayerPose,
    last_input_sequence: Option<u32>,
    input: [f32; 2],
    last_seen: Instant,
}

fn usage() -> ! {
    eprintln!(
        "Usage:\n  coop-sync-experiment host [bind-address]\n  coop-sync-experiment client <server-address> <x-input> <y-input> <seconds>"
    );
    std::process::exit(2);
}

fn parse_addr(value: Option<String>, fallback: &str) -> SocketAddr {
    value
        .unwrap_or_else(|| fallback.to_owned())
        .parse()
        .unwrap_or_else(|_| usage())
}

fn send(socket: &UdpSocket, packet: &Packet, address: SocketAddr) -> io::Result<()> {
    let bytes = packet
        .encode()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    socket.send_to(&bytes, address).map(|_| ())
}

fn add_peer(peers: &mut HashMap<SocketAddr, Peer>, address: SocketAddr) -> Option<PlayerId> {
    if let Some(peer) = peers.get(&address) {
        return Some(peer.player.id);
    }
    if peers.len() >= MAX_PLAYERS - 1 {
        return None;
    }
    let id = (1..MAX_PLAYERS as u8)
        .map(PlayerId)
        .find(|candidate| peers.values().all(|peer| peer.player.id != *candidate))?;
    peers.insert(
        address,
        Peer {
            player: PlayerPose {
                id,
                position: [0.0; 3],
                heading: 0.0,
            },
            last_input_sequence: None,
            input: [0.0; 2],
            last_seen: Instant::now(),
        },
    );
    Some(id)
}

fn run_host(bind: SocketAddr) -> io::Result<()> {
    let socket = UdpSocket::bind(bind)?;
    socket.set_nonblocking(true)?;
    println!(
        "Host listening on {} (50 ms snapshots, at most 3 clients)",
        socket.local_addr()?
    );

    let mut peers = HashMap::<SocketAddr, Peer>::new();
    let mut tick = 0u32;
    let mut buffer = [0u8; MAX_PACKET_BYTES];
    let mut next_tick = Instant::now();

    loop {
        loop {
            match socket.recv_from(&mut buffer) {
                Ok((length, address)) => {
                    let Ok(packet) = Packet::decode(&buffer[..length]) else {
                        continue;
                    };
                    match packet {
                        Packet::Hello { scene_key } => {
                            if scene_key != DEMO_SCENE {
                                continue;
                            }
                            let was_present = peers.contains_key(&address);
                            let Some(id) = add_peer(&mut peers, address) else {
                                continue;
                            };
                            if let Some(peer) = peers.get_mut(&address) {
                                peer.last_seen = Instant::now();
                            }
                            if !was_present {
                                println!("Player {} joined from {}", id.0, address);
                            }
                            send(
                                &socket,
                                &Packet::Welcome {
                                    player: id,
                                    scene_key: DEMO_SCENE,
                                },
                                address,
                            )?;
                        }
                        Packet::Input(input) => {
                            let Some(peer) = peers.get_mut(&address) else {
                                continue;
                            };
                            peer.last_seen = Instant::now();
                            let fresh = peer.last_input_sequence.map_or(true, |previous| {
                                sequence_is_newer(input.sequence, previous)
                            });
                            if fresh {
                                peer.last_input_sequence = Some(input.sequence);
                                let length = (input.axes[0].powi(2) + input.axes[1].powi(2))
                                    .sqrt()
                                    .max(1.0);
                                peer.input = [input.axes[0] / length, input.axes[1] / length];
                                peer.player.heading = input.heading;
                            }
                        }
                        Packet::Welcome { .. }
                        | Packet::Snapshot { .. }
                        | Packet::CampaignUpdate { .. }
                        | Packet::WorldUpdate { .. }
                        | Packet::VatsAim { .. }
                        | Packet::VatsAimResult { .. }
                        | Packet::VatsAttack { .. }
                        | Packet::VatsAttackResult { .. }
                        | Packet::WorldKeyframeRequest { .. }
                        | Packet::WorldKeyframeChunk { .. }
                        | Packet::WorldKeyframeAck { .. }
                        | Packet::DialogueUpdate { .. }
                        | Packet::DialogueChoice { .. }
                        | Packet::DialogueChoiceResult { .. }
                        | Packet::DialogueListen { .. }
                        | Packet::CampaignSync { .. }
                        | Packet::CampaignSyncAck { .. }
                        | Packet::PlayerAttack { .. }
                        | Packet::PlayerAttackResult { .. }
                        | Packet::PlayerInventory { .. }
                        | Packet::PlayerInventoryAck { .. }
                        | Packet::PlayerProfile { .. }
                        | Packet::PlayerProfileAck { .. } => {}
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
        }

        let now = Instant::now();
        peers.retain(|address, peer| {
            let active = now.duration_since(peer.last_seen) <= PEER_TIMEOUT;
            if !active {
                println!("Player {} disconnected ({})", peer.player.id.0, address);
            }
            active
        });

        if now >= next_tick {
            tick = tick.wrapping_add(1);
            for peer in peers.values_mut() {
                peer.player.position[0] +=
                    peer.input[0] * SPEED_UNITS_PER_SECOND * TICK.as_secs_f32();
                peer.player.position[1] +=
                    peer.input[1] * SPEED_UNITS_PER_SECOND * TICK.as_secs_f32();
            }
            let mut players = Vec::with_capacity(peers.len() + 1);
            players.push(PlayerPose {
                id: PlayerId(0),
                position: [0.0; 3],
                heading: 0.0,
            });
            players.extend(peers.values().map(|peer| peer.player));
            players.sort_by_key(|player| player.id);
            for (address, peer) in &peers {
                send(
                    &socket,
                    &Packet::Snapshot {
                        tick,
                        assigned_player: peer.player.id,
                        players: players.clone(),
                        world_revision: 0,
                        campaign_revision: 0,
                        world_update_revision: 0,
                        world_events: Vec::new(),
                    },
                    *address,
                )?;
            }
            next_tick = now + TICK;
        } else {
            thread::sleep(next_tick - now);
        }
    }
}

fn run_client(server: SocketAddr, input_x: f32, input_y: f32, seconds: f32) -> io::Result<()> {
    if !input_x.is_finite()
        || !input_y.is_finite()
        || input_x.abs() > 1.0
        || input_y.abs() > 1.0
        || !seconds.is_finite()
        || seconds <= 0.0
    {
        usage();
    }
    let socket = UdpSocket::bind("0.0.0.0:0")?;
    socket.set_nonblocking(true)?;
    let started = Instant::now();
    let mut next_send = started;
    let mut next_hello = started;
    let mut sequence = 0u32;
    let mut assigned_player = None;
    let mut last_printed_tick = None;
    let mut buffer = [0u8; MAX_PACKET_BYTES];
    println!("Sending input to {server}; client stops after {seconds:.1}s");

    while started.elapsed() < Duration::from_secs_f32(seconds) {
        let now = Instant::now();
        if now >= next_hello {
            send(
                &socket,
                &Packet::Hello {
                    scene_key: DEMO_SCENE,
                },
                server,
            )?;
            next_hello = now + HELLO_INTERVAL;
        }
        if now >= next_send {
            sequence = sequence.wrapping_add(1);
            send(
                &socket,
                &Packet::Input(PlayerInput {
                    sequence,
                    axes: [input_x, input_y],
                    heading: 0.0,
                    pitch: 0.0,
                    view_aspect: 16.0 / 9.0,
                    running: false,
                    sneaking: false,
                }),
                server,
            )?;
            next_send = now + TICK;
        }

        match socket.recv_from(&mut buffer) {
            Ok((length, _)) => match Packet::decode(&buffer[..length]) {
                Ok(Packet::Welcome { player, scene_key }) if scene_key == DEMO_SCENE => {
                    assigned_player = Some(player);
                }
                Ok(Packet::Snapshot {
                    tick,
                    assigned_player: id,
                    players,
                    ..
                }) => {
                    assigned_player = Some(id);
                    if last_printed_tick != Some(tick) {
                        let summary = players
                            .iter()
                            .map(|player| {
                                format!(
                                    "P{}=({:.1}, {:.1}, {:.1})",
                                    player.id.0,
                                    player.position[0],
                                    player.position[1],
                                    player.position[2]
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(" ");
                        println!("tick {tick}: {summary}");
                        last_printed_tick = Some(tick);
                    }
                }
                _ => {}
            },
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(2));
            }
            Err(error) => return Err(error),
        }
    }
    if let Some(player) = assigned_player {
        println!("Session player ID: {}", player.0);
    } else {
        println!("No host response received.");
    }
    Ok(())
}

fn parse_f32(value: Option<String>) -> f32 {
    value
        .and_then(|text| text.parse().ok())
        .unwrap_or_else(|| usage())
}

fn main() -> io::Result<()> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("host") => run_host(parse_addr(args.next(), "0.0.0.0:39000")),
        Some("client") => {
            let server = parse_addr(args.next(), "");
            let input_x = parse_f32(args.next());
            let input_y = parse_f32(args.next());
            let seconds = parse_f32(args.next());
            if args.next().is_some() {
                usage();
            }
            run_client(server, input_x, input_y, seconds)
        }
        _ => usage(),
    }
}
