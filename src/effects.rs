//! Hit-feedback FX: pulse-and-flash on damaged entities, muzzle flashes,
//! and short-lived hit particles. Plus the shared `EffectMeshes` cache so
//! every bullet / enemy / beam reuses the same mesh handle (one draw call
//! per part rather than one per spawn).

use bevy::prelude::*;
use bevy::render::view::RenderLayers;
use rand::Rng;

use crate::balance::{FLASH_DURATION, HIT_D, HIT_K, HIT_PULSE, PLAY_LAYER};
use crate::components::Velocity;
use crate::palette::PaletteMaterials;

// ---------- Cached meshes ----------

/// Shared mesh handles for short-lived FX + per-shot/per-enemy primitives.
/// Storing a single handle per shape lets Bevy batch all entities that share
/// it into one draw call, and skips per-spawn asset alloc + GPU upload churn.
#[derive(Resource)]
pub struct EffectMeshes {
    pub muzzle_flash: Handle<Mesh>,
    pub particle: Handle<Mesh>,
    pub bullet_friendly_outer: Handle<Mesh>,
    pub bullet_friendly_inner: Handle<Mesh>,
    pub bullet_enemy_outer: Handle<Mesh>,
    pub bullet_enemy_inner: Handle<Mesh>,
    pub enemy_body: Handle<Mesh>,
    pub enemy_turret_base: Handle<Mesh>,
    pub enemy_turret_barrel: Handle<Mesh>,
    /// Bright dot on the bow of a Bomber — visual signal of the threat.
    pub bomber_warhead: Handle<Mesh>,
    /// Cached turret base + barrel for ally ships. Sized between player and
    /// enemy turrets; shared across every `ShipClass`.
    pub ally_turret_base: Handle<Mesh>,
    pub ally_turret_barrel: Handle<Mesh>,
    /// Small twin-bullet meshes used by carrier-launched planes. Sized
    /// noticeably smaller than the player's bullets so plane MG fire
    /// reads as light-arms vs the ship's main batteries.
    pub bullet_plane_outer: Handle<Mesh>,
    pub bullet_plane_inner: Handle<Mesh>,
    /// Submarine homing-missile body. Longer + skinnier than a regular
    /// bullet so the silhouette reads as a missile in flight.
    pub bullet_missile_outer: Handle<Mesh>,
    pub bullet_missile_inner: Handle<Mesh>,
    /// Sea-mine meshes — dark outer shell + small red warning dot.
    pub mine_outer: Handle<Mesh>,
    pub mine_inner: Handle<Mesh>,
    /// Long thin rectangle reused by every railgun beam. Width 1, length
    /// `BEAM_LENGTH` along local +Y. Width is animated via Transform.scale.x.
    pub beam: Handle<Mesh>,
}

// ---------- Components ----------

/// Short-lived burst placed at a turret muzzle when it fires; fades + shrinks.
#[derive(Component)]
pub struct MuzzleFlash {
    pub life: f32,
    pub max_life: f32,
}

/// Hit particle that drifts and fades after an impact.
#[derive(Component)]
pub struct HitParticle {
    pub life: f32,
    pub max_life: f32,
    /// Per-particle base scale so the fade keeps the random spawn variation.
    pub base_scale: f32,
}

/// Per-entity damped-spring pulse + brief white flash on hit.
/// `a = -k(x - 1) - dv` snaps the spring back to rest scale 1.0; `pulse()`
/// adds an impulse. The render scale also gets multiplied by `rest_scale`
/// so variant-sized enemies don't snap back to 1.0 between hits.
#[derive(Component)]
pub struct HitFx {
    spring_x: f32,
    spring_v: f32,
    flash_remaining: f32,
    base_material: Handle<ColorMaterial>,
    rest_scale: f32,
}

impl HitFx {
    pub fn new(base_material: Handle<ColorMaterial>) -> Self {
        Self {
            spring_x: 1.0,
            spring_v: 0.0,
            flash_remaining: 0.0,
            base_material,
            rest_scale: 1.0,
        }
    }
    pub fn with_rest_scale(mut self, s: f32) -> Self {
        self.rest_scale = s;
        self
    }
    pub fn pulse(&mut self) {
        self.spring_x += HIT_PULSE;
        self.flash_remaining = FLASH_DURATION;
    }
}

