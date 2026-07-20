//! The networked game session: connect → hello → game_start → play.
//! Prediction runs the EXACT zz-core step_body the server runs, at the same
//! fixed 30 Hz; on every snapshot the local body is reset to server truth and
//! unacked inputs are replayed on top (proper reconciliation). Corrections
//! render through a decaying error offset so they are invisible. Remote
//! players and zombies interpolate 100 ms in the past (ShotAnte's ring
//! buffer, generalized to N entities).

use bevy::input::mouse::AccumulatedMouseMotion;
use bevy::prelude::*;
use std::collections::{HashSet, VecDeque};
use zz_core::constants::*;
use zz_core::map::generate_map;
use zz_core::movement::step_body;
use zz_core::protocol::{ClientMsg, ServerMsg, encode_input};
use zz_core::snapshot::{dequant_pos, dequant_yaw8, dequant_yaw16};
use zz_core::types::{Body, PlayerInput};

use crate::map_render::CurrentMap;
use crate::models::{
    self, CrumpleFx, HumanoidRig, RigAssets, Viewmodel, GROWL_RANGE,
};
use crate::net::{NetClient, NetEvent};
use crate::platform;

/// Remote entities render this far in the past (seconds).
const INTERP_DELAY_S: f64 = INTERP_DELAY_MS as f64 / 1000.0;
/// Error-offset half-life: corrections fade out over roughly two half-lives.
const ERROR_HALF_LIFE_S: f32 = 0.05;
const PENDING_CAP: usize = 64;

pub struct GamePlugin;

impl Plugin for GamePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Session::default())
            .insert_resource(Predicted::default())
            .insert_resource(crate::seams::UiQueue::default())
            .insert_resource(crate::seams::LobbyView {
                name: platform::initial_name(),
                join_prefill: platform::join_code_from_url(),
                ..Default::default()
            })
            .insert_resource(crate::seams::LatestSnapshot::default())
            .insert_resource(crate::seams::LastStats::default())
            .insert_resource(crate::seams::FxQueue::default())
            .insert_resource(crate::seams::SfxQueue::default())
            .insert_resource(crate::seams::Roster::default())
            .insert_resource(MyId::default())
            .insert_resource(PrevSelf::default())
            .insert_resource(ViewmodelKick::default())
            .insert_resource(GrowlMemory::default())
            .add_systems(
                Update,
                (
                    connect_on_start,
                    net_poll,
                    process_intents,
                    sync_cursor,
                    fps_controller.run_if(in_match),
                    apply_camera.run_if(in_match),
                    interpolate_remotes.run_if(in_match),
                    animate_rigs.run_if(in_match),
                    tick_crumples.run_if(in_match),
                    ensure_viewmodel.run_if(in_match),
                    tick_viewmodel_sys.run_if(in_match),
                    proximity_growls.run_if(in_match),
                    cleanup_match_visuals,
                )
                    .chain(),
            );
    }
}

// ── session state ──────────────────────────────────────────────────────────

#[derive(Resource, Default, PartialEq)]
pub enum Session {
    #[default]
    Boot,
    Connecting,
    /// Connected; name entry + create/join UI showing.
    Menu,
    /// In a lobby waiting room (code shared, roster visible).
    InLobby,
    Playing {
        my_slot: u8,
    },
    /// Team wiped: stats overlay over the frozen world.
    Ended {
        my_slot: u8,
    },
}

impl Session {
    fn my_slot(&self) -> Option<u8> {
        match self {
            Session::Playing { my_slot } | Session::Ended { my_slot } => Some(*my_slot),
            _ => None,
        }
    }
}

/// Our connection id from Welcome — identifies "me" in lobby rosters.
#[derive(Resource, Default)]
struct MyId(String);

/// Previous own vitals, for deriving hurt/pickup sound triggers.
#[derive(Resource, Default)]
struct PrevSelf {
    health: u8,
    ammo_reserve: u8,
    grenades: u8,
}

pub fn in_match(session: Res<Session>) -> bool {
    session.my_slot().is_some()
}

/// True while the skeleton fly-camera should still fly (menu/boot states).
pub fn menu_active(session: Res<Session>) -> bool {
    session.my_slot().is_none()
}

