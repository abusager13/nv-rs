//! Small versioned wire protocol shared by the co-op experiment and viewer.
//!
//! This protocol is still experimental. It carries player input, pose
//! snapshots, a bounded replay of shared death/destruction/health changes,
//! chunked world keyframes for recovery, and owner-scoped campaign changes sent
//! to the host. It does not authenticate peers.

use std::collections::HashSet;
use std::fmt;

const MAGIC: &[u8; 4] = b"NVCE";
const VERSION: u8 = 10;
const HELLO: u8 = 1;
const INPUT: u8 = 2;
const WELCOME: u8 = 3;
const SNAPSHOT: u8 = 4;
const CAMPAIGN_UPDATE: u8 = 5;
const VATS_AIM: u8 = 6;
const VATS_AIM_RESULT: u8 = 7;
const VATS_ATTACK: u8 = 8;
const VATS_ATTACK_RESULT: u8 = 9;
const WORLD_KEYFRAME_REQUEST: u8 = 10;
const WORLD_KEYFRAME_CHUNK: u8 = 11;
const WORLD_KEYFRAME_ACK: u8 = 12;
const DIALOGUE_UPDATE: u8 = 13;
const DIALOGUE_LISTEN: u8 = 14;
const WORLD_UPDATE: u8 = 15;
pub const MAX_PLAYERS: usize = 4;
pub const MAX_PACKET_BYTES: usize = 1200;
/// Camera viewport aspect ratios accepted on the wire for aim validation.
pub const MIN_VIEW_ASPECT: f32 = 0.5;
pub const MAX_VIEW_ASPECT: f32 = 4.0;
/// Recent world changes repeated in snapshots so a short UDP loss does not
/// permanently lose a shared mutation.
pub const MAX_WORLD_EVENTS: usize = 80;
/// Guest world changes retained until the host acknowledges their revision.
pub const MAX_WORLD_UPDATE_EVENTS: usize = 64;
/// Entity flags per reliable, retransmitted world-keyframe chunk.
pub const MAX_WORLD_KEYFRAME_ENTRIES: usize = 130;
/// Bound the memory a malformed keyframe declaration can reserve.
pub const MAX_WORLD_KEYFRAME_CHUNKS: usize = 4096;
/// Text bounds keep the complete dialogue view under one UDP datagram.
pub const MAX_DIALOGUE_NAME_BYTES: usize = 64;
pub const MAX_DIALOGUE_TEXT_BYTES: usize = 700;
pub const MAX_DIALOGUE_TOPICS: usize = 8;
pub const MAX_DIALOGUE_TOPIC_BYTES: usize = 48;
/// Private campaign changes sent from each player to the host.
pub const MAX_CAMPAIGN_EVENTS: usize = 64;

/// A session-assigned player identity. Zero is reserved for the host.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PlayerId(pub u8);

/// A player's host-authoritative pose in Fallout game coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerPose {
    pub id: PlayerId,
    pub position: [f32; 3],
    /// Radians clockwise from north.
    pub heading: f32,
}