// ---------- Particle / flash spawn helper ----------

/// Spawn `count` short-lived streak particles at `pos`, exploding in a circle
/// at `speed` units/sec with random size variation.
pub fn spawn_hit_particles(
    commands: &mut Commands,
    em: &EffectMeshes,
    mat: &Handle<ColorMaterial>,
    pos: Vec2,
    count: u32,
    speed: f32,
    rng: &mut rand::rngs::ThreadRng,
) {
    use std::f32::consts::TAU;
    for _ in 0..count {
        let a = rng.gen_range(0.0..TAU);
        let s = rng.gen_range(speed * 0.4..speed);
        let v = Vec2::new(a.cos(), a.sin()) * s;
        let life = rng.gen_range(0.3..0.6);
        // Particle mesh's long axis is +Y, so convert (cos,sin) → angle in
        // our 0=+Y / +PI/2=-X frame: rot = (-vx).atan2(vy).
        let rot = (-v.x).atan2(v.y);
        let scale = rng.gen_range(0.8..1.4);
        commands.spawn((
            Mesh2d(em.particle.clone()),
            MeshMaterial2d(mat.clone()),
            Transform {
                translation: Vec3::new(pos.x, pos.y, 5.5),
                rotation: Quat::from_rotation_z(rot),
                scale: Vec3::new(scale, scale, 1.0),
            },
            HitParticle { life, max_life: life, base_scale: scale },
            Velocity(v),
            RenderLayers::layer(PLAY_LAYER),
        ));
    }
}

// ---------- Tickers ----------

pub fn update_muzzle_flashes(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut Transform, &mut MuzzleFlash)>,
) {
    let dt = time.delta_secs();
    for (e, mut tf, mut f) in &mut q {
        f.life -= dt;
        if f.life <= 0.0 {
            commands.entity(e).despawn();
            continue;
        }
        let t = (f.life / f.max_life).clamp(0.0, 1.0);
        // Pop in then ease out: scale peaks at spawn, shrinks to 0.4 by end.
        let s = 0.4 + 0.7 * t;
        tf.scale.x = s;
        tf.scale.y = s;
        tf.scale.z = 1.0;
    }
}

pub fn tick_hit_fx(time: Res<Time>, mut q: Query<(&mut HitFx, &mut Transform)>) {
    let dt = time.delta_secs();
    for (mut fx, mut tf) in &mut q {
        let a = -HIT_K * (fx.spring_x - 1.0) - HIT_D * fx.spring_v;
        fx.spring_v += a * dt;
        fx.spring_x += fx.spring_v * dt;
        if fx.flash_remaining > 0.0 {
            fx.flash_remaining = (fx.flash_remaining - dt).max(0.0);
        }
        // Multiplied by rest_scale so variant-scaled enemies don't reset to 1.0.
        let s = fx.spring_x.max(0.0) * fx.rest_scale;
        tf.scale.x = s;
        tf.scale.y = s;
        tf.scale.z = 1.0;
    }
}

pub fn apply_hit_fx_visuals(
    pm: Option<Res<PaletteMaterials>>,
    mut q: Query<(&HitFx, &mut MeshMaterial2d<ColorMaterial>)>,
) {
    let Some(pm) = pm else { return; };
    for (fx, mut mat) in &mut q {
        let want = if fx.flash_remaining > 0.0 { &pm.flash } else { &fx.base_material };
        if mat.0 != *want {
            mat.0 = want.clone();
        }
    }
}

pub fn update_hit_particles(
    time: Res<Time>,
    mut commands: Commands,
    mut q: Query<(Entity, &mut Transform, &mut HitParticle, &mut Velocity)>,
) {
    let dt = time.delta_secs();
    let drag = 0.88_f32.powf(60.0 * dt); // ~12% velocity loss per frame at 60Hz
    for (e, mut tf, mut p, mut v) in &mut q {
        p.life -= dt;
        if p.life <= 0.0 {
            commands.entity(e).despawn();
            continue;
        }
        v.0 *= drag;
        let t = (p.life / p.max_life).clamp(0.0, 1.0);
        let s = p.base_scale * (0.3 + 0.7 * t);
        tf.scale.x = s;
        tf.scale.y = s;
        tf.scale.z = 1.0;
    }
}