/// The lobby/menu overlay is interactive (cursor must stay free).
#[allow(dead_code)] // consumed by the lobby_ui worker branch
pub fn ui_active(session: Res<Session>) -> bool {
    matches!(
        *session,
        Session::Menu | Session::InLobby | Session::Ended { .. }
    )
}

/// Client-side predicted self. The camera derives from this, never from raw
/// snapshots.
#[derive(Resource)]
pub struct Predicted {
    pub body: Body,
    pub yaw: f32,
    pub pitch: f32,
    pending: VecDeque<PlayerInput>,
    next_seq: u32,
    send_accum: f32,
    /// rendered = corrected + error_offset; decays to zero over ~100 ms
    error_offset: Vec3,
    /// false until the first snapshot has seeded the body from server truth
    synced: bool,
}

impl Default for Predicted {
    fn default() -> Self {
        Predicted {
            body: Body::at(0.0, 0.0),
            yaw: 0.0,
            pitch: 0.0,
            pending: VecDeque::new(),
            next_seq: 1,
            send_accum: 0.0,
            error_offset: Vec3::ZERO,
            synced: false,
        }
    }
}

#[derive(Component)]
pub struct RemotePlayer {
    pub slot: u8,
    buf: VecDeque<(f64, Vec3, f32)>, // (recv time, feet pos, yaw)
}

#[derive(Component)]
pub struct RemoteZombie {
    pub id: u16,
    pub kind: u8,
    /// Snapshot attack state (1 = telegraph / windup).
    pub state: u8,
    buf: VecDeque<(f64, Vec3, f32)>,
}

/// Tracks own-shot recoil kicks so the viewmodel can react without racing hud's Fx drain.
#[derive(Resource, Default)]
struct ViewmodelKick {
    pending: u32,
}

/// Last half-second growl bucket we already fired for, so each (id, bucket)
/// yields at most one growl (not one per frame while the hash is hot).
#[derive(Resource, Default)]
struct GrowlMemory {
    /// (zombie id, time-bucket) pairs fired this bucket window; cleared when
    /// the global bucket advances.
    bucket: u32,
    fired: HashSet<u16>,
}

/// All seam-resource writes bundled to stay under Bevy's system-param limit.
#[derive(bevy::ecs::system::SystemParam)]
struct SeamWrites<'w> {
    my_id: ResMut<'w, MyId>,
    lobby_view: ResMut<'w, crate::seams::LobbyView>,
    last_stats: ResMut<'w, crate::seams::LastStats>,
    latest: ResMut<'w, crate::seams::LatestSnapshot>,
    fx: ResMut<'w, crate::seams::FxQueue>,
    sfx: ResMut<'w, crate::seams::SfxQueue>,
    prev_self: ResMut<'w, PrevSelf>,
    roster: ResMut<'w, crate::seams::Roster>,
}

// ── connection + message flow ──────────────────────────────────────────────

fn connect_on_start(mut session: ResMut<Session>, mut net: ResMut<NetClient>) {
    if *session != Session::Boot {
        return;
    }
    match net.connect(&platform::server_url()) {
        Ok(()) => *session = Session::Connecting,
        Err(e) => warn!("connect failed: {e} — retrying"),
    }
}

