//! Small versioned wire protocol shared by the co-op experiment and viewer.
//!
//! This protocol is still experimental. It carries player input, pose
//! snapshots, host-resolved V.A.T.S. and ordinary hitscan/melee attacks,
//! acknowledged combat inventory and actor-value/perk profiles, a bounded
//! replay of shared death/destruction/health changes, chunked world keyframes
//! for recovery, and owner-scoped campaign changes sent to the host, plus
//! replayed host-authoritative shared quest changes. It does not authenticate
//! peers.

use std::collections::HashSet;
use std::fmt;

const MAGIC: &[u8; 4] = b"NVCE";
const VERSION: u8 = 24;
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
const CAMPAIGN_SYNC: u8 = 16;
const CAMPAIGN_SYNC_ACK: u8 = 17;
const PLAYER_ATTACK: u8 = 18;
const PLAYER_ATTACK_RESULT: u8 = 19;
const PLAYER_INVENTORY: u8 = 20;
const PLAYER_INVENTORY_ACK: u8 = 21;
const PLAYER_PROFILE: u8 = 22;
const PLAYER_PROFILE_ACK: u8 = 23;
const DIALOGUE_CHOICE: u8 = 24;
const DIALOGUE_CHOICE_RESULT: u8 = 25;
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
/// Dialogue topic references kept under one UDP datagram.
pub const MAX_DIALOGUE_TOPICS: usize = 8;
/// Campaign changes sent to or replayed by the host.
pub const MAX_CAMPAIGN_EVENTS: usize = 64;
/// Combat-relevant inventory entries carried in one player's baseline.
pub const MAX_PLAYER_INVENTORY_ENTRIES: usize = 128;
/// Actor values included in a guest's combat profile (the game's AV table).
pub const MAX_PLAYER_PROFILE_ACTOR_VALUES: usize = 77;
/// Perks included in a guest's combat profile.
pub const MAX_PLAYER_PROFILE_PERKS: usize = 96;

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

/// A positive count of one of a player's reported combat inventory forms.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InventoryEntry {
    pub form: u32,
    pub count: u32,
}

/// A guest's current and permanent value for one actor value.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ActorValueEntry {
    pub actor_value: u8,
    pub current: f32,
    pub permanent: f32,
}

/// A guest's owned perk and its current rank.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PerkEntry {
    pub form: u32,
    pub rank: u8,
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
    ActorDied {
        reference: u32,
        /// Co-op player who delivered the host-resolved killing hit, if any.
        player: Option<u8>,
    },
    ActorRevived {
        reference: u32,
    },
    ActorHealth {
        reference: u32,
        health_milli: i32,
    },
    ReferenceDestroyed {
        reference: u32,
    },
    ReferenceRestored {
        reference: u32,
    },
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

/// A game-data-backed response being spoken in a conversation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DialogueLine {
    pub info: u32,
    pub response: u16,
}

/// A game-data-backed response offered to the speaker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DialogueChoice {
    pub topic: u32,
    pub info: u32,
}