/// Latest movement intent from a client. Axes are in game-world X/Y space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayerInput {
    pub sequence: u32,
    pub axes: [f32; 2],
    /// Radians clockwise from north.
    pub heading: f32,
    /// Camera pitch in radians, positive upward.
    pub pitch: f32,
    /// Logical viewport width divided by height, clamped to the supported range.
    pub view_aspect: f32,
    pub running: bool,
    pub sneaking: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorldChange {
    ActorDied { reference: u32 },
    ActorRevived { reference: u32 },
    ActorHealth { reference: u32, health_milli: i32 },
    ReferenceDestroyed { reference: u32 },
    ReferenceRestored { reference: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorldEvent {
    pub revision: u32,
    pub change: WorldChange,
}

/// One current actor/reference entry in an authoritative world keyframe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorldStateEntry {
    pub reference: u32,
    pub dead: bool,
    pub destroyed: bool,
    /// Current actor health in thousandths, when this entry is an actor.
    pub health_milli: Option<i32>,
}

/// Read-only conversation details replicated to nearby players.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DialogueSnapshot {
    pub speaker: u32,
    pub speaker_name: String,
    /// Current spoken line, or empty while the speaker offers topics.
    pub text: String,
    /// Offered topics while the speaker waits for a choice; listeners cannot
    /// select them.
    pub topics: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CampaignChange {
    QuestStage {
        quest: u32,
        stage: u16,
    },
    QuestStageDone {
        quest: u32,
        stage: u16,
        done: bool,
    },
    QuestObjective {
        quest: u32,
        objective: i32,
        completed: bool,
    },
    QuestStatus {
        quest: u32,
        running: bool,
        completed: bool,
        failed: bool,
    },
    Reputation {
        reputation: u32,
        fame: f32,
        infamy: f32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CampaignEvent {
    pub revision: u32,
    pub change: CampaignChange,
}

/// Versioned messages used by the host and clients.
#[derive(Clone, Debug, PartialEq)]
pub enum Packet {
    /// Initial session and loaded-cell handshake. `scene_key` is a stable
    /// application-provided identity for the starting cell/worldspace.
    Hello {
        scene_key: u64,
    },
    Input(PlayerInput),
    Welcome {
        player: PlayerId,
        scene_key: u64,
    },
    Snapshot {
        tick: u32,
        assigned_player: PlayerId,
        players: Vec<PlayerPose>,
        world_revision: u32,
        /// Highest private campaign event the host has recorded for this peer.
        campaign_revision: u32,
        /// Highest contiguous guest world event the host has consumed.
        world_update_revision: u32,
        world_events: Vec<WorldEvent>,
    },
    /// Ask the host for a complete shared-world baseline after a replay gap.
    WorldKeyframeRequest {
        transfer_id: u32,
        after_revision: u32,
    },
    /// Part of an authoritative world baseline. The host repeats chunks until
    /// it receives an acknowledgement.
    WorldKeyframeChunk {
        transfer_id: u32,
        revision: u32,
        index: u16,
        count: u16,
        entries: Vec<WorldStateEntry>,
    },
    WorldKeyframeAck {
        transfer_id: u32,
    },
    /// A client reports its current local dialogue state to the host.
    DialogueUpdate {
        sequence: u32,
        dialogue: Option<DialogueSnapshot>,
    },
    /// The host sends one nearby speaker's read-only dialogue state to a
    /// listener. `player` identifies the speaker's player, not the NPC.
    DialogueListen {
        player: PlayerId,
        sequence: u32,
        dialogue: Option<DialogueSnapshot>,
    },
    /// Owner-scoped quest and faction state. The host records it under the
    /// sender's assigned player ID and does not broadcast it to other peers.
    CampaignUpdate {
        events: Vec<CampaignEvent>,
    },
    /// Guest observations of shared-world changes. The host acknowledges an
    /// event only when its own authoritative state already has that value; it
    /// never applies a guest's claimed world mutation.
    WorldUpdate {
        events: Vec<WorldEvent>,
    },
    /// Ask the host to validate that this target is currently eligible for
    /// V.A.T.S. (`target == 0` is not a valid request).
    VatsAim {
        sequence: u32,
        target: u32,
    },
    /// The host's answer to a V.A.T.S. target eligibility request.
    VatsAimResult {
        sequence: u32,
        target: u32,
        accepted: bool,
    },
    /// Ask the host to validate and reserve one queued V.A.T.S. attack, then
    /// roll its hit/miss from the reported effective hit probability.
    VatsAttack {
        sequence: u32,
        attack_id: u64,
        shot_index: u16,
        target: u32,
        weapon: u32,
        kind: u8,
        reload: bool,
        slot: i8,
        part_av: i32,
        action_points: f32,
        hit_chance: f32,
    },
    /// Idempotent answer for a V.A.T.S. AP reservation and host hit roll.
    VatsAttackResult {
        sequence: u32,
        attack_id: u64,
        shot_index: u16,
        accepted: bool,
        action_points: f32,
        hit: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodeError {
    TooShort,
    BadMagic,
    UnsupportedVersion(u8),
    UnknownKind(u8),
    WrongLength,
    TooManyPlayers,
    TooManyWorldEvents,
    TooManyWorldUpdateEvents,
    TooManyCampaignEvents,
    DuplicatePlayerId,
    AssignedPlayerMissing,
    InvalidNumber,
    InvalidFlags,
    InvalidPlayerId,
    InvalidTarget,
    UnknownWorldChange(u8),
    UnknownCampaignChange(u8),
    InvalidWorldKeyframe,
    TooManyWorldKeyframeEntries,
    InvalidDialogue,
    TooManyDialogueTopics,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort => f.write_str("packet is too short"),
            Self::BadMagic => f.write_str("packet magic does not match"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported protocol version {version}")
            }
            Self::UnknownKind(kind) => write!(f, "unknown packet kind {kind}"),
            Self::WrongLength => f.write_str("packet has an invalid length"),
            Self::TooManyPlayers => f.write_str("snapshot exceeds the player limit"),
            Self::TooManyWorldEvents => f.write_str("snapshot exceeds the world event limit"),
            Self::TooManyWorldUpdateEvents => f.write_str("world update exceeds the event limit"),
            Self::TooManyCampaignEvents => f.write_str("campaign update exceeds the event limit"),
            Self::DuplicatePlayerId => f.write_str("snapshot repeats a player ID"),
            Self::AssignedPlayerMissing => {
                f.write_str("snapshot does not include its assigned player")
            }
            Self::InvalidNumber => f.write_str("packet contains a non-finite number"),
            Self::InvalidFlags => f.write_str("packet contains invalid flag bits or boolean"),
            Self::InvalidPlayerId => f.write_str("packet contains an invalid player ID"),
            Self::InvalidTarget => f.write_str("V.A.T.S. packet contains an invalid target"),
            Self::UnknownWorldChange(kind) => {
                write!(f, "snapshot contains unknown world change {kind}")
            }
            Self::UnknownCampaignChange(kind) => {
                write!(f, "campaign update contains unknown change {kind}")
            }
            Self::InvalidWorldKeyframe => f.write_str("world keyframe chunk is invalid"),
            Self::TooManyWorldKeyframeEntries => {
                f.write_str("world keyframe chunk exceeds its entry limit")
            }
            Self::InvalidDialogue => f.write_str("dialogue packet contains invalid data"),
            Self::TooManyDialogueTopics => f.write_str("dialogue packet contains too many topics"),
        }
    }
}

impl std::error::Error for DecodeError {}

fn encode_world_event(out: &mut Vec<u8>, event: WorldEvent) {
    out.extend_from_slice(&event.revision.to_be_bytes());
    match event.change {
        WorldChange::ActorDied { reference } => {
            out.push(1);
            out.extend_from_slice(&reference.to_be_bytes());
        }
        WorldChange::ActorRevived { reference } => {
            out.push(2);
            out.extend_from_slice(&reference.to_be_bytes());
        }
        WorldChange::ReferenceDestroyed { reference } => {
            out.push(3);
            out.extend_from_slice(&reference.to_be_bytes());
        }
        WorldChange::ReferenceRestored { reference } => {
            out.push(4);
            out.extend_from_slice(&reference.to_be_bytes());
        }
        WorldChange::ActorHealth {
            reference,
            health_milli,
        } => {
            out.push(5);
            out.extend_from_slice(&reference.to_be_bytes());
            out.extend_from_slice(&health_milli.to_be_bytes());
        }
    }
}

fn decode_world_event(bytes: &[u8]) -> Result<(WorldEvent, usize), DecodeError> {
    if bytes.len() < 9 {
        return Err(DecodeError::TooShort);
    }
    let revision = u32::from_be_bytes(bytes[..4].try_into().unwrap());
    let reference = u32::from_be_bytes(bytes[5..9].try_into().unwrap());
    let change = match bytes[4] {
        1 => WorldChange::ActorDied { reference },
        2 => WorldChange::ActorRevived { reference },
        3 => WorldChange::ReferenceDestroyed { reference },
        4 => WorldChange::ReferenceRestored { reference },
        5 => {
            if bytes.len() < 13 {
                return Err(DecodeError::TooShort);
            }
            let health_milli = i32::from_be_bytes(bytes[9..13].try_into().unwrap());
            if health_milli < 0 {
                return Err(DecodeError::InvalidNumber);
            }
            WorldChange::ActorHealth {
                reference,
                health_milli,
            }
        }
        kind => return Err(DecodeError::UnknownWorldChange(kind)),
    };
    let length = if matches!(change, WorldChange::ActorHealth { .. }) {
        13
    } else {
        9
    };
    Ok((WorldEvent { revision, change }, length))
}

impl Packet {
    pub fn encode(&self) -> Result<Vec<u8>, DecodeError> {
        let mut out = Vec::with_capacity(12 + MAX_PLAYERS * 17);
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        match self {
            Self::Hello { scene_key } => {
                out.push(HELLO);
                out.extend_from_slice(&scene_key.to_be_bytes());
            }
            Self::Input(input) => {
                if !input.axes.iter().all(|v| v.is_finite() && v.abs() <= 1.0)
                    || !input.heading.is_finite()
                    || !input.pitch.is_finite()
                    || input.pitch.abs() > std::f32::consts::FRAC_PI_2
                    || !valid_view_aspect(input.view_aspect)
                {
                    return Err(DecodeError::InvalidNumber);
                }
                out.push(INPUT);
                out.extend_from_slice(&input.sequence.to_be_bytes());
                for value in input.axes {
                    out.extend_from_slice(&value.to_bits().to_be_bytes());
                }
                out.extend_from_slice(&input.heading.to_bits().to_be_bytes());
                out.extend_from_slice(&input.pitch.to_bits().to_be_bytes());
                out.extend_from_slice(&input.view_aspect.to_bits().to_be_bytes());
                out.push(u8::from(input.running) | (u8::from(input.sneaking) << 1));
            }
            Self::Welcome { player, scene_key } => {
                if player.0 == 0 || usize::from(player.0) >= MAX_PLAYERS {
                    return Err(DecodeError::InvalidPlayerId);
                }
                out.push(WELCOME);
                out.push(player.0);
                out.extend_from_slice(&scene_key.to_be_bytes());
            }
            Self::Snapshot {
                tick,
                assigned_player,
                players,
                world_revision,
                campaign_revision,
                world_update_revision,
                world_events,
            } => {
                if players.len() > MAX_PLAYERS {
                    return Err(DecodeError::TooManyPlayers);
                }
                if assigned_player.0 as usize >= MAX_PLAYERS {
                    return Err(DecodeError::InvalidPlayerId);
                }
                let mut ids = HashSet::with_capacity(players.len());
                for player in players {
                    if player.id.0 as usize >= MAX_PLAYERS {
                        return Err(DecodeError::InvalidPlayerId);
                    }
                    if !ids.insert(player.id) {
                        return Err(DecodeError::DuplicatePlayerId);
                    }
                    if !player.position.iter().all(|v| v.is_finite()) || !player.heading.is_finite()
                    {
                        return Err(DecodeError::InvalidNumber);
                    }
                }
                if !ids.contains(assigned_player) {
                    return Err(DecodeError::AssignedPlayerMissing);
                }
                if world_events.len() > MAX_WORLD_EVENTS {
                    return Err(DecodeError::TooManyWorldEvents);
                }
                out.push(SNAPSHOT);
                out.extend_from_slice(&tick.to_be_bytes());
                out.push(assigned_player.0);
                out.push(players.len() as u8);
                for player in players {
                    out.push(player.id.0);
                    for value in player.position {
                        out.extend_from_slice(&value.to_bits().to_be_bytes());
                    }
                    out.extend_from_slice(&player.heading.to_bits().to_be_bytes());
                }
                out.extend_from_slice(&world_revision.to_be_bytes());
                out.extend_from_slice(&campaign_revision.to_be_bytes());
                out.extend_from_slice(&world_update_revision.to_be_bytes());
                out.push(world_events.len() as u8);
                for event in world_events {
                    encode_world_event(&mut out, *event);
                }
            }
            Self::CampaignUpdate { events } => {
                if events.len() > MAX_CAMPAIGN_EVENTS {
                    return Err(DecodeError::TooManyCampaignEvents);
                }
                out.push(CAMPAIGN_UPDATE);
                out.push(events.len() as u8);
                for event in events {
                    out.extend_from_slice(&event.revision.to_be_bytes());
                    match event.change {
                        CampaignChange::QuestStage { quest, stage } => {
                            out.push(1);
                            out.extend_from_slice(&quest.to_be_bytes());
                            out.extend_from_slice(&stage.to_be_bytes());
                        }
                        CampaignChange::QuestStageDone { quest, stage, done } => {
                            out.push(2);
                            out.extend_from_slice(&quest.to_be_bytes());
                            out.extend_from_slice(&stage.to_be_bytes());
                            out.push(u8::from(done));
                        }
                        CampaignChange::QuestObjective {
                            quest,
                            objective,
                            completed,
                        } => {
                            out.push(5);
                            out.extend_from_slice(&quest.to_be_bytes());
                            out.extend_from_slice(&objective.to_be_bytes());
                            out.push(u8::from(completed));
                        }
                        CampaignChange::QuestStatus {
                            quest,
                            running,
                            completed,
                            failed,
                        } => {
                            out.push(3);
                            out.extend_from_slice(&quest.to_be_bytes());
                            out.push(
                                u8::from(running)
                                    | (u8::from(completed) << 1)
                                    | (u8::from(failed) << 2),
                            );
                        }
                        CampaignChange::Reputation {
                            reputation,
                            fame,
                            infamy,
                        } => {
                            if !fame.is_finite() || !infamy.is_finite() {
                                return Err(DecodeError::InvalidNumber);
                            }
                            out.push(4);
                            out.extend_from_slice(&reputation.to_be_bytes());
                            out.extend_from_slice(&fame.to_bits().to_be_bytes());
                            out.extend_from_slice(&infamy.to_bits().to_be_bytes());
                        }
                    }
                }
            }
            Self::WorldUpdate { events } => {
                if events.len() > MAX_WORLD_UPDATE_EVENTS {
                    return Err(DecodeError::TooManyWorldUpdateEvents);
                }
                out.push(WORLD_UPDATE);
                out.push(events.len() as u8);
                for event in events {
                    encode_world_event(&mut out, *event);
                }
            }
            Self::WorldKeyframeRequest {
                transfer_id,
                after_revision,
            } => {
                out.push(WORLD_KEYFRAME_REQUEST);
                out.extend_from_slice(&transfer_id.to_be_bytes());
                out.extend_from_slice(&after_revision.to_be_bytes());
            }
            Self::WorldKeyframeChunk {
                transfer_id,
                revision,
                index,
                count,
                entries,
            } => {
                if entries.len() > MAX_WORLD_KEYFRAME_ENTRIES {
                    return Err(DecodeError::TooManyWorldKeyframeEntries);
                }
                if *count == 0
                    || usize::from(*count) > MAX_WORLD_KEYFRAME_CHUNKS
                    || *index >= *count
                    || entries.iter().any(|entry| {
                        entry.reference == 0
                            || (!entry.dead && !entry.destroyed && entry.health_milli.is_none())
                            || entry.health_milli.is_some_and(|health| health < 0)
                    })
                {
                    return Err(DecodeError::InvalidWorldKeyframe);
                }
                out.push(WORLD_KEYFRAME_CHUNK);
                out.extend_from_slice(&transfer_id.to_be_bytes());
                out.extend_from_slice(&revision.to_be_bytes());
                out.extend_from_slice(&index.to_be_bytes());
                out.extend_from_slice(&count.to_be_bytes());
                out.extend_from_slice(&(entries.len() as u16).to_be_bytes());
                for entry in entries {
                    out.extend_from_slice(&entry.reference.to_be_bytes());
                    let flags = u8::from(entry.dead)
                        | (u8::from(entry.destroyed) << 1)
                        | (u8::from(entry.health_milli.is_some()) << 2);
                    out.push(flags);
                    if let Some(health) = entry.health_milli {
                        out.extend_from_slice(&health.to_be_bytes());
                    }
                }
            }
            Self::WorldKeyframeAck { transfer_id } => {
                out.push(WORLD_KEYFRAME_ACK);
                out.extend_from_slice(&transfer_id.to_be_bytes());
            }
            Self::DialogueUpdate { sequence, dialogue } => {
                out.push(DIALOGUE_UPDATE);
                out.extend_from_slice(&sequence.to_be_bytes());
                encode_dialogue(&mut out, dialogue.as_ref())?;
            }
            Self::DialogueListen {
                player,
                sequence,
                dialogue,
            } => {
                if usize::from(player.0) >= MAX_PLAYERS {
                    return Err(DecodeError::InvalidPlayerId);
                }
                out.push(DIALOGUE_LISTEN);
                out.push(player.0);
                out.extend_from_slice(&sequence.to_be_bytes());
                encode_dialogue(&mut out, dialogue.as_ref())?;
            }
            Self::VatsAim { sequence, target } => {
                if *target == 0 {
                    return Err(DecodeError::InvalidTarget);
                }
                out.push(VATS_AIM);
                out.extend_from_slice(&sequence.to_be_bytes());
                out.extend_from_slice(&target.to_be_bytes());
            }
            Self::VatsAimResult {
                sequence,
                target,
                accepted,
            } => {
                if *target == 0 {
                    return Err(DecodeError::InvalidTarget);
                }
                out.push(VATS_AIM_RESULT);
                out.extend_from_slice(&sequence.to_be_bytes());
                out.extend_from_slice(&target.to_be_bytes());
                out.push(u8::from(*accepted));
            }
            Self::VatsAttack {
                sequence,
                attack_id,
                shot_index,
                target,
                weapon,
                kind,
                reload,
                slot,
                part_av,
                action_points,
                hit_chance,
            } => {
                if *target == 0 {
                    return Err(DecodeError::InvalidTarget);
                }
                if !action_points.is_finite() || *action_points <= 0.0 {
                    return Err(DecodeError::InvalidNumber);
                }
                if !hit_chance.is_finite() || !(0.0..=1.0).contains(hit_chance) {
                    return Err(DecodeError::InvalidNumber);
                }
                out.push(VATS_ATTACK);
                out.extend_from_slice(&sequence.to_be_bytes());
                out.extend_from_slice(&attack_id.to_be_bytes());
                out.extend_from_slice(&shot_index.to_be_bytes());
                out.extend_from_slice(&target.to_be_bytes());
                out.extend_from_slice(&weapon.to_be_bytes());
                out.push(*kind);
                out.push(u8::from(*reload));
                out.push(*slot as u8);
                out.extend_from_slice(&part_av.to_be_bytes());
                out.extend_from_slice(&action_points.to_bits().to_be_bytes());
                out.extend_from_slice(&hit_chance.to_bits().to_be_bytes());
            }
            Self::VatsAttackResult {
                sequence,
                attack_id,
                shot_index,
                accepted,
                action_points,
                hit,
            } => {
                if !action_points.is_finite() || *action_points < 0.0 {
                    return Err(DecodeError::InvalidNumber);
                }
                out.push(VATS_ATTACK_RESULT);
                out.extend_from_slice(&sequence.to_be_bytes());
                out.extend_from_slice(&attack_id.to_be_bytes());
                out.extend_from_slice(&shot_index.to_be_bytes());
                out.push(u8::from(*accepted));
                out.extend_from_slice(&action_points.to_bits().to_be_bytes());
                out.push(u8::from(*hit));
            }
        }
        if out.len() > MAX_PACKET_BYTES {
            return Err(DecodeError::WrongLength);
        }
        Ok(out)
    }

    pub fn decode(packet: &[u8]) -> Result<Self, DecodeError> {
        if packet.len() > MAX_PACKET_BYTES {
            return Err(DecodeError::WrongLength);
        }
        if packet.len() < 6 {
            return Err(DecodeError::TooShort);
        }
        if &packet[..4] != MAGIC {
            return Err(DecodeError::BadMagic);
        }
        if packet[4] != VERSION {
            return Err(DecodeError::UnsupportedVersion(packet[4]));
        }
        match packet[5] {
            HELLO => {
                require_length(packet, 14)?;
                Ok(Self::Hello {
                    scene_key: u64::from_be_bytes(packet[6..14].try_into().unwrap()),
                })
            }
            INPUT => {
                require_length(packet, 31)?;
                let sequence = u32::from_be_bytes(packet[6..10].try_into().unwrap());
                let axes = [read_f32(packet, 10)?, read_f32(packet, 14)?];
                let heading = read_f32(packet, 18)?;
                let pitch = read_f32(packet, 22)?;
                let view_aspect = read_f32(packet, 26)?;
                let flags = packet[30];
                if flags & !0b11 != 0 {
                    return Err(DecodeError::InvalidFlags);
                }
                if axes.iter().any(|v| v.abs() > 1.0)
                    || pitch.abs() > std::f32::consts::FRAC_PI_2
                    || !valid_view_aspect(view_aspect)
                {
                    return Err(DecodeError::InvalidNumber);
                }
                Ok(Self::Input(PlayerInput {
                    sequence,
                    axes,
                    heading,
                    pitch,
                    view_aspect,
                    running: flags & 0b01 != 0,
                    sneaking: flags & 0b10 != 0,
                }))
            }
            WELCOME => {
                require_length(packet, 15)?;
                let player = PlayerId(packet[6]);
                if player.0 == 0 || usize::from(player.0) >= MAX_PLAYERS {
                    return Err(DecodeError::InvalidPlayerId);
                }
                Ok(Self::Welcome {
                    player,
                    scene_key: u64::from_be_bytes(packet[7..15].try_into().unwrap()),
                })
            }
            SNAPSHOT => {
                if packet.len() < 17 {
                    return Err(DecodeError::TooShort);
                }
                let tick = u32::from_be_bytes(packet[6..10].try_into().unwrap());
                let assigned_player = PlayerId(packet[10]);
                let count = usize::from(packet[11]);
                if assigned_player.0 as usize >= MAX_PLAYERS {
                    return Err(DecodeError::InvalidPlayerId);
                }
                if count > MAX_PLAYERS {
                    return Err(DecodeError::TooManyPlayers);
                }
                let players_end = 12 + count * 17;
                if packet.len() < players_end + 13 {
                    return Err(DecodeError::TooShort);
                }
                let mut players = Vec::with_capacity(count);
                let mut ids = HashSet::with_capacity(count);
                for chunk in packet[12..players_end].chunks_exact(17) {
                    let id = PlayerId(chunk[0]);
                    if id.0 as usize >= MAX_PLAYERS {
                        return Err(DecodeError::InvalidPlayerId);
                    }
                    if !ids.insert(id) {
                        return Err(DecodeError::DuplicatePlayerId);
                    }
                    let position = [
                        read_f32(chunk, 1)?,
                        read_f32(chunk, 5)?,
                        read_f32(chunk, 9)?,
                    ];
                    let heading = read_f32(chunk, 13)?;
                    players.push(PlayerPose {
                        id,
                        position,
                        heading,
                    });
                }
                if !ids.contains(&assigned_player) {
                    return Err(DecodeError::AssignedPlayerMissing);
                }
                let world_revision =
                    u32::from_be_bytes(packet[players_end..players_end + 4].try_into().unwrap());
                let campaign_revision = u32::from_be_bytes(
                    packet[players_end + 4..players_end + 8].try_into().unwrap(),
                );
                let world_update_revision = u32::from_be_bytes(
                    packet[players_end + 8..players_end + 12]
                        .try_into()
                        .unwrap(),
                );
                let event_count = usize::from(packet[players_end + 12]);
                if event_count > MAX_WORLD_EVENTS {
                    return Err(DecodeError::TooManyWorldEvents);
                }
                let mut world_events = Vec::with_capacity(event_count);
                let mut offset = players_end + 13;
                for _ in 0..event_count {
                    let (event, used) = decode_world_event(&packet[offset..])?;
                    offset += used;
                    world_events.push(event);
                }
                require_length(packet, offset)?;
                Ok(Self::Snapshot {
                    tick,
                    assigned_player,
                    players,
                    world_revision,
                    campaign_revision,
                    world_update_revision,
                    world_events,
                })
            }
            CAMPAIGN_UPDATE => {
                if packet.len() < 7 {
                    return Err(DecodeError::TooShort);
                }
                let count = usize::from(packet[6]);
                if count > MAX_CAMPAIGN_EVENTS {
                    return Err(DecodeError::TooManyCampaignEvents);
                }
                let mut offset = 7;
                let mut events = Vec::with_capacity(count);
                for _ in 0..count {
                    if packet.len() < offset + 5 {
                        return Err(DecodeError::TooShort);
                    }
                    let revision =
                        u32::from_be_bytes(packet[offset..offset + 4].try_into().unwrap());
                    let kind = packet[offset + 4];
                    offset += 5;
                    let change = match kind {
                        1 => {
                            require_available(packet, offset, 6)?;
                            let quest =
                                u32::from_be_bytes(packet[offset..offset + 4].try_into().unwrap());
                            let stage = u16::from_be_bytes(
                                packet[offset + 4..offset + 6].try_into().unwrap(),
                            );
                            offset += 6;
                            CampaignChange::QuestStage { quest, stage }
                        }
                        2 => {
                            require_available(packet, offset, 7)?;
                            let quest =
                                u32::from_be_bytes(packet[offset..offset + 4].try_into().unwrap());
                            let stage = u16::from_be_bytes(
                                packet[offset + 4..offset + 6].try_into().unwrap(),
                            );
                            let done = read_bool(packet[offset + 6])?;
                            offset += 7;
                            CampaignChange::QuestStageDone { quest, stage, done }
                        }
                        3 => {
                            require_available(packet, offset, 5)?;
                            let quest =
                                u32::from_be_bytes(packet[offset..offset + 4].try_into().unwrap());
                            let flags = packet[offset + 4];
                            if flags & !0b111 != 0 {
                                return Err(DecodeError::InvalidFlags);
                            }
                            offset += 5;
                            CampaignChange::QuestStatus {
                                quest,
                                running: flags & 1 != 0,
                                completed: flags & 2 != 0,
                                failed: flags & 4 != 0,
                            }
                        }
                        4 => {
                            require_available(packet, offset, 12)?;
                            let reputation =
                                u32::from_be_bytes(packet[offset..offset + 4].try_into().unwrap());
                            let fame = read_f32(packet, offset + 4)?;
                            let infamy = read_f32(packet, offset + 8)?;
                            offset += 12;
                            CampaignChange::Reputation {
                                reputation,
                                fame,
                                infamy,
                            }
                        }
                        5 => {
                            require_available(packet, offset, 9)?;
                            let quest =
                                u32::from_be_bytes(packet[offset..offset + 4].try_into().unwrap());
                            let objective = i32::from_be_bytes(
                                packet[offset + 4..offset + 8].try_into().unwrap(),
                            );
                            let completed = read_bool(packet[offset + 8])?;
                            offset += 9;
                            CampaignChange::QuestObjective {
                                quest,
                                objective,
                                completed,
                            }
                        }
                        other => return Err(DecodeError::UnknownCampaignChange(other)),
                    };
                    events.push(CampaignEvent { revision, change });
                }
                require_length(packet, offset)?;
                Ok(Self::CampaignUpdate { events })
            }
            WORLD_UPDATE => {
                if packet.len() < 7 {
                    return Err(DecodeError::TooShort);
                }
                let count = usize::from(packet[6]);
                if count > MAX_WORLD_UPDATE_EVENTS {
                    return Err(DecodeError::TooManyWorldUpdateEvents);
                }
                let mut events = Vec::with_capacity(count);
                let mut offset = 7;
                for _ in 0..count {
                    let (event, used) = decode_world_event(&packet[offset..])?;
                    offset += used;
                    events.push(event);
                }
                require_length(packet, offset)?;
                Ok(Self::WorldUpdate { events })
            }
            WORLD_KEYFRAME_REQUEST => {
                require_length(packet, 14)?;
                Ok(Self::WorldKeyframeRequest {
                    transfer_id: u32::from_be_bytes(packet[6..10].try_into().unwrap()),
                    after_revision: u32::from_be_bytes(packet[10..14].try_into().unwrap()),
                })
            }
            WORLD_KEYFRAME_CHUNK => {
                if packet.len() < 20 {
                    return Err(DecodeError::TooShort);
                }
                let transfer_id = u32::from_be_bytes(packet[6..10].try_into().unwrap());
                let revision = u32::from_be_bytes(packet[10..14].try_into().unwrap());
                let index = u16::from_be_bytes(packet[14..16].try_into().unwrap());
                let count = u16::from_be_bytes(packet[16..18].try_into().unwrap());
                let entry_count =
                    usize::from(u16::from_be_bytes(packet[18..20].try_into().unwrap()));
                if count == 0 || usize::from(count) > MAX_WORLD_KEYFRAME_CHUNKS || index >= count {
                    return Err(DecodeError::InvalidWorldKeyframe);
                }
                if entry_count > MAX_WORLD_KEYFRAME_ENTRIES {
                    return Err(DecodeError::TooManyWorldKeyframeEntries);
                }
                let mut entries = Vec::with_capacity(entry_count);
                let mut offset = 20;
                for _ in 0..entry_count {
                    require_available(packet, offset, 5)?;
                    let flags = packet[offset + 4];
                    if flags == 0 || flags & !0b111 != 0 {
                        return Err(DecodeError::InvalidWorldKeyframe);
                    }
                    let reference =
                        u32::from_be_bytes(packet[offset..offset + 4].try_into().unwrap());
                    if reference == 0 {
                        return Err(DecodeError::InvalidWorldKeyframe);
                    }
                    offset += 5;
                    let health_milli = if flags & 4 != 0 {
                        require_available(packet, offset, 4)?;
                        let health =
                            i32::from_be_bytes(packet[offset..offset + 4].try_into().unwrap());
                        if health < 0 {
                            return Err(DecodeError::InvalidNumber);
                        }
                        offset += 4;
                        Some(health)
                    } else {
                        None
                    };
                    entries.push(WorldStateEntry {
                        reference,
                        dead: flags & 1 != 0,
                        destroyed: flags & 2 != 0,
                        health_milli,
                    });
                }
                require_length(packet, offset)?;
                Ok(Self::WorldKeyframeChunk {
                    transfer_id,
                    revision,
                    index,
                    count,
                    entries,
                })
            }
            WORLD_KEYFRAME_ACK => {
                require_length(packet, 10)?;
                Ok(Self::WorldKeyframeAck {
                    transfer_id: u32::from_be_bytes(packet[6..10].try_into().unwrap()),
                })
            }
            DIALOGUE_UPDATE => {
                if packet.len() < 11 {
                    return Err(DecodeError::TooShort);
                }
                let sequence = u32::from_be_bytes(packet[6..10].try_into().unwrap());
                let mut offset = 10;
                let dialogue = decode_dialogue(packet, &mut offset)?;
                require_length(packet, offset)?;
                Ok(Self::DialogueUpdate { sequence, dialogue })
            }
            DIALOGUE_LISTEN => {
                if packet.len() < 12 {
                    return Err(DecodeError::TooShort);
                }
                let player = PlayerId(packet[6]);
                if usize::from(player.0) >= MAX_PLAYERS {
                    return Err(DecodeError::InvalidPlayerId);
                }
                let sequence = u32::from_be_bytes(packet[7..11].try_into().unwrap());
                let mut offset = 11;
                let dialogue = decode_dialogue(packet, &mut offset)?;
                require_length(packet, offset)?;
                Ok(Self::DialogueListen {
                    player,
                    sequence,
                    dialogue,
                })
            }
            VATS_AIM => {
                require_length(packet, 14)?;
                let target = u32::from_be_bytes(packet[10..14].try_into().unwrap());
                if target == 0 {
                    return Err(DecodeError::InvalidTarget);
                }
                Ok(Self::VatsAim {
                    sequence: u32::from_be_bytes(packet[6..10].try_into().unwrap()),
                    target,
                })
            }
            VATS_AIM_RESULT => {
                require_length(packet, 15)?;
                let target = u32::from_be_bytes(packet[10..14].try_into().unwrap());
                if target == 0 {
                    return Err(DecodeError::InvalidTarget);
                }
                Ok(Self::VatsAimResult {
                    sequence: u32::from_be_bytes(packet[6..10].try_into().unwrap()),
                    target,
                    accepted: read_bool(packet[14])?,
                })
            }
            VATS_ATTACK => {
                require_length(packet, 43)?;
                let target = u32::from_be_bytes(packet[20..24].try_into().unwrap());
                let action_points = read_f32(packet, 35)?;
                let hit_chance = read_f32(packet, 39)?;
                if target == 0 {
                    return Err(DecodeError::InvalidTarget);
                }
                if action_points <= 0.0 {
                    return Err(DecodeError::InvalidNumber);
                }
                if !(0.0..=1.0).contains(&hit_chance) {
                    return Err(DecodeError::InvalidNumber);
                }
                Ok(Self::VatsAttack {
                    sequence: u32::from_be_bytes(packet[6..10].try_into().unwrap()),
                    attack_id: u64::from_be_bytes(packet[10..18].try_into().unwrap()),
                    shot_index: u16::from_be_bytes(packet[18..20].try_into().unwrap()),
                    target,
                    weapon: u32::from_be_bytes(packet[24..28].try_into().unwrap()),
                    kind: packet[28],
                    reload: read_bool(packet[29])?,
                    slot: packet[30] as i8,
                    part_av: i32::from_be_bytes(packet[31..35].try_into().unwrap()),
                    action_points,
                    hit_chance,
                })
            }
            VATS_ATTACK_RESULT => {
                require_length(packet, 26)?;
                let shot_index = u16::from_be_bytes(packet[18..20].try_into().unwrap());
                let accepted = read_bool(packet[20])?;
                let action_points = read_f32(packet, 21)?;
                let hit = read_bool(packet[25])?;
                if action_points < 0.0 {
                    return Err(DecodeError::InvalidNumber);
                }
                Ok(Self::VatsAttackResult {
                    sequence: u32::from_be_bytes(packet[6..10].try_into().unwrap()),
                    attack_id: u64::from_be_bytes(packet[10..18].try_into().unwrap()),
                    shot_index,
                    accepted,
                    action_points,
                    hit,
                })
            }
            kind => Err(DecodeError::UnknownKind(kind)),
        }
    }
}

fn valid_view_aspect(aspect: f32) -> bool {
    aspect.is_finite() && (MIN_VIEW_ASPECT..=MAX_VIEW_ASPECT).contains(&aspect)
}

fn encode_dialogue(
    out: &mut Vec<u8>,
    dialogue: Option<&DialogueSnapshot>,
) -> Result<(), DecodeError> {
    let Some(dialogue) = dialogue else {
        out.push(0);
        return Ok(());
    };
    let name = dialogue.speaker_name.as_bytes();
    let text = dialogue.text.as_bytes();
    if dialogue.speaker == 0
        || name.len() > MAX_DIALOGUE_NAME_BYTES
        || text.len() > MAX_DIALOGUE_TEXT_BYTES
    {
        return Err(DecodeError::InvalidDialogue);
    }
    if dialogue.topics.len() > MAX_DIALOGUE_TOPICS {
        return Err(DecodeError::TooManyDialogueTopics);
    }
    if dialogue
        .topics
        .iter()
        .any(|topic| topic.len() > MAX_DIALOGUE_TOPIC_BYTES)
    {
        return Err(DecodeError::InvalidDialogue);
    }

    out.push(1);
    out.extend_from_slice(&dialogue.speaker.to_be_bytes());
    out.push(name.len() as u8);
    out.extend_from_slice(name);
    out.extend_from_slice(&(text.len() as u16).to_be_bytes());
    out.extend_from_slice(text);
    out.push(dialogue.topics.len() as u8);
    for topic in &dialogue.topics {
        out.push(topic.len() as u8);
        out.extend_from_slice(topic.as_bytes());
    }
    Ok(())
}

fn decode_dialogue(
    packet: &[u8],
    offset: &mut usize,
) -> Result<Option<DialogueSnapshot>, DecodeError> {
    require_available(packet, *offset, 1)?;
    let active = read_bool(packet[*offset])?;
    *offset += 1;
    if !active {
        return Ok(None);
    }

    require_available(packet, *offset, 5)?;
    let speaker = u32::from_be_bytes(packet[*offset..*offset + 4].try_into().unwrap());
    *offset += 4;
    if speaker == 0 {
        return Err(DecodeError::InvalidDialogue);
    }
    let name_len = usize::from(packet[*offset]);
    *offset += 1;
    if name_len > MAX_DIALOGUE_NAME_BYTES {
        return Err(DecodeError::InvalidDialogue);
    }
    let name = decode_dialogue_string(packet, offset, name_len)?;

    require_available(packet, *offset, 2)?;
    let text_len = usize::from(u16::from_be_bytes(
        packet[*offset..*offset + 2].try_into().unwrap(),
    ));
    *offset += 2;
    if text_len > MAX_DIALOGUE_TEXT_BYTES {
        return Err(DecodeError::InvalidDialogue);
    }
    let text = decode_dialogue_string(packet, offset, text_len)?;

    require_available(packet, *offset, 1)?;
    let topic_count = usize::from(packet[*offset]);
    *offset += 1;
    if topic_count > MAX_DIALOGUE_TOPICS {
        return Err(DecodeError::TooManyDialogueTopics);
    }
    let mut topics = Vec::with_capacity(topic_count);
    for _ in 0..topic_count {
        require_available(packet, *offset, 1)?;
        let length = usize::from(packet[*offset]);
        *offset += 1;
        if length > MAX_DIALOGUE_TOPIC_BYTES {
            return Err(DecodeError::InvalidDialogue);
        }
        topics.push(decode_dialogue_string(packet, offset, length)?);
    }
    Ok(Some(DialogueSnapshot {
        speaker,
        speaker_name: name,
        text,
        topics,
    }))
}

fn decode_dialogue_string(
    packet: &[u8],
    offset: &mut usize,
    length: usize,
) -> Result<String, DecodeError> {
    require_available(packet, *offset, length)?;
    let value = std::str::from_utf8(&packet[*offset..*offset + length])
        .map_err(|_| DecodeError::InvalidDialogue)?;
    *offset += length;
    Ok(value.to_owned())
}

fn require_length(packet: &[u8], expected: usize) -> Result<(), DecodeError> {
    if packet.len() == expected {
        Ok(())
    } else {
        Err(DecodeError::WrongLength)
    }
}

fn require_available(packet: &[u8], offset: usize, count: usize) -> Result<(), DecodeError> {
    if packet.len() >= offset + count {
        Ok(())
    } else {
        Err(DecodeError::TooShort)
    }
}

fn read_bool(value: u8) -> Result<bool, DecodeError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(DecodeError::InvalidFlags),
    }
}

fn read_f32(packet: &[u8], offset: usize) -> Result<f32, DecodeError> {
    let value = f32::from_bits(u32::from_be_bytes(
        packet[offset..offset + 4].try_into().unwrap(),
    ));
    if value.is_finite() {
        Ok(value)
    } else {
        Err(DecodeError::InvalidNumber)
    }
}

/// True when `candidate` is later than `previous` within the wrapping
/// half-range sequence window used by UDP input packets.
pub fn sequence_is_newer(candidate: u32, previous: u32) -> bool {
    candidate != previous && candidate.wrapping_sub(previous) < (1 << 31)
}