#[allow(clippy::too_many_arguments)]
fn net_poll(
    mut commands: Commands,
    mut session: ResMut<Session>,
    mut net: ResMut<NetClient>,
    mut predicted: ResMut<Predicted>,
    time: Res<Time>,
    map: Option<Res<CurrentMap>>,
    mut remotes: Query<(Entity, &mut RemotePlayer)>,
    mut zombies: Query<(Entity, &mut RemoteZombie, &Transform, Option<&HumanoidRig>)>,
    assets: Option<Res<RigAssets>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut seams: SeamWrites,
    mut vm_kick: ResMut<ViewmodelKick>,
) {
    // one-time shared rig mesh/material bank
    if assets.is_none() {
        let bank = RigAssets::build(&mut meshes, &mut materials);
        commands.insert_resource(bank);
        return; // assets visible next frame; nothing else depends on this tick
    }
    let assets = assets.unwrap();
    let now = time.elapsed_secs_f64();

    for ev in net.drain() {
        match ev {
            NetEvent::Connected => {
                info!("connected");
                if *session == Session::Connecting {
                    *session = Session::Menu;
                }
            }
            NetEvent::Msg(ServerMsg::Welcome { player_id, .. }) => {
                seams.my_id.0 = player_id;
            }
            NetEvent::Msg(ServerMsg::LobbyState {
                code,
                host_id,
                players,
                env,
                invite_url,
            }) => {
                seams.lobby_view.code = code;
                seams.lobby_view.invite_url = invite_url;
                seams.lobby_view.is_host = host_id == seams.my_id.0;
                seams.lobby_view.env = Some(env);
                seams.lobby_view.players = players
                    .iter()
                    .map(|p| (p.name.clone(), p.id == host_id, p.id == seams.my_id.0))
                    .collect();
                seams.lobby_view.status.clear();
                // fresh lobby state moves Menu→InLobby; after a match it waits
                // for the player to dismiss the stats overlay (BackToLobby)
                if matches!(*session, Session::Menu | Session::Connecting) {
                    *session = Session::InLobby;
                }
            }
            NetEvent::Msg(ServerMsg::GameStart {
                map_seed,
                env,
                your_slot,
                players,
            }) => {
                info!("game_start: slot {your_slot}, env {env:?}, seed {map_seed}");
                // Clear leftover remotes / crumples from a previous match.
                for (e, _, _, _) in zombies.iter() {
                    commands.entity(e).despawn();
                }
                for (e, _) in remotes.iter() {
                    commands.entity(e).despawn();
                }
                commands.insert_resource(CurrentMap(generate_map(env, &map_seed)));
                *session = Session::Playing { my_slot: your_slot };
                *predicted = Predicted::default();
                *seams.prev_self = PrevSelf::default();
                seams.last_stats.0 = None;
                seams.roster.0 = players
                    .iter()
                    .map(|p| (p.slot, p.name.clone(), p.slot == your_slot))
                    .collect();
                vm_kick.pending = 0;
            }
            NetEvent::Msg(ServerMsg::MatchEnd { stats }) => {
                info!(
                    "TEAM WIPED — {} zombies killed, survived {} s",
                    stats.zombies_killed,
                    stats.duration_ms / 1000
                );
                seams.sfx.0.push_back(crate::seams::Sfx::TeamWipe);
                seams.last_stats.0 = Some(stats);
                if let Some(slot) = session.my_slot() {
                    *session = Session::Ended { my_slot: slot };
                }
            }
            NetEvent::Msg(ServerMsg::Error { message }) => {
                warn!("server error: {message}");
                seams.lobby_view.status = message;
            }
            NetEvent::Msg(_) => {}
            NetEvent::Closed(reason) => {
                warn!("disconnected: {reason} — reconnecting");
                seams.lobby_view.status = format!("disconnected: {reason}");
                *session = Session::Boot;
            }
            NetEvent::Snap(snap) => {
                let (Some(my_slot), Some(map)) = (session.my_slot(), map.as_deref()) else {
                    continue;
                };

                // ── seams: visual + sound triggers derived from the wire ───
                for s in &snap.shots {
                    let end = Vec3::new(
                        dequant_pos(s.end[0]),
                        dequant_pos(s.end[1]),
                        dequant_pos(s.end[2]),
                    );
                    let from_me = s.slot == my_slot;
                    seams.fx.0.push_back(crate::seams::VisualEvent::Shot {
                        slot: s.slot,
                        end,
                        hit_kind: s.hit_kind,
                        from_me,
                    });
                    seams.sfx.0.push_back(crate::seams::Sfx::Shoot { from_me });
                    if from_me {
                        // Viewmodel recoil + muzzle flash (own Shot only).
                        vm_kick.pending = vm_kick.pending.saturating_add(1);
                    }
                    if from_me && s.hit_kind == 1 {
                        seams.sfx.0.push_back(crate::seams::Sfx::HitConfirm);
                    }
                    if from_me && s.hit_kind >= 2 {
                        seams.sfx.0.push_back(crate::seams::Sfx::KillConfirm {
                            headshot: s.hit_kind == 3,
                        });
                    }
                }
                for b in &snap.booms {
                    let pos = Vec3::new(
                        dequant_pos(b.pos[0]),
                        dequant_pos(b.pos[1]),
                        dequant_pos(b.pos[2]),
                    );
                    let dist = (pos
                        - Vec3::new(predicted.body.x, predicted.body.y, predicted.body.z))
                    .length();
                    seams
                        .fx
                        .0
                        .push_back(crate::seams::VisualEvent::Boom { pos });
                    seams.sfx.0.push_back(crate::seams::Sfx::Explosion { dist });
                }
                if let Some(me) = snap.players.iter().find(|p| p.slot == my_slot) {
                    if me.health < seams.prev_self.health {
                        seams.sfx.0.push_back(crate::seams::Sfx::Hurt);
                    }
                    if me.ammo_reserve > seams.prev_self.ammo_reserve
                        || me.grenades > seams.prev_self.grenades
                    {
                        seams.sfx.0.push_back(crate::seams::Sfx::Pickup);
                    }
                    seams.prev_self.health = me.health;
                    seams.prev_self.ammo_reserve = me.ammo_reserve;
                    seams.prev_self.grenades = me.grenades;
                }

                // ── self: adopt server truth, replay unacked inputs ────────
                if let Some(me) = snap.players.iter().find(|p| p.slot == my_slot) {
                    let rendered_before =
                        Vec3::new(predicted.body.x, predicted.body.y, predicted.body.z)
                            + predicted.error_offset;

                    predicted.body.x = dequant_pos(me.pos[0]);
                    predicted.body.y = dequant_pos(me.pos[1]);
                    predicted.body.z = dequant_pos(me.pos[2]);

                    let acked = me.last_acked_seq;
                    while predicted.pending.front().is_some_and(|i| i.seq <= acked) {
                        predicted.pending.pop_front();
                    }
                    let pending: Vec<PlayerInput> = predicted.pending.iter().copied().collect();
                    for input in &pending {
                        step_body(
                            &mut predicted.body,
                            input,
                            TICK_DT,
                            PLAYER_SPEED,
                            &map.0.walls,
                            map.0.arena_half,
                        );
                    }

                    if predicted.synced {
                        let corrected =
                            Vec3::new(predicted.body.x, predicted.body.y, predicted.body.z);
                        predicted.error_offset =
                            (rendered_before - corrected).clamp_length_max(2.0);
                    } else {
                        predicted.synced = true;
                        predicted.error_offset = Vec3::ZERO;
                    }
                }

                // ── remote players: upsert + sample ────────────────────────
                let mut seen_slots = HashSet::new();
                for p in snap.players.iter().filter(|p| p.slot != my_slot) {
                    seen_slots.insert(p.slot);
                    let pos = Vec3::new(
                        dequant_pos(p.pos[0]),
                        dequant_pos(p.pos[1]),
                        dequant_pos(p.pos[2]),
                    );
                    let yaw = dequant_yaw16(p.yaw);
                    if let Some((_, mut rp)) = remotes.iter_mut().find(|(_, rp)| rp.slot == p.slot)
                    {
                        push_sample(&mut rp.buf, now, pos, yaw);
                    } else {
                        let mut buf = VecDeque::new();
                        push_sample(&mut buf, now, pos, yaw);
                        let mut ent = commands.spawn((
                            RemotePlayer {
                                slot: p.slot,
                                buf,
                            },
                            Transform::from_translation(pos),
                            Visibility::default(),
                        ));
                        let rig = models::attach_humanoid(
                            &mut ent,
                            &assets,
                            false,
                            p.slot,
                            p.slot as u16,
                        );
                        ent.insert(rig);
                    }
                }
                for (e, rp) in remotes.iter() {
                    if !seen_slots.contains(&rp.slot) {
                        commands.entity(e).despawn();
                    }
                }

                // ── zombies: upsert + sample, crumple then despawn missing ─
                let live_ids: HashSet<u16> = snap.zombies.iter().map(|z| z.id).collect();
                for z in &snap.zombies {
                    let pos = Vec3::new(
                        dequant_pos(z.pos[0]),
                        dequant_pos(z.pos[1]),
                        dequant_pos(z.pos[2]),
                    );
                    let yaw = dequant_yaw8(z.yaw);
                    let kind = z.kind.min(2);
                    if let Some((_, mut rz, _, _)) =
                        zombies.iter_mut().find(|(_, rz, _, _)| rz.id == z.id)
                    {
                        rz.state = z.state;
                        rz.kind = kind;
                        push_sample(&mut rz.buf, now, pos, yaw);
                    } else {
                        let scale = models::kind_scale(kind);
                        let mut buf = VecDeque::new();
                        push_sample(&mut buf, now, pos, yaw);
                        let mut ent = commands.spawn((
                            RemoteZombie {
                                id: z.id,
                                kind,
                                state: z.state,
                                buf,
                            },
                            Transform::from_translation(pos).with_scale(scale),
                            Visibility::default(),
                        ));
                        let rig =
                            models::attach_humanoid(&mut ent, &assets, true, kind, z.id);
                        ent.insert(rig);
                    }
                }
                // Despawn missing: fire-and-forget crumple, then bookkeeping despawn.
                for (e, rz, tf, _rig) in zombies.iter() {
                    if !live_ids.contains(&rz.id) {
                        models::spawn_crumple(
                            &mut commands,
                            &assets,
                            tf.translation,
                            tf.rotation.to_euler(EulerRot::YXZ).0,
                            tf.scale,
                            rz.kind,
                        );
                        commands.entity(e).despawn();
                    }
                }

                seams.latest.0 = Some(snap);
            }
        }
    }
}

