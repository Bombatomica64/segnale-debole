//! Milestone 34 "Procedural pixel-art projectile textures": every player shot
//! and enemy bolt draws with pre-built textured materials and spins / tumbles
//! cleanly in flight (headless, no GPU).

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;

use segnale_debole::assets_gen::ProjectileTextures;
use segnale_debole::build_headless_test_app;
use segnale_debole::enemies::{spawn_enemy_projectile, Enemy, EnemyProjectile};
use segnale_debole::fx::{needle_collar_transform, BurstColor, FxAssets};
use segnale_debole::level::{LevelEntity, LevelGrid};
use segnale_debole::physics::raycast_grid;
use segnale_debole::player::{Player, PLAYER_EYE_HEIGHT};
use segnale_debole::state::{start_run, AppState, PlayerModifiers, RunStats};
use segnale_debole::weapons::{rocket_warhead_uv, spawn_parried_projectile, spawn_rocket, BoltAssets, ParryAssets, PlayerProjectile, ProjectileKind, RocketAssets};

/// `(rotation, player kind, enemy (color, velocity))` of one shot in flight.
type Shot = (Quat, Option<ProjectileKind>, Option<(BurstColor, Vec3)>);

fn step(app: &mut App, frames: usize) {
    for _ in 0..frames {
        app.update();
    }
}

fn arena(seed: u64) -> App {
    let mut app = build_headless_test_app();
    app.world_mut()
        .run_system_once(move |mut run: ResMut<RunStats>, mut mods: ResMut<PlayerModifiers>, mut next: ResMut<NextState<AppState>>| {
            start_run(&mut run, &mut mods, &mut next, seed);
        })
        .unwrap();
    step(&mut app, 3);
    assert_eq!(*app.world().resource::<State<AppState>>().get(), AppState::Playing);
    let mut q = app.world_mut().query::<&mut Player>();
    let mut p = q.single_mut(app.world_mut()).unwrap();
    p.invuln_timer = 1.0e6;
    let demons: Vec<Entity> = app.world_mut().query_filtered::<Entity, With<Enemy>>().iter(app.world()).collect();
    for e in demons {
        app.world_mut().despawn(e);
    }
    step(&mut app, 2);
    app
}

fn eye(app: &mut App) -> Vec3 {
    let mut q = app.world_mut().query_filtered::<&Transform, With<Player>>();
    q.single(app.world()).unwrap().translation + Vec3::Y * PLAYER_EYE_HEIGHT
}

