//! `HostServices` is object-safe and implementable with the exact frozen signatures.

use glam::Vec3;
use re_types::{
    ActionFrame, AudioCmd, BusLevel, CharacterState, EntityId, GroupMask, HostError, HostResult,
    HostServices, RagdollMode, RagdollState, RayHit, VehicleInput, VehicleState,
};

/// A minimal host that records what scripts asked for.
#[derive(Default)]
struct RecordingHost {
    actions: ActionFrame,
    log: Vec<String>,
    restart: bool,
}

impl HostServices for RecordingHost {
    fn raycast(&self, origin: Vec3, dir: Vec3, max_dist: f32, mask: GroupMask) -> Option<RayHit> {
        // A ground plane at y = 0 in group 0.
        if !mask.contains_group(0) || dir.y >= 0.0 || origin.y < 0.0 {
            return None;
        }
        let distance = origin.y / -dir.y;
        (distance <= max_dist).then(|| RayHit {
            entity: EntityId(1),
            point: origin + dir * distance,
            normal: Vec3::Y,
            distance,
        })
    }

    fn apply_impulse(&mut self, entity: EntityId, impulse: Vec3) -> HostResult<()> {
        self.log.push(format!("impulse {entity} {impulse}"));
        Ok(())
    }

    fn character_move(
        &mut self,
        entity: EntityId,
        desired_velocity: Vec3,
        jump: bool,
    ) -> HostResult<()> {
        self.log
            .push(format!("move {entity} {desired_velocity} {jump}"));
        Ok(())
    }

    fn character_state(&self, entity: EntityId) -> HostResult<CharacterState> {
        Err(HostError::new(
            "RE0510",
            format!("{entity} is not a character"),
        ))
    }

    fn body_velocity(&self, _entity: EntityId) -> HostResult<(Vec3, Vec3)> {
        Ok((Vec3::X, Vec3::ZERO))
    }

    fn vehicle_drive(&mut self, entity: EntityId, input: VehicleInput) -> HostResult<()> {
        self.log
            .push(format!("drive {} {}", entity.0, input.throttle));
        Ok(())
    }

    fn vehicle_state(&self, _entity: EntityId) -> HostResult<VehicleState> {
        Ok(VehicleState::default())
    }

    fn ragdoll_mode(&mut self, entity: EntityId, mode: RagdollMode) -> HostResult<()> {
        self.log.push(format!("ragdoll {} {mode:?}", entity.0));
        Ok(())
    }

    fn ragdoll_state(&self, _entity: EntityId) -> HostResult<RagdollState> {
        Ok(RagdollState::default())
    }

    fn ragdoll_impulse(&mut self, entity: EntityId, joint: &str, impulse: Vec3) -> HostResult<()> {
        self.log.push(format!("hit {} {joint} {impulse}", entity.0));
        Ok(())
    }

    fn play_sound(&mut self, clip: &str, at: Option<Vec3>, volume: f32) -> HostResult<()> {
        self.log.push(format!("sound {clip} {at:?} {volume}"));
        Ok(())
    }

    fn audio_mix(&mut self, cmd: AudioCmd) -> HostResult<()> {
        self.log.push(format!("mix {cmd:?}"));
        Ok(())
    }

    fn audio_level(&self, bus: &str) -> HostResult<BusLevel> {
        Ok(BusLevel {
            bus: bus.into(),
            peak_db: -60.0,
            rms_db: -60.0,
        })
    }

    fn actions(&self) -> &ActionFrame {
        &self.actions
    }

    fn set_hud_text(&mut self, entity: EntityId, text: &str) -> HostResult<()> {
        self.log.push(format!("hud {entity} {text}"));
        Ok(())
    }

    fn request_screenshot(&mut self, path: &str) -> HostResult<()> {
        self.log.push(format!("screenshot {path}"));
        Ok(())
    }

    fn regenerate(&mut self, source_id: &str, seed: i64) -> HostResult<()> {
        self.log.push(format!("regenerate {source_id} {seed}"));
        Ok(())
    }

    fn request_restart(&mut self) {
        self.restart = true;
    }
}

/// What a script binding does: it only ever sees `&mut dyn HostServices`.
fn script_frame(host: &mut dyn HostServices) -> Result<(), HostError> {
    let hit = host
        .raycast(Vec3::new(0.0, 2.0, 0.0), Vec3::NEG_Y, 10.0, GroupMask::ALL)
        .expect("ground below");
    assert_eq!(hit.distance, 2.0);
    let jump = host.actions().pressed("jump");
    host.character_move(EntityId(5), Vec3::X, jump)?;
    host.apply_impulse(EntityId(6), Vec3::Y)?;
    let (linear, _angular) = host.body_velocity(EntityId(6))?;
    assert_eq!(linear, Vec3::X);
    host.play_sound("assets/sfx/pickup.wav", Some(hit.point), 1.0)?;
    host.set_hud_text(EntityId(7), "3 / 10")?;
    host.request_screenshot("out/shot.png")?;
    host.regenerate("island", 99)?;
    host.request_restart();
    host.character_state(EntityId(6)).map(|_| ())
}

#[test]
fn a_script_drives_the_host_through_the_trait_object() {
    let mut host = RecordingHost::default();
    let err = script_frame(&mut host).expect_err("entity 6 is not a character");
    assert_eq!(err.code, "RE0510");
    assert!(host.restart);
    assert_eq!(host.log.len(), 6);
    assert_eq!(host.log[5], "regenerate island 99");
}