// ── UI intents → protocol ──────────────────────────────────────────────────

fn process_intents(
    mut queue: ResMut<crate::seams::UiQueue>,
    mut lobby_view: ResMut<crate::seams::LobbyView>,
    mut session: ResMut<Session>,
    mut net: ResMut<NetClient>,
    latest: Res<crate::seams::LatestSnapshot>,
) {
    use crate::seams::UiIntent;
    while let Some(intent) = queue.0.pop_front() {
        match intent {
            UiIntent::SetName(n) => {
                platform::persist_name(&n);
                lobby_view.name = n;
            }
            UiIntent::CreateLobby(env) => {
                send_hello(&mut net, &lobby_view.name);
                net.send_msg(&ClientMsg::CreateLobby { env });
                lobby_view.status = "creating lobby…".into();
            }
            UiIntent::JoinLobby(code) => {
                let code = code.trim().to_uppercase();
                if code.is_empty() {
                    lobby_view.status = "enter a lobby code".into();
                    continue;
                }
                send_hello(&mut net, &lobby_view.name);
                net.send_msg(&ClientMsg::JoinLobby { code });
                lobby_view.status = "joining…".into();
            }
            UiIntent::SetEnv(env) => net.send_msg(&ClientMsg::SetEnv { env }),
            UiIntent::StartGame => net.send_msg(&ClientMsg::StartGame),
            UiIntent::LeaveLobby => {
                net.send_msg(&ClientMsg::LeaveLobby);
                *session = Session::Menu;
                let name = lobby_view.name.clone();
                *lobby_view = crate::seams::LobbyView {
                    name,
                    ..Default::default()
                };
            }
            UiIntent::BackToLobby => {
                // the server already returned the roster to the lobby; we just
                // dismiss the stats overlay
                *session = Session::InLobby;
            }
            UiIntent::PauseToggle => {
                let paused = latest.0.as_ref().is_some_and(|s| s.paused);
                net.send_msg(if paused {
                    &ClientMsg::Resume
                } else {
                    &ClientMsg::Pause
                });
            }
        }
    }
}