/// Horizontal direction from the eye with the most free space.
fn open_dir(app: &mut App) -> (Vec3, f32) {
    let grid = app.world().resource::<LevelGrid>().clone();
    let e = eye(app);
    (0..16)
        .map(|i| {
            let a = i as f32 * std::f32::consts::TAU / 16.0;
            let d = Vec3::new(a.cos(), 0.0, a.sin());
            (d, raycast_grid(&grid, e, d, 40.0).map_or(40.0, |h| h.distance))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap()
}

fn material(app: &App, h: &Handle<StandardMaterial>) -> StandardMaterial {
    app.world().resource::<Assets<StandardMaterial>>().get(h).expect("material exists").clone()
}

fn assert_textured(app: &App, h: &Handle<StandardMaterial>, glowing: bool, what: &str) {
    let m = material(app, h);
    assert!(m.base_color_texture.is_some(), "{what}: base_color_texture");
    if glowing {
        assert!(m.emissive_texture.is_some(), "{what}: emissive_texture");
    }
}

#[test]
fn every_projectile_material_is_pixel_textured() {
    let app = build_headless_test_app();
    let tex = app.world().resource::<ProjectileTextures>().clone();
    let images = app.world().resource::<Assets<Image>>();
    assert!(images.get(&tex.plasma_bolt).is_some() && images.get(&tex.energy_ring).is_some());

    let bolts = app.world().resource::<BoltAssets>().clone();
    assert_textured(&app, &bolts.core_mat, true, "plasma core");
    assert_textured(&app, &bolts.halo_mat, true, "plasma halo");
    assert_eq!(material(&app, &bolts.halo_mat).alpha_mode, AlphaMode::Add);
    assert_eq!(material(&app, &bolts.core_mat).base_color_texture.unwrap().id(), tex.plasma_bolt.id());

    let parry = app.world().resource::<ParryAssets>().clone();
    assert_textured(&app, &parry.core_mat, true, "parry core");
    assert_textured(&app, &parry.ring_mat, true, "parry ring");
    assert_eq!(material(&app, &parry.ring_mat).base_color_texture.unwrap().id(), tex.energy_ring.id());

    let rockets = app.world().resource::<RocketAssets>().clone();
    let tex_ids: Vec<_> = rockets.models.iter().filter_map(|(_, m)| material(&app, m).base_color_texture.map(|t| t.id())).collect();
    assert!(tex_ids.contains(&tex.rocket_body.id()), "rocket casing");
    assert!(tex_ids.contains(&tex.rocket_flame.id()), "rocket flame");
    let casing = rockets.models.iter().map(|(_, m)| material(&app, m)).find(|m| !m.unlit && m.base_color_texture.is_some()).unwrap();
    assert!(casing.metallic < 0.5, "painted casing keeps a diffuse response: {}", casing.metallic);
    // The glowing warhead samples only the warhead rows of the body texture.
    let warhead = rockets.models.iter().map(|(_, m)| material(&app, m)).find(|m| m.unlit && m.base_color_texture.as_ref().map(|t| t.id()) == Some(tex.rocket_body.id())).unwrap();
    assert_eq!(warhead.uv_transform, rocket_warhead_uv());
    assert!(warhead.uv_transform.translation.y > 0.5 && warhead.uv_transform.matrix2.y_axis.y < 0.5);

    let fx = app.world().resource::<FxAssets>().clone();
    assert_textured(&app, &fx.tracer, true, "tracer");
    assert_textured(&app, &fx.tracer_core, true, "tracer core");
    let mut cores = std::collections::HashSet::new();
    for c in [BurstColor::Fire, BurstColor::Cyan, BurstColor::Green, BurstColor::Spark] {
        let look = fx.shot_look(c);
        assert_textured(&app, &look.core, true, "shot core");
        assert_textured(&app, &look.halo, true, "shot halo");
        assert_textured(&app, &look.ring, true, "shot ring");
        assert!(cores.insert(material(&app, &look.core).base_color_texture.unwrap().id()), "{c:?} has its own texture");
    }
    assert_eq!(fx.shot_look(BurstColor::Gore).core.id(), fx.shot_look(BurstColor::Fire).core.id());
}

#[test]
fn projectiles_spin_and_tumble_without_nans() {
    let mut app = arena(341);
    let (dir, free) = open_dir(&mut app);
    assert!(free > 10.0, "need an open lane: {free}");
    let start = eye(&mut app) + dir * 1.5;
    let (parry, bolts, rockets, fx) = {
        let w = app.world();
        (w.resource::<ParryAssets>().clone(), w.resource::<BoltAssets>().clone(), w.resource::<RocketAssets>().clone(), w.resource::<FxAssets>().clone())
    };
    app.world_mut()
        .run_system_once(move |mut commands: Commands| {
            commands.spawn((
                    LevelEntity,
                    PlayerProjectile::plasma(dir * 30.0, 1.0),
                    Mesh3d(bolts.core_mesh.clone()),
                    MeshMaterial3d(bolts.core_mat.clone()),
                    Transform::from_translation(start).with_rotation(Quat::from_rotation_arc(Vec3::Y, dir)),
            ));
            spawn_rocket(&mut commands, &rockets, start + Vec3::Y * 0.3, dir, Vec3::ZERO, 1.0);
            spawn_parried_projectile(&mut commands, &parry, start - Vec3::Y * 0.3, dir, 20.0);
        })
        .unwrap();
    let colors = [BurstColor::Fire, BurstColor::Cyan, BurstColor::Green, BurstColor::Gore, BurstColor::Spark];
    let up = Vec3::Y * 0.5;
    app.world_mut()
        .run_system_once(move |mut commands: Commands| {
            for (i, c) in colors.iter().enumerate() {
                let vel = (dir + Vec3::Y * 0.05).normalize() * 4.0;
                spawn_enemy_projectile(&mut commands, &fx, start + dir * (1.0 + i as f32) + up, vel, 5.0, 0.25, *c);
            }
        })
        .unwrap();
    app.update();
    let snapshot = |app: &mut App| -> Vec<Shot> {
        let mut out = Vec::new();
        let mut q = app.world_mut().query::<(&Transform, Option<&PlayerProjectile>, Option<&EnemyProjectile>)>();
        for (tf, pp, ep) in q.iter(app.world()) {
            if pp.is_some() || ep.is_some() {
                out.push((tf.rotation, pp.map(|p| p.kind), ep.map(|e| (e.color, e.vel))));
            }
        }
        out
    };
    // Rail lances wear one shock-collar ring (the rail look's ring material)
    // at the shoulder, clear of the sheath.
    let fx = app.world().resource::<FxAssets>().clone();
    let mut lances = app.world_mut().query::<(&EnemyProjectile, &Children)>();
    let mut parts = app.world_mut().query::<(&Mesh3d, &MeshMaterial3d<StandardMaterial>, &Transform)>();
    let mut collars = 0;
    for (p, children) in lances.iter(app.world()) {
        if p.color != BurstColor::Spark {
            continue;
        }
        for child in children.iter() {
            let Ok((mesh, mat, tf)) = parts.get(app.world(), child) else { continue };
            if mesh.0.id() == fx.ring.id() {
                collars += 1;
                assert_eq!(mat.0.id(), fx.shot_look(BurstColor::Spark).ring.id());
                assert_eq!(*tf, needle_collar_transform());
                assert!(tf.scale.x * 0.94 > 0.55, "collar clears the 0.55 sheath");
            }
        }
    }
    assert_eq!(collars, 1, "one collar per rail lance");
    let before = snapshot(&mut app);
    assert_eq!(before.len(), 3 + colors.len(), "all shots in flight: {before:?}");
    // Rail lances spawn aligned with their velocity.
    for (rot, _, ep) in &before {
        if let Some((BurstColor::Spark, vel)) = ep {
            assert!((*rot * Vec3::Y).dot(vel.normalize()) > 0.999, "rail lance aligned with flight");
        }
    }
    step(&mut app, 3);
    let after = snapshot(&mut app);
    assert_eq!(after.len(), before.len(), "nothing hit anything");
    for ((r0, pk, ep), (r1, _, _)) in before.iter().zip(&after) {
        assert!(r1.is_finite() && r1.is_normalized(), "{pk:?}/{ep:?} rotation {r1:?}");
        assert!(r0.angle_between(*r1) > 0.05, "{pk:?}/{ep:?} spins in flight");
        if pk.is_some() {
            assert!((*r1 * Vec3::Y).dot(dir) > 0.999, "{pk:?} spins about its flight axis");
        }
        if let Some((BurstColor::Spark, vel)) = ep {
            assert!((*r1 * Vec3::Y).dot(vel.normalize()) > 0.999, "rail lance stays aligned");
        }
    }
    assert!(after.iter().any(|(_, k, _)| *k == Some(ProjectileKind::Rocket)));
}