/// Read-only conversation references replicated to nearby players.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DialogueSnapshot {
    pub speaker: u32,
    pub line: Option<DialogueLine>,
    /// Offered topics while the speaker waits for a choice; listeners cannot
    /// select them.
    pub topics: Vec<DialogueChoice>,
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
    QuestVariable {
        quest: u32,
        index: u16,
        value: f32,
    },
    GlobalValue {
        global: u32,
        value: f32,
    },
    DialogueFact {
        kind: DialogueFactKind,
        form: u32,
        learned: bool,
    },
    PlayerSex {
        female: Option<bool>,
    },
    PlayerItemCount {
        form: u32,
        count: u32,
    },
    PlayerItemsInitialized,
    /// Host-authored current and permanent actor value for one player.
    PlayerActorValue {
        player: u8,
        actor_value: u8,
        current: f32,
        permanent: f32,
    },
    /// Miscellaneous stat report or host-authoritative update for one player.
    PlayerMiscStat {
        player: u8,
        stat: u8,
        value: u32,
        authoritative: bool,
    },
    /// Client's initial per-player miscellaneous-stat baseline.
    PlayerMiscStatsInitialized {
        player: u8,
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

/// Player-owned dialogue knowledge carried through the campaign event stream.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DialogueFactKind {
    Topic,
    Said,
    TalkedTo,
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
    /// A client reports its current local dialogue state to the host. The
    /// viewer accepts it only for a loaded, living speaker in interaction
    /// range with clear line of sight.
    DialogueUpdate {
        sequence: u32,
        dialogue: Option<DialogueSnapshot>,
    },
    /// A speaker's locally chosen response, paired with the choice list that
    /// was visible when it was selected. `source` identifies the line whose
    /// explicit `TCLT` choices produced the menu. The host rebuilds the choice
    /// set from loaded game data and that guest's synchronized player state.
    DialogueChoice {
        sequence: u32,
        offer: DialogueSnapshot,
        /// The line whose explicit `TCLT` list produced this menu, when any.
        source: Option<u32>,
        topic: u32,
        info: u32,
    },
    /// Acknowledgement for a reliable dialogue-choice request.
    DialogueChoiceResult {
        sequence: u32,
        accepted: bool,
    },
    /// The host sends one nearby speaker's read-only dialogue state to a
    /// listener. `player` identifies the speaker's player, not the NPC.
    DialogueListen {
        player: PlayerId,
        sequence: u32,
        dialogue: Option<DialogueSnapshot>,
    },
    /// A client's quest and faction progress report. The host records it
    /// under the sender's player ID, then republishes shared-scope quests.
    CampaignUpdate {
        events: Vec<CampaignEvent>,
    },
    /// Host-authoritative shared quest changes, delivered reliably by replay.
    CampaignSync {
        events: Vec<CampaignEvent>,
    },
    /// Highest contiguous shared campaign event applied by a client.
    CampaignSyncAck {
        revision: u32,
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
    /// calculate and roll its hit/miss from host-owned game data.
    VatsAttack {
        sequence: u32,
        attack_id: u64,
        shot_index: u16,
        target: u32,
        weapon: u32,
        inventory_revision: u32,
        profile_revision: u32,
        ammo: u32,
        kind: u8,
        reload: bool,
        slot: i8,
        part_av: i32,
        action_points: f32,
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
    /// Ask the host to resolve one hitscan shot, non-thrown flying projectile,
    /// or melee swing from the sender's latest host-tracked pose and aim.
    /// The inventory revision must name the sender's latest acknowledged
    /// combat-inventory snapshot. The host rejects unreported weapons and
    /// ammunition.
    PlayerAttack {
        sequence: u32,
        inventory_revision: u32,
        profile_revision: u32,
        weapon: u32,
        ammo: u32,
        attack_group: u8,
        power: bool,
    },
    /// Acknowledge a player attack request. The world result is replicated in
    /// the normal authoritative health/death stream.
    PlayerAttackResult {
        sequence: u32,
        accepted: bool,
    },
    /// Replace the sender's host-side combat weapon and ammunition mirror.
    PlayerInventory {
        revision: u32,
        entries: Vec<InventoryEntry>,
    },
    /// Confirm that a combat-inventory baseline is installed on the host.
    PlayerInventoryAck {
        revision: u32,
    },
    /// Replace the sender's host-side combat actor values and perk list.
    PlayerProfile {
        revision: u32,
        level: u16,
        actor_values: Vec<ActorValueEntry>,
        perks: Vec<PerkEntry>,
    },
    /// Confirm that a combat profile is installed on the host.
    PlayerProfileAck {
        revision: u32,
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
    InvalidAttack,
    TooManyInventoryEntries,
    InvalidInventory,
    InvalidProfile,
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
            Self::InvalidAttack => f.write_str("player attack packet contains invalid data"),
            Self::TooManyInventoryEntries => {
                f.write_str("player inventory exceeds its entry limit")
            }
            Self::InvalidInventory => f.write_str("player inventory contains invalid data"),
            Self::InvalidProfile => f.write_str("player combat profile contains invalid data"),
        }
    }
}

impl std::error::Error for DecodeError {}

fn encode_world_event(out: &mut Vec<u8>, event: WorldEvent) -> Result<(), DecodeError> {
    out.extend_from_slice(&event.revision.to_be_bytes());
    match event.change {
        WorldChange::ActorDied { reference, player } => {
            if player.is_some_and(|player| usize::from(player) >= MAX_PLAYERS) {
                return Err(DecodeError::InvalidPlayerId);
            }
            out.push(1);
            out.extend_from_slice(&reference.to_be_bytes());
            out.push(player.map_or(0, |player| player + 1));
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
    Ok(())
}

fn decode_world_event(bytes: &[u8]) -> Result<(WorldEvent, usize), DecodeError> {
    if bytes.len() < 9 {
        return Err(DecodeError::TooShort);
    }
    let revision = u32::from_be_bytes(bytes[..4].try_into().unwrap());
    let reference = u32::from_be_bytes(bytes[5..9].try_into().unwrap());
    let change = match bytes[4] {
        1 => {
            if bytes.len() < 10 {
                return Err(DecodeError::TooShort);
            }
            let player = match bytes[9] {
                0 => None,
                encoded if usize::from(encoded) <= MAX_PLAYERS => Some(encoded - 1),
                _ => return Err(DecodeError::InvalidPlayerId),
            };
            WorldChange::ActorDied { reference, player }
        }
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
    let length = match change {
        WorldChange::ActorDied { .. } => 10,
        WorldChange::ActorHealth { .. } => 13,
        _ => 9,
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
                    encode_world_event(&mut out, *event)?;
                }
            }
            Self::CampaignUpdate { events } => {
                out.push(CAMPAIGN_UPDATE);
                encode_campaign_events(&mut out, events)?;
            }
            Self::CampaignSync { events } => {
                out.push(CAMPAIGN_SYNC);
                encode_campaign_events(&mut out, events)?;
            }
            Self::CampaignSyncAck { revision } => {
                out.push(CAMPAIGN_SYNC_ACK);
                out.extend_from_slice(&revision.to_be_bytes());
            }
            Self::WorldUpdate { events } => {
                if events.len() > MAX_WORLD_UPDATE_EVENTS {
                    return Err(DecodeError::TooManyWorldUpdateEvents);
                }
                out.push(WORLD_UPDATE);
                out.push(events.len() as u8);
                for event in events {
                    encode_world_event(&mut out, *event)?;
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
            Self::DialogueChoice {
                sequence,
                offer,
                source,
                topic,
                info,
            } => {
                if offer.line.is_some()
                    || offer.topics.is_empty()
                    || offer.topics.len() > MAX_DIALOGUE_TOPICS
                    || *topic == 0
                    || *info == 0
                    || source == &Some(0)
                    || !offer
                        .topics
                        .iter()
                        .any(|choice| choice.topic == *topic && choice.info == *info)
                {
                    return Err(DecodeError::InvalidDialogue);
                }
                out.push(DIALOGUE_CHOICE);
                out.extend_from_slice(&sequence.to_be_bytes());
                encode_dialogue(&mut out, Some(offer))?;
                if let Some(source) = source {
                    out.push(1);
                    out.extend_from_slice(&source.to_be_bytes());
                } else {
                    out.push(0);
                }
                out.extend_from_slice(&topic.to_be_bytes());
                out.extend_from_slice(&info.to_be_bytes());
            }
            Self::DialogueChoiceResult { sequence, accepted } => {
                out.push(DIALOGUE_CHOICE_RESULT);
                out.extend_from_slice(&sequence.to_be_bytes());
                out.push(u8::from(*accepted));
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
                inventory_revision,
                profile_revision,
                ammo,
                kind,
                reload,
                slot,
                part_av,
                action_points,
            } => {
                if *target == 0 {
                    return Err(DecodeError::InvalidTarget);
                }
                if !action_points.is_finite() || *action_points <= 0.0 {
                    return Err(DecodeError::InvalidNumber);
                }
                out.push(VATS_ATTACK);
                out.extend_from_slice(&sequence.to_be_bytes());
                out.extend_from_slice(&attack_id.to_be_bytes());
                out.extend_from_slice(&shot_index.to_be_bytes());
                out.extend_from_slice(&target.to_be_bytes());
                out.extend_from_slice(&weapon.to_be_bytes());
                out.extend_from_slice(&inventory_revision.to_be_bytes());
                out.extend_from_slice(&profile_revision.to_be_bytes());
                out.extend_from_slice(&ammo.to_be_bytes());
                out.push(*kind);
                out.push(u8::from(*reload));
                out.push(*slot as u8);
                out.extend_from_slice(&part_av.to_be_bytes());
                out.extend_from_slice(&action_points.to_bits().to_be_bytes());
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
            Self::PlayerAttack {
                sequence,
                inventory_revision,
                profile_revision,
                weapon,
                ammo,
                attack_group,
                power,
            } => {
                if *attack_group >= 64 {
                    return Err(DecodeError::InvalidAttack);
                }
                out.push(PLAYER_ATTACK);
                out.extend_from_slice(&sequence.to_be_bytes());
                out.extend_from_slice(&inventory_revision.to_be_bytes());
                out.extend_from_slice(&profile_revision.to_be_bytes());
                out.extend_from_slice(&weapon.to_be_bytes());
                out.extend_from_slice(&ammo.to_be_bytes());
                out.push(*attack_group);
                out.push(u8::from(*power));
            }
            Self::PlayerAttackResult { sequence, accepted } => {
                out.push(PLAYER_ATTACK_RESULT);
                out.extend_from_slice(&sequence.to_be_bytes());
                out.push(u8::from(*accepted));
            }
            Self::PlayerInventory { revision, entries } => {
                if entries.len() > MAX_PLAYER_INVENTORY_ENTRIES {
                    return Err(DecodeError::TooManyInventoryEntries);
                }
                let mut forms = HashSet::with_capacity(entries.len());
                if entries
                    .iter()
                    .any(|entry| entry.form == 0 || entry.count == 0 || !forms.insert(entry.form))
                {
                    return Err(DecodeError::InvalidInventory);
                }
                out.push(PLAYER_INVENTORY);
                out.extend_from_slice(&revision.to_be_bytes());
                out.push(entries.len() as u8);
                for entry in entries {
                    out.extend_from_slice(&entry.form.to_be_bytes());
                    out.extend_from_slice(&entry.count.to_be_bytes());
                }
            }
            Self::PlayerInventoryAck { revision } => {
                out.push(PLAYER_INVENTORY_ACK);
                out.extend_from_slice(&revision.to_be_bytes());
            }
            Self::PlayerProfile {
                revision,
                level,
                actor_values,
                perks,
            } => {
                if actor_values.len() > MAX_PLAYER_PROFILE_ACTOR_VALUES
                    || perks.len() > MAX_PLAYER_PROFILE_PERKS
                {
                    return Err(DecodeError::InvalidProfile);
                }
                let mut actor_ids = HashSet::with_capacity(actor_values.len());
                if actor_values.iter().any(|entry| {
                    usize::from(entry.actor_value) >= MAX_PLAYER_PROFILE_ACTOR_VALUES
                        || !entry.current.is_finite()
                        || !entry.permanent.is_finite()
                        || !actor_ids.insert(entry.actor_value)
                }) {
                    return Err(DecodeError::InvalidProfile);
                }
                let mut perk_forms = HashSet::with_capacity(perks.len());
                if perks
                    .iter()
                    .any(|perk| perk.form == 0 || perk.rank == 0 || !perk_forms.insert(perk.form))
                {
                    return Err(DecodeError::InvalidProfile);
                }
                out.push(PLAYER_PROFILE);
                out.extend_from_slice(&revision.to_be_bytes());
                out.extend_from_slice(&level.to_be_bytes());
                out.push(actor_values.len() as u8);
                out.push(perks.len() as u8);
                for entry in actor_values {
                    out.push(entry.actor_value);
                    out.extend_from_slice(&entry.current.to_bits().to_be_bytes());
                    out.extend_from_slice(&entry.permanent.to_bits().to_be_bytes());
                }
                for perk in perks {
                    out.extend_from_slice(&perk.form.to_be_bytes());
                    out.push(perk.rank);
                }
            }
            Self::PlayerProfileAck { revision } => {
                out.push(PLAYER_PROFILE_ACK);
                out.extend_from_slice(&revision.to_be_bytes());
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
            CAMPAIGN_UPDATE | CAMPAIGN_SYNC => {
                let events = decode_campaign_events(packet)?;
                if packet[5] == CAMPAIGN_UPDATE {
                    Ok(Self::CampaignUpdate { events })
                } else {
                    Ok(Self::CampaignSync { events })
                }
            }
            CAMPAIGN_SYNC_ACK => {
                require_length(packet, 10)?;
                Ok(Self::CampaignSyncAck {
                    revision: u32::from_be_bytes(packet[6..10].try_into().unwrap()),
                })
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
                if packet.len() < 21 {
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
            DIALOGUE_CHOICE => {
                if packet.len() < 34 {
                    return Err(DecodeError::TooShort);
                }
                let sequence = u32::from_be_bytes(packet[6..10].try_into().unwrap());
                let mut offset = 10;
                let offer =
                    decode_dialogue(packet, &mut offset)?.ok_or(DecodeError::InvalidDialogue)?;
                require_available(packet, offset, 1)?;
                let has_source = read_bool(packet[offset])?;
                offset += 1;
                let source = if has_source {
                    require_available(packet, offset, 4)?;
                    let source = u32::from_be_bytes(packet[offset..offset + 4].try_into().unwrap());
                    offset += 4;
                    if source == 0 {
                        return Err(DecodeError::InvalidDialogue);
                    }
                    Some(source)
                } else {
                    None
                };
                require_available(packet, offset, 8)?;
                let topic = u32::from_be_bytes(packet[offset..offset + 4].try_into().unwrap());
                let info = u32::from_be_bytes(packet[offset + 4..offset + 8].try_into().unwrap());
                offset += 8;
                require_length(packet, offset)?;
                if offer.line.is_some()
                    || offer.topics.is_empty()
                    || topic == 0
                    || info == 0
                    || !offer
                        .topics
                        .iter()
                        .any(|choice| choice.topic == topic && choice.info == info)
                {
                    return Err(DecodeError::InvalidDialogue);
                }
                Ok(Self::DialogueChoice {
                    sequence,
                    offer,
                    source,
                    topic,
                    info,
                })
            }
            DIALOGUE_CHOICE_RESULT => {
                require_length(packet, 11)?;
                let accepted = match packet[10] {
                    0 => false,
                    1 => true,
                    _ => return Err(DecodeError::InvalidDialogue),
                };
                Ok(Self::DialogueChoiceResult {
                    sequence: u32::from_be_bytes(packet[6..10].try_into().unwrap()),
                    accepted,
                })
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
                require_length(packet, 51)?;
                let target = u32::from_be_bytes(packet[20..24].try_into().unwrap());
                let action_points = read_f32(packet, 47)?;
                if target == 0 {
                    return Err(DecodeError::InvalidTarget);
                }
                if action_points <= 0.0 {
                    return Err(DecodeError::InvalidNumber);
                }
                Ok(Self::VatsAttack {
                    sequence: u32::from_be_bytes(packet[6..10].try_into().unwrap()),
                    attack_id: u64::from_be_bytes(packet[10..18].try_into().unwrap()),
                    shot_index: u16::from_be_bytes(packet[18..20].try_into().unwrap()),
                    target,
                    weapon: u32::from_be_bytes(packet[24..28].try_into().unwrap()),
                    inventory_revision: u32::from_be_bytes(packet[28..32].try_into().unwrap()),
                    profile_revision: u32::from_be_bytes(packet[32..36].try_into().unwrap()),
                    ammo: u32::from_be_bytes(packet[36..40].try_into().unwrap()),
                    kind: packet[40],
                    reload: read_bool(packet[41])?,
                    slot: packet[42] as i8,
                    part_av: i32::from_be_bytes(packet[43..47].try_into().unwrap()),
                    action_points,
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
            PLAYER_ATTACK => {
                require_length(packet, 28)?;
                let attack_group = packet[26];
                if attack_group >= 64 {
                    return Err(DecodeError::InvalidAttack);
                }
                Ok(Self::PlayerAttack {
                    sequence: u32::from_be_bytes(packet[6..10].try_into().unwrap()),
                    inventory_revision: u32::from_be_bytes(packet[10..14].try_into().unwrap()),
                    profile_revision: u32::from_be_bytes(packet[14..18].try_into().unwrap()),
                    weapon: u32::from_be_bytes(packet[18..22].try_into().unwrap()),
                    ammo: u32::from_be_bytes(packet[22..26].try_into().unwrap()),
                    attack_group,
                    power: read_bool(packet[27])?,
                })
            }
            PLAYER_ATTACK_RESULT => {
                require_length(packet, 11)?;
                Ok(Self::PlayerAttackResult {
                    sequence: u32::from_be_bytes(packet[6..10].try_into().unwrap()),
                    accepted: read_bool(packet[10])?,
                })
            }
            PLAYER_INVENTORY => {
                if packet.len() < 11 {
                    return Err(DecodeError::TooShort);
                }
                let count = usize::from(packet[10]);
                if count > MAX_PLAYER_INVENTORY_ENTRIES {
                    return Err(DecodeError::TooManyInventoryEntries);
                }
                require_length(packet, 11 + count * 8)?;
                let revision = u32::from_be_bytes(packet[6..10].try_into().unwrap());
                let mut forms = HashSet::with_capacity(count);
                let mut entries = Vec::with_capacity(count);
                for chunk in packet[11..].chunks_exact(8) {
                    let form = u32::from_be_bytes(chunk[..4].try_into().unwrap());
                    let count = u32::from_be_bytes(chunk[4..].try_into().unwrap());
                    if form == 0 || count == 0 || !forms.insert(form) {
                        return Err(DecodeError::InvalidInventory);
                    }
                    entries.push(InventoryEntry { form, count });
                }
                Ok(Self::PlayerInventory { revision, entries })
            }
            PLAYER_INVENTORY_ACK => {
                require_length(packet, 10)?;
                Ok(Self::PlayerInventoryAck {
                    revision: u32::from_be_bytes(packet[6..10].try_into().unwrap()),
                })
            }
            PLAYER_PROFILE => {
                if packet.len() < 14 {
                    return Err(DecodeError::TooShort);
                }
                let actor_count = usize::from(packet[12]);
                let perk_count = usize::from(packet[13]);
                if actor_count > MAX_PLAYER_PROFILE_ACTOR_VALUES
                    || perk_count > MAX_PLAYER_PROFILE_PERKS
                {
                    return Err(DecodeError::InvalidProfile);
                }
                require_length(packet, 14 + actor_count * 9 + perk_count * 5)?;
                let revision = u32::from_be_bytes(packet[6..10].try_into().unwrap());
                let level = u16::from_be_bytes(packet[10..12].try_into().unwrap());
                let mut actor_values = Vec::with_capacity(actor_count);
                let mut actor_ids = HashSet::with_capacity(actor_count);
                let mut cursor = 14;
                for _ in 0..actor_count {
                    let actor_value = packet[cursor];
                    let current = read_f32(packet, cursor + 1)?;
                    let permanent = read_f32(packet, cursor + 5)?;
                    if usize::from(actor_value) >= MAX_PLAYER_PROFILE_ACTOR_VALUES
                        || !actor_ids.insert(actor_value)
                    {
                        return Err(DecodeError::InvalidProfile);
                    }
                    actor_values.push(ActorValueEntry {
                        actor_value,
                        current,
                        permanent,
                    });
                    cursor += 9;
                }
                let mut perks = Vec::with_capacity(perk_count);
                let mut perk_forms = HashSet::with_capacity(perk_count);
                for chunk in packet[cursor..].chunks_exact(5) {
                    let form = u32::from_be_bytes(chunk[..4].try_into().unwrap());
                    let rank = chunk[4];
                    if form == 0 || rank == 0 || !perk_forms.insert(form) {
                        return Err(DecodeError::InvalidProfile);
                    }
                    perks.push(PerkEntry { form, rank });
                }
                Ok(Self::PlayerProfile {
                    revision,
                    level,
                    actor_values,
                    perks,
                })
            }
            PLAYER_PROFILE_ACK => {
                require_length(packet, 10)?;
                Ok(Self::PlayerProfileAck {
                    revision: u32::from_be_bytes(packet[6..10].try_into().unwrap()),
                })
            }
            kind => Err(DecodeError::UnknownKind(kind)),
        }
    }
}

fn valid_view_aspect(aspect: f32) -> bool {
    aspect.is_finite() && (MIN_VIEW_ASPECT..=MAX_VIEW_ASPECT).contains(&aspect)
}

fn encode_campaign_events(out: &mut Vec<u8>, events: &[CampaignEvent]) -> Result<(), DecodeError> {
    if events.len() > MAX_CAMPAIGN_EVENTS {
        return Err(DecodeError::TooManyCampaignEvents);
    }
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
            CampaignChange::QuestStatus {
                quest,
                running,
                completed,
                failed,
            } => {
                out.push(3);
                out.extend_from_slice(&quest.to_be_bytes());
                out.push(u8::from(running) | (u8::from(completed) << 1) | (u8::from(failed) << 2));
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
            CampaignChange::QuestVariable {
                quest,
                index,
                value,
            } => {
                if !value.is_finite() {
                    return Err(DecodeError::InvalidNumber);
                }
                out.push(6);
                out.extend_from_slice(&quest.to_be_bytes());
                out.extend_from_slice(&index.to_be_bytes());
                out.extend_from_slice(&value.to_bits().to_be_bytes());
            }
            CampaignChange::GlobalValue { global, value } => {
                if !value.is_finite() {
                    return Err(DecodeError::InvalidNumber);
                }
                out.push(7);
                out.extend_from_slice(&global.to_be_bytes());
                out.extend_from_slice(&value.to_bits().to_be_bytes());
            }
            CampaignChange::DialogueFact {
                kind,
                form,
                learned,
            } => {
                out.push(8);
                out.push(match kind {
                    DialogueFactKind::Topic => 1,
                    DialogueFactKind::Said => 2,
                    DialogueFactKind::TalkedTo => 3,
                });
                out.extend_from_slice(&form.to_be_bytes());
                out.push(u8::from(learned));
            }
            CampaignChange::PlayerSex { female } => {
                out.push(9);
                out.push(match female {
                    None => 0,
                    Some(false) => 1,
                    Some(true) => 2,
                });
            }
            CampaignChange::PlayerItemCount { form, count } => {
                if form == 0 || count > i32::MAX as u32 {
                    return Err(DecodeError::InvalidInventory);
                }
                out.push(10);
                out.extend_from_slice(&form.to_be_bytes());
                out.extend_from_slice(&count.to_be_bytes());
            }
            CampaignChange::PlayerItemsInitialized => out.push(11),
            CampaignChange::PlayerActorValue {
                player,
                actor_value,
                current,
                permanent,
            } => {
                if usize::from(player) >= MAX_PLAYERS
                    || usize::from(actor_value) >= MAX_PLAYER_PROFILE_ACTOR_VALUES
                    || !current.is_finite()
                    || !permanent.is_finite()
                {
                    return Err(DecodeError::InvalidProfile);
                }
                out.push(12);
                out.push(player);
                out.push(actor_value);
                out.extend_from_slice(&current.to_bits().to_be_bytes());
                out.extend_from_slice(&permanent.to_bits().to_be_bytes());
            }
            CampaignChange::PlayerMiscStat {
                player,
                stat,
                value,
                authoritative,
            } => {
                if usize::from(player) >= MAX_PLAYERS {
                    return Err(DecodeError::InvalidPlayerId);
                }
                out.push(13);
                out.push(player);
                out.push(stat);
                out.extend_from_slice(&value.to_be_bytes());
                out.push(u8::from(authoritative));
            }
            CampaignChange::PlayerMiscStatsInitialized { player } => {
                if usize::from(player) >= MAX_PLAYERS {
                    return Err(DecodeError::InvalidPlayerId);
                }
                out.push(14);
                out.push(player);
            }
        }
    }
    Ok(())
}

fn decode_campaign_events(packet: &[u8]) -> Result<Vec<CampaignEvent>, DecodeError> {
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
        require_available(packet, offset, 5)?;
        let revision = u32::from_be_bytes(packet[offset..offset + 4].try_into().unwrap());
        let kind = packet[offset + 4];
        offset += 5;
        let change = match kind {
            1 => {
                require_available(packet, offset, 6)?;
                let quest = u32::from_be_bytes(packet[offset..offset + 4].try_into().unwrap());
                let stage = u16::from_be_bytes(packet[offset + 4..offset + 6].try_into().unwrap());
                offset += 6;
                CampaignChange::QuestStage { quest, stage }
            }
            2 => {
                require_available(packet, offset, 7)?;
                let quest = u32::from_be_bytes(packet[offset..offset + 4].try_into().unwrap());
                let stage = u16::from_be_bytes(packet[offset + 4..offset + 6].try_into().unwrap());
                let done = read_bool(packet[offset + 6])?;
                offset += 7;
                CampaignChange::QuestStageDone { quest, stage, done }
            }
            3 => {
                require_available(packet, offset, 5)?;
                let quest = u32::from_be_bytes(packet[offset..offset + 4].try_into().unwrap());
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
                let reputation = u32::from_be_bytes(packet[offset..offset + 4].try_into().unwrap());
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
                let quest = u32::from_be_bytes(packet[offset..offset + 4].try_into().unwrap());
                let objective =
                    i32::from_be_bytes(packet[offset + 4..offset + 8].try_into().unwrap());
                let completed = read_bool(packet[offset + 8])?;
                offset += 9;
                CampaignChange::QuestObjective {
                    quest,
                    objective,
                    completed,
                }
            }
            6 => {
                require_available(packet, offset, 10)?;
                let quest = u32::from_be_bytes(packet[offset..offset + 4].try_into().unwrap());
                let index = u16::from_be_bytes(packet[offset + 4..offset + 6].try_into().unwrap());
                let value = read_f32(packet, offset + 6)?;
                offset += 10;
                CampaignChange::QuestVariable {
                    quest,
                    index,
                    value,
                }
            }
            7 => {
                require_available(packet, offset, 8)?;
                let global = u32::from_be_bytes(packet[offset..offset + 4].try_into().unwrap());
                let value = read_f32(packet, offset + 4)?;
                offset += 8;
                CampaignChange::GlobalValue { global, value }
            }
            8 => {
                require_available(packet, offset, 6)?;
                let kind = match packet[offset] {
                    1 => DialogueFactKind::Topic,
                    2 => DialogueFactKind::Said,
                    3 => DialogueFactKind::TalkedTo,
                    _ => return Err(DecodeError::InvalidFlags),
                };
                let form = u32::from_be_bytes(packet[offset + 1..offset + 5].try_into().unwrap());
                let learned = read_bool(packet[offset + 5])?;
                offset += 6;
                CampaignChange::DialogueFact {
                    kind,
                    form,
                    learned,
                }
            }
            9 => {
                require_available(packet, offset, 1)?;
                let female = match packet[offset] {
                    0 => None,
                    1 => Some(false),
                    2 => Some(true),
                    _ => return Err(DecodeError::InvalidFlags),
                };
                offset += 1;
                CampaignChange::PlayerSex { female }
            }
            10 => {
                require_available(packet, offset, 8)?;
                let form = u32::from_be_bytes(packet[offset..offset + 4].try_into().unwrap());
                let count = u32::from_be_bytes(packet[offset + 4..offset + 8].try_into().unwrap());
                offset += 8;
                if form == 0 || count > i32::MAX as u32 {
                    return Err(DecodeError::InvalidInventory);
                }
                CampaignChange::PlayerItemCount { form, count }
            }
            11 => CampaignChange::PlayerItemsInitialized,
            12 => {
                require_available(packet, offset, 10)?;
                let player = packet[offset];
                let actor_value = packet[offset + 1];
                let current = read_f32(packet, offset + 2)?;
                let permanent = read_f32(packet, offset + 6)?;
                offset += 10;
                if usize::from(player) >= MAX_PLAYERS
                    || usize::from(actor_value) >= MAX_PLAYER_PROFILE_ACTOR_VALUES
                {
                    return Err(DecodeError::InvalidProfile);
                }
                CampaignChange::PlayerActorValue {
                    player,
                    actor_value,
                    current,
                    permanent,
                }
            }
            13 => {
                require_available(packet, offset, 7)?;
                let player = packet[offset];
                let stat = packet[offset + 1];
                let value = u32::from_be_bytes(packet[offset + 2..offset + 6].try_into().unwrap());
                let authoritative = match packet[offset + 6] {
                    0 => false,
                    1 => true,
                    _ => return Err(DecodeError::InvalidFlags),
                };
                offset += 7;
                if usize::from(player) >= MAX_PLAYERS {
                    return Err(DecodeError::InvalidPlayerId);
                }
                CampaignChange::PlayerMiscStat {
                    player,
                    stat,
                    value,
                    authoritative,
                }
            }
            14 => {
                require_available(packet, offset, 1)?;
                let player = packet[offset];
                offset += 1;
                if usize::from(player) >= MAX_PLAYERS {
                    return Err(DecodeError::InvalidPlayerId);
                }
                CampaignChange::PlayerMiscStatsInitialized { player }
            }
            other => return Err(DecodeError::UnknownCampaignChange(other)),
        };
        events.push(CampaignEvent { revision, change });
    }
    require_length(packet, offset)?;
    Ok(events)
}

fn encode_dialogue(
    out: &mut Vec<u8>,
    dialogue: Option<&DialogueSnapshot>,
) -> Result<(), DecodeError> {
    let Some(dialogue) = dialogue else {
        out.push(0);
        return Ok(());
    };
    if dialogue.speaker == 0 {
        return Err(DecodeError::InvalidDialogue);
    }
    if dialogue.topics.len() > MAX_DIALOGUE_TOPICS {
        return Err(DecodeError::TooManyDialogueTopics);
    }
    if dialogue.line.is_some() && !dialogue.topics.is_empty() {
        return Err(DecodeError::InvalidDialogue);
    }

    out.push(1);
    out.extend_from_slice(&dialogue.speaker.to_be_bytes());
    if let Some(line) = dialogue.line {
        if line.info == 0 {
            return Err(DecodeError::InvalidDialogue);
        }
        out.push(1);
        out.extend_from_slice(&line.info.to_be_bytes());
        out.extend_from_slice(&line.response.to_be_bytes());
    } else {
        out.push(0);
    }
    out.push(dialogue.topics.len() as u8);
    for topic in &dialogue.topics {
        if topic.topic == 0 || topic.info == 0 {
            return Err(DecodeError::InvalidDialogue);
        }
        out.extend_from_slice(&topic.topic.to_be_bytes());
        out.extend_from_slice(&topic.info.to_be_bytes());
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
    let has_line = read_bool(packet[*offset])?;
    *offset += 1;
    let line = if has_line {
        require_available(packet, *offset, 6)?;
        let info = u32::from_be_bytes(packet[*offset..*offset + 4].try_into().unwrap());
        let response = u16::from_be_bytes(packet[*offset + 4..*offset + 6].try_into().unwrap());
        *offset += 6;
        if info == 0 {
            return Err(DecodeError::InvalidDialogue);
        }
        Some(DialogueLine { info, response })
    } else {
        None
    };

    require_available(packet, *offset, 1)?;
    let topic_count = usize::from(packet[*offset]);
    *offset += 1;
    if topic_count > MAX_DIALOGUE_TOPICS {
        return Err(DecodeError::TooManyDialogueTopics);
    }
    if line.is_some() && topic_count != 0 {
        return Err(DecodeError::InvalidDialogue);
    }
    require_available(packet, *offset, topic_count * 8)?;
    let mut topics = Vec::with_capacity(topic_count);
    for _ in 0..topic_count {
        let topic = u32::from_be_bytes(packet[*offset..*offset + 4].try_into().unwrap());
        let info = u32::from_be_bytes(packet[*offset + 4..*offset + 8].try_into().unwrap());
        *offset += 8;
        if topic == 0 || info == 0 {
            return Err(DecodeError::InvalidDialogue);
        }
        topics.push(DialogueChoice { topic, info });
    }
    Ok(Some(DialogueSnapshot {
        speaker,
        line,
        topics,
    }))
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