fn send_hello(net: &mut NetClient, name: &str) {
    let name = if name.trim().is_empty() {
        "survivor"
    } else {
        name.trim()
    };
    net.send_msg(&ClientMsg::Hello {
        name: name.to_string(),
    });
}

/// The cursor is free whenever interactive UI is up; the click-to-grab flow
/// (main.rs) only applies in-match.
fn sync_cursor(session: Res<Session>, mut windows: Query<&mut bevy::window::CursorOptions>) {
    if !session.is_changed() {
        return;
    }
    let free = matches!(
        *session,
        Session::Menu
            | Session::InLobby
            | Session::Ended { .. }
            | Session::Boot
            | Session::Connecting
    );
    if free {
        for mut c in windows.iter_mut() {
            c.grab_mode = bevy::window::CursorGrabMode::None;
            c.visible = true;
        }
    }
}

fn push_sample(buf: &mut VecDeque<(f64, Vec3, f32)>, t: f64, pos: Vec3, yaw: f32) {
    buf.push_back((t, pos, yaw));
    while buf.len() > 12 {
        buf.pop_front();
    }
}

// ── local controller: input → predict → send ───────────────────────────────

#[allow(clippy::too_many_arguments)] // Bevy system params, not an API
fn fps_controller(
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    mouse: Res<AccumulatedMouseMotion>,
    time: Res<Time>,
    windows: Query<&bevy::window::CursorOptions>,
    map: Option<Res<CurrentMap>>,
    mut predicted: ResMut<Predicted>,
    mut net: ResMut<NetClient>,
) {
    let Some(map) = map else { return };

    // mouse look only while the pointer is captured
    let locked = windows
        .iter()
        .next()
        .is_some_and(|c| c.grab_mode != bevy::window::CursorGrabMode::None);
    if locked {
        let delta = mouse.delta;
        const SENS: f32 = 0.0025;
        predicted.yaw -= delta.x * SENS;
        predicted.pitch = (predicted.pitch - delta.y * SENS).clamp(-MAX_PITCH, MAX_PITCH);
    }

    // Keyboard turn (Q left / X right; E is taken by interact): mouse-free
    // aiming for accessibility and for driving the client from automation
    // (browser verification runs).
    const KEY_TURN_RATE: f32 = 2.2; // rad/s
    if keys.pressed(KeyCode::KeyQ) {
        predicted.yaw += KEY_TURN_RATE * time.delta_secs();
    }
    if keys.pressed(KeyCode::KeyX) {
        predicted.yaw -= KEY_TURN_RATE * time.delta_secs();
    }

    if !predicted.synced {
        return; // first snapshot seeds the body; don't predict from (0,0)
    }

    // fixed 30 Hz: sample intent, send, predict — one step per send
    predicted.send_accum += time.delta_secs();
    while predicted.send_accum >= TICK_DT {
        predicted.send_accum -= TICK_DT;
        let input = PlayerInput {
            seq: predicted.next_seq,
            forward: keys.pressed(KeyCode::KeyW) || keys.pressed(KeyCode::ArrowUp),
            backward: keys.pressed(KeyCode::KeyS) || keys.pressed(KeyCode::ArrowDown),
            left: keys.pressed(KeyCode::KeyA) || keys.pressed(KeyCode::ArrowLeft),
            right: keys.pressed(KeyCode::KeyD) || keys.pressed(KeyCode::ArrowRight),
            jump: keys.pressed(KeyCode::Space),
            fire: locked && buttons.pressed(MouseButton::Left),
            grenade: keys.pressed(KeyCode::KeyG),
            interact: keys.pressed(KeyCode::KeyE),
            yaw: predicted.yaw,
            pitch: predicted.pitch,
        };
        predicted.next_seq += 1;

        net.send_bin(encode_input(&input).to_vec());
        if predicted.pending.len() >= PENDING_CAP {
            predicted.pending.pop_front();
        }
        predicted.pending.push_back(input);

        step_body(
            &mut predicted.body,
            &input,
            TICK_DT,
            PLAYER_SPEED,
            &map.0.walls,
            map.0.arena_half,
        );
    }
}

fn apply_camera(
    time: Res<Time>,
    mut predicted: ResMut<Predicted>,
    mut cam: Query<&mut Transform, With<Camera3d>>,
) {
    // exponential decay of the correction offset
    let k = (0.5f32).powf(time.delta_secs() / ERROR_HALF_LIFE_S);
    predicted.error_offset *= k;

    let Ok(mut tf) = cam.single_mut() else { return };
    let eye = Vec3::new(
        predicted.body.x,
        predicted.body.y + PLAYER_EYE,
        predicted.body.z,
    ) + predicted.error_offset;
    tf.translation = eye;
    tf.rotation = Quat::from_euler(EulerRot::YXZ, predicted.yaw, predicted.pitch, 0.0);
}

// ── remote interpolation ───────────────────────────────────────────────────

fn interpolate_remotes(
    time: Res<Time>,
    mut players: Query<(&mut Transform, &mut RemotePlayer), Without<RemoteZombie>>,
    mut zombies: Query<(&mut Transform, &mut RemoteZombie), Without<RemotePlayer>>,
) {
    let target = time.elapsed_secs_f64() - INTERP_DELAY_S;
    for (mut tf, mut rp) in players.iter_mut() {
        interp_into(&mut tf, &mut rp.buf, target);
    }
    for (mut tf, mut rz) in zombies.iter_mut() {
        interp_into(&mut tf, &mut rz.buf, target);
    }
}

fn interp_into(tf: &mut Transform, buf: &mut VecDeque<(f64, Vec3, f32)>, target: f64) {
    // drop samples older than the pair bracketing `target`
    while buf.len() >= 2 && buf[1].0 <= target {
        buf.pop_front();
    }
    let (pos, yaw) = match buf.len() {
        0 => return,
        1 => (buf[0].1, buf[0].2),
        _ => {
            let (t0, p0, y0) = buf[0];
            let (t1, p1, y1) = buf[1];
            if target <= t0 {
                (p0, y0)
            } else {
                let k = ((target - t0) / (t1 - t0).max(1e-6)) as f32;
                let k = k.clamp(0.0, 1.0);
                (p0.lerp(p1, k), angle_lerp(y0, y1, k))
            }
        }
    };
    tf.translation = pos;
    tf.rotation = Quat::from_rotation_y(yaw);
}

fn angle_lerp(a: f32, b: f32, k: f32) -> f32 {
    let tau = core::f32::consts::TAU;
    let mut d = (b - a).rem_euclid(tau);
    if d > tau / 2.0 {
        d -= tau;
    }
    a + d * k
}

// ── rig animation / viewmodel / growls ─────────────────────────────────────

/// Sync attack telegraph from snapshot state, then pose limbs from motion.
///
/// Root transforms (on Remote*) and joint transforms (body/limb pivots) are
/// disjoint via Without filters, so both queries can coexist.
#[allow(clippy::type_complexity)]
fn animate_rigs(
    time: Res<Time>,
    mut zombies: Query<(&RemoteZombie, &Transform, &mut HumanoidRig), Without<RemotePlayer>>,
    mut players: Query<
        (&Transform, &mut HumanoidRig),
        (With<RemotePlayer>, Without<RemoteZombie>),
    >,
    mut joints: Query<
        &mut Transform,
        (
            Without<RemotePlayer>,
            Without<RemoteZombie>,
            Without<HumanoidRig>,
        ),
    >,
) {
    let dt = time.delta_secs().max(1e-4);

    for (rz, tf, mut rig) in zombies.iter_mut() {
        rig.attacking = rz.state == 1;
        let pose = models::compute_rig_pose(&mut rig, tf.translation, dt);
        apply_joint_pose(&mut joints, &rig, pose);
    }
    for (tf, mut rig) in players.iter_mut() {
        rig.attacking = false;
        let pose = models::compute_rig_pose(&mut rig, tf.translation, dt);
        apply_joint_pose(&mut joints, &rig, pose);
    }
}

#[allow(clippy::type_complexity)]
fn apply_joint_pose(
    joints: &mut Query<
        &mut Transform,
        (
            Without<RemotePlayer>,
            Without<RemoteZombie>,
            Without<HumanoidRig>,
        ),
    >,
    rig: &HumanoidRig,
    pose: models::RigPose,
) {
    if let Ok(mut body) = joints.get_mut(rig.body) {
        body.translation.y = pose.body_y;
    }
    for (e, angle) in [
        (rig.left_arm, pose.left_arm_x),
        (rig.right_arm, pose.right_arm_x),
        (rig.left_leg, pose.left_leg_x),
        (rig.right_leg, pose.right_leg_x),
    ] {
        if let Ok(mut tf) = joints.get_mut(e) {
            tf.rotation = Quat::from_rotation_x(angle);
        }
    }
}

fn tick_crumples(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut CrumpleFx)>,
    mut transforms: Query<&mut Transform, Without<CrumpleFx>>,
) {
    let dt = time.delta_secs();
    for (e, mut fx) in q.iter_mut() {
        let Ok(mut body_tf) = transforms.get_mut(fx.body) else {
            commands.entity(e).despawn();
            continue;
        };
        if models::tick_crumple(&mut fx, &mut body_tf, dt) {
            commands.entity(e).despawn();
        }
    }
}

/// Spawn the FP rifle once per match when the camera is available.
fn ensure_viewmodel(
    mut commands: Commands,
    assets: Option<Res<RigAssets>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    cam: Query<Entity, With<Camera3d>>,
    existing: Query<Entity, With<Viewmodel>>,
) {
    if !existing.is_empty() {
        return;
    }
    let Some(assets) = assets else { return };
    let Ok(camera) = cam.single() else { return };
    models::spawn_viewmodel(&mut commands, camera, &assets, &mut materials);
}

fn tick_viewmodel_sys(
    time: Res<Time>,
    mut kick: ResMut<ViewmodelKick>,
    mut vm_q: Query<(&mut Viewmodel, &mut Transform)>,
    mut vis_q: Query<&mut Visibility, Without<Viewmodel>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let Ok((mut vm, mut tf)) = vm_q.single_mut() else {
        return;
    };
    while kick.pending > 0 {
        models::viewmodel_on_shot(&mut vm);
        kick.pending -= 1;
    }
    let flash_vis = vis_q.get_mut(vm.flash_entity).ok();
    let flash_mat = materials.get_mut(&vm.flash_mat);
    // tick_viewmodel wants Option<&mut T>; reborrow carefully
    match (flash_vis, flash_mat) {
        (Some(mut vis), Some(mut mat)) => {
            models::tick_viewmodel(
                &mut vm,
                &mut tf,
                Some(&mut vis),
                Some(&mut *mat),
                time.delta_secs(),
            );
        }
        (Some(mut vis), None) => {
            models::tick_viewmodel(&mut vm, &mut tf, Some(&mut vis), None, time.delta_secs());
        }
        (None, Some(mut mat)) => {
            models::tick_viewmodel(
                &mut vm,
                &mut tf,
                None,
                Some(&mut *mat),
                time.delta_secs(),
            );
        }
        (None, None) => {
            models::tick_viewmodel(&mut vm, &mut tf, None, None, time.delta_secs());
        }
    }
}

/// Nearby zombies growl occasionally (deterministic id+time hash, no rand).
/// Rising-edge per (id, half-second bucket) so we don't enqueue every frame.
fn proximity_growls(
    time: Res<Time>,
    predicted: Res<Predicted>,
    zombies: Query<(&RemoteZombie, &Transform)>,
    mut sfx: ResMut<crate::seams::SfxQueue>,
    mut mem: ResMut<GrowlMemory>,
) {
    if !predicted.synced {
        return;
    }
    let now = time.elapsed_secs_f64();
    let bucket = (now * 2.0).floor() as u32;
    if mem.bucket != bucket {
        mem.bucket = bucket;
        mem.fired.clear();
    }
    let me = Vec3::new(predicted.body.x, predicted.body.y, predicted.body.z);
    for (rz, tf) in zombies.iter() {
        let dist = me.distance(tf.translation);
        if dist > GROWL_RANGE {
            continue;
        }
        if !models::should_growl(rz.id, now) {
            continue;
        }
        if !mem.fired.insert(rz.id) {
            continue; // already growled this bucket
        }
        // Distance-scale volume; closer = louder.
        let volume = (1.0 - dist / GROWL_RANGE).clamp(0.12, 0.85);
        sfx.0
            .push_back(crate::seams::Sfx::Growl { volume });
    }
}

/// Despawn match-only visuals when leaving Playing/Ended.
fn cleanup_match_visuals(
    session: Res<Session>,
    mut commands: Commands,
    players: Query<Entity, With<RemotePlayer>>,
    zombies: Query<Entity, With<RemoteZombie>>,
    crumples: Query<Entity, With<CrumpleFx>>,
    viewmodels: Query<Entity, With<Viewmodel>>,
) {
    if !session.is_changed() {
        return;
    }
    let keep = matches!(
        *session,
        Session::Playing { .. } | Session::Ended { .. }
    );
    if keep {
        return;
    }
    for e in players
        .iter()
        .chain(zombies.iter())
        .chain(crumples.iter())
        .chain(viewmodels.iter())
    {
        commands.entity(e).despawn();
    }
}
