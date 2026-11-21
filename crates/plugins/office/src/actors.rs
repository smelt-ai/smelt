//! 角色动作由会话事件启动，按时刻取样。走动、完成和提醒都有终点，空闲不维持帧循环。

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use smelt_plugin_api::RuntimeKey;

use crate::layout;
use crate::model::{Activity, Room};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Position {
    pub(crate) x: f32,
    pub(crate) y: f32,
}

impl Position {
    const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
    fn distance(self, other: Self) -> f32 {
        (self.x - other.x).abs() + (self.y - other.y).abs()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Facing {
    North,
    South,
    East,
    West,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pose {
    Walk,
    Think,
    Type,
    Wait,
    Error,
    Rest,
    Celebrate,
    Offline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cue {
    Approval,
    Input,
    Error,
    Completed,
    Sleep,
    Thought,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Frame {
    pub(crate) position: Position,
    pub(crate) facing: Facing,
    pub(crate) pose: Pose,
    pub(crate) step: u8,
    pub(crate) cue: Option<Cue>,
    pub(crate) cue_lift: i32,
    pub(crate) animating: bool,
}

fn destination(activity: Activity) -> Position {
    let (x, y) = match activity {
        Activity::Thinking | Activity::Executing => layout::WORK,
        Activity::Approval | Activity::Input | Activity::Failed => layout::WAIT,
        Activity::Idle | Activity::Offline => layout::REST,
    };
    Position::new(x, y)
}

impl Frame {
    pub(crate) fn settled(activity: Activity) -> Self {
        Self::at(activity, destination(activity))
    }

    fn at(activity: Activity, position: Position) -> Self {
        let (pose, cue) = match activity {
            Activity::Thinking => (Pose::Think, Some(Cue::Thought)),
            Activity::Executing => (Pose::Type, None),
            Activity::Approval => (Pose::Wait, Some(Cue::Approval)),
            Activity::Input => (Pose::Wait, Some(Cue::Input)),
            Activity::Failed => (Pose::Error, Some(Cue::Error)),
            Activity::Idle => (Pose::Rest, Some(Cue::Sleep)),
            Activity::Offline => (Pose::Offline, None),
        };
        Self {
            position,
            facing: if activity.running() {
                Facing::North
            } else {
                Facing::South
            },
            pose,
            cue,
            step: 0,
            cue_lift: 0,
            animating: false,
        }
    }
}

/// 工位前的通道：先走到通道，再横向走，最后进工位/休息椅；不穿过桌面。
struct Route {
    points: [Position; 4],
    distance: f32,
}

impl Route {
    fn new(from: Position, to: Position) -> Self {
        let points = if from == to {
            [to; 4]
        } else if from.x == to.x {
            [from, from, to, to]
        } else {
            [
                from,
                Position::new(from.x, layout::AISLE_Y),
                Position::new(to.x, layout::AISLE_Y),
                to,
            ]
        };
        let distance = points
            .windows(2)
            .map(|pair| pair[0].distance(pair[1]))
            .sum();
        Self { points, distance }
    }

    fn duration(&self) -> f32 {
        self.distance / 36.
    }

    fn sample(&self, seconds: f32) -> (Position, Facing) {
        let mut remaining = seconds.max(0.) * 36.;
        for pair in self.points.windows(2) {
            let distance = pair[0].distance(pair[1]);
            if distance == 0. {
                continue;
            }
            if remaining < distance {
                let t = remaining / distance;
                let facing = if pair[1].x > pair[0].x {
                    Facing::East
                } else if pair[1].x < pair[0].x {
                    Facing::West
                } else if pair[1].y > pair[0].y {
                    Facing::South
                } else {
                    Facing::North
                };
                return (
                    Position::new(
                        pair[0].x + (pair[1].x - pair[0].x) * t,
                        pair[0].y + (pair[1].y - pair[0].y) * t,
                    ),
                    facing,
                );
            }
            remaining -= distance;
        }
        (self.points[3], Facing::South)
    }
}

struct Actor {
    activity: Activity,
    route: Route,
    changed_at: Instant,
    celebrate: bool,
}

impl Actor {
    fn new(activity: Activity, now: Instant) -> Self {
        let to = destination(activity);
        let from = if activity == Activity::Offline {
            to
        } else {
            Position::new(layout::ENTRANCE.0, layout::ENTRANCE.1)
        };
        Self {
            activity,
            route: Route::new(from, to),
            changed_at: now,
            celebrate: false,
        }
    }

    fn update(&mut self, activity: Activity, completed: bool, now: Instant) {
        if activity == self.activity {
            return;
        }
        let from = self.sample(now, true).position;
        self.celebrate = self.activity.running() && activity == Activity::Idle && completed;
        self.activity = activity;
        self.route = Route::new(
            from,
            if activity == Activity::Offline {
                from
            } else {
                destination(activity)
            },
        );
        self.changed_at = now;
    }

    fn sample(&self, now: Instant, animate: bool) -> Frame {
        let mut frame = Frame::at(self.activity, self.route.points[3]);
        if !animate || self.activity == Activity::Offline {
            return frame;
        }
        let elapsed = now.saturating_duration_since(self.changed_at).as_secs_f32();
        let pause = if self.celebrate { 0.65 } else { 0. };
        frame.step = ((elapsed * 8.) as u64 % 8) as u8;
        if elapsed < pause {
            frame.position = self.route.points[0];
            frame.pose = Pose::Celebrate;
            frame.cue = Some(Cue::Completed);
            frame.cue_lift = (elapsed * 5.).round() as i32;
            frame.animating = true;
            return frame;
        }
        let travel = elapsed - pause;
        if travel < self.route.duration() {
            (frame.position, frame.facing) = self.route.sample(travel);
            frame.pose = Pose::Walk;
            frame.cue = None;
            frame.animating = true;
            return frame;
        }
        let settled_for = travel - self.route.duration();
        // 提醒弹出和坐下只播一次。之后等待审批/输入、出错、休息均静态。
        let settling = settled_for < 0.9;
        frame.animating = self.activity.running() || settling;
        if self.activity.needs_you() && settling {
            frame.cue_lift =
                (2. * (settled_for * 10.).sin().abs() * (1. - settled_for / 0.9)).round() as i32;
        }
        if !frame.animating {
            frame.step = 0;
        }
        frame
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    machine: RuntimeKey,
    conversation: String,
}

#[derive(Default)]
pub(crate) struct Cast {
    actors: HashMap<Key, Actor>,
}

impl Cast {
    pub(crate) fn sync(&mut self, rooms: &[Room], now: Instant) {
        let mut current = HashSet::new();
        for room in rooms {
            for worker in &room.workers {
                let key = Key {
                    machine: room.machine.clone(),
                    conversation: worker.conversation.clone(),
                };
                self.actors
                    .entry(key.clone())
                    .and_modify(|actor| actor.update(worker.activity, worker.completed, now))
                    .or_insert_with(|| Actor::new(worker.activity, now));
                current.insert(key);
            }
        }
        self.actors.retain(|key, _| current.contains(key));
    }

    pub(crate) fn frame(
        &self,
        machine: &RuntimeKey,
        conversation: &str,
        activity: Activity,
        now: Instant,
        animate: bool,
    ) -> Frame {
        self.actors
            .get(&Key {
                machine: machine.clone(),
                conversation: conversation.into(),
            })
            .map_or_else(
                || Frame::settled(activity),
                |actor| actor.sample(now, animate),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn arrivals_walk_instead_of_teleporting_then_use_the_correct_work_pose() {
        let now = Instant::now();
        let actor = Actor::new(Activity::Executing, now);
        let entering = actor.sample(now, true);
        let walking = actor.sample(now + Duration::from_millis(500), true);
        let working = actor.sample(now + Duration::from_secs(3), true);
        assert_eq!(entering.pose, Pose::Walk);
        assert_eq!(walking.pose, Pose::Walk);
        assert_ne!(walking.position, entering.position);
        assert_ne!(walking.position, working.position);
        assert_eq!(working.position, destination(Activity::Executing));
        assert_eq!(working.pose, Pose::Type);
        assert!(working.animating);
    }

    #[test]
    fn completion_celebrates_walks_to_the_sofa_then_stops() {
        let start = Instant::now();
        let now = start + Duration::from_secs(3);
        let mut actor = Actor::new(Activity::Executing, start);
        actor.update(Activity::Idle, true, now);
        assert_eq!(actor.sample(now, true).pose, Pose::Celebrate);
        assert_eq!(
            actor.sample(now + Duration::from_secs(1), true).pose,
            Pose::Walk
        );
        let resting = actor.sample(now + Duration::from_secs(4), true);
        assert_eq!(resting.pose, Pose::Rest);
        assert_eq!(resting.cue, Some(Cue::Sleep));
        assert!(!resting.animating);
        assert_eq!(resting, actor.sample(now + Duration::from_secs(60), true));
    }

    #[test]
    fn interruptions_retarget_from_the_current_position_and_offline_freezes() {
        let start = Instant::now();
        let now = start + Duration::from_millis(500);
        let mut actor = Actor::new(Activity::Executing, start);
        let position = actor.sample(now, true).position;
        actor.update(Activity::Approval, false, now);
        assert_eq!(actor.sample(now, true).position, position);
        let now = now + Duration::from_millis(150);
        let position = actor.sample(now, true).position;
        actor.update(Activity::Offline, false, now);
        let offline = actor.sample(now, true);
        assert_eq!(offline.position, position);
        assert!(!offline.animating);
        assert_eq!(offline, actor.sample(now + Duration::from_secs(10), true));
    }

    #[test]
    fn attention_is_finite_and_reduced_motion_always_uses_the_final_pose() {
        let now = Instant::now();
        for activity in [
            Activity::Approval,
            Activity::Input,
            Activity::Failed,
            Activity::Idle,
        ] {
            let actor = Actor::new(activity, now);
            let final_frame = actor.sample(now + Duration::from_secs(5), true);
            assert!(!final_frame.animating);
            assert_eq!(actor.sample(now, false), final_frame);
        }
        let actor = Actor::new(Activity::Thinking, now);
        let frame = actor.sample(now, false);
        assert_eq!(frame.pose, Pose::Think);
        assert!(!frame.animating);
    }

    #[test]
    fn unchanged_status_does_not_restart_walking_and_working_stays_at_its_desk() {
        let start = Instant::now();
        let mut actor = Actor::new(Activity::Thinking, start);
        let now = start + Duration::from_secs(3);
        actor.update(Activity::Thinking, false, now);
        assert_eq!(actor.sample(now, true).pose, Pose::Think);
        actor.update(Activity::Executing, false, now);
        assert_eq!(
            actor.sample(now, true).position,
            destination(Activity::Executing)
        );
        assert_eq!(actor.sample(now, true).pose, Pose::Type);
    }

    #[test]
    fn an_unverified_end_goes_to_rest_without_claiming_success() {
        let start = Instant::now();
        let mut actor = Actor::new(Activity::Executing, start);
        let now = start + Duration::from_secs(3);
        actor.update(Activity::Idle, false, now);
        assert_eq!(actor.sample(now, true).pose, Pose::Walk);
        assert_ne!(actor.sample(now, true).cue, Some(Cue::Completed));
    }

    #[test]
    fn switching_work_phase_on_the_way_to_the_desk_keeps_walking_forward() {
        let start = Instant::now();
        let now = start + Duration::from_millis(1200);
        let mut actor = Actor::new(Activity::Thinking, start);
        let before = actor.sample(now, true).position;
        actor.update(Activity::Executing, false, now);
        let after = actor
            .sample(now + Duration::from_millis(100), true)
            .position;
        assert_eq!(after.x, before.x);
        assert!(after.y < before.y, "同一工位的工作阶段变化不应走回通道");
    }

    #[test]
    fn cubicle_routes_pass_through_the_door_and_clear_the_front_partitions() {
        let positions = [layout::WORK, layout::WAIT, layout::REST, layout::ENTRANCE];
        let clear = |position: Position| {
            // 角色脚下的投影穿过前隔断时，整个人的宽度必须在门洞里。
            let feet = position.y + 20.;
            if (layout::FRONT_Y as f32..=(layout::FRONT_Y + layout::WALL_DEPTH) as f32)
                .contains(&feet)
            {
                assert!(position.x >= layout::DOOR_LEFT as f32);
                assert!(position.x + 13. < layout::DOOR_RIGHT as f32);
            }
        };
        for from in positions {
            for to in positions {
                let route = Route::new(Position::new(from.0, from.1), Position::new(to.0, to.1));
                for tick in 0..=200 {
                    let (position, _) = route.sample(route.duration() * tick as f32 / 200.);
                    clear(position);
                    // 中途改变状态也不能让角色从当前位置抄近路穿墙。
                    for target in positions {
                        let interrupted = Route::new(position, Position::new(target.0, target.1));
                        for tick in 0..=40 {
                            clear(
                                interrupted
                                    .sample(interrupted.duration() * tick as f32 / 40.)
                                    .0,
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn cast_separates_machines_keeps_existing_motion_and_removes_old_actors() {
        use crate::model::Worker;
        let room = |machine, activity| Room {
            machine,
            machine_name: "Machine".into(),
            root: "/repo".into(),
            name: "repo".into(),
            unavailable: None,
            workers: vec![Worker {
                conversation: "same-id".into(),
                title: "Task".into(),
                provider: "Codex".into(),
                icon: None,
                activity,
                completed: false,
                detail: None,
                look: 0,
            }],
        };
        let local = RuntimeKey::Local;
        let remote = RuntimeKey::Remote("server".into());
        let start = Instant::now();
        let now = start + Duration::from_secs(3);
        let mut cast = Cast::default();
        let mut rooms = vec![
            room(local.clone(), Activity::Thinking),
            room(remote.clone(), Activity::Approval),
        ];
        cast.sync(&rooms, start);
        rooms[0].workers[0].title = "Renamed".into();
        cast.sync(&rooms, now);
        assert_eq!(
            cast.frame(&local, "same-id", Activity::Thinking, now, true)
                .pose,
            Pose::Think
        );
        assert_eq!(
            cast.frame(&remote, "same-id", Activity::Approval, now, true)
                .pose,
            Pose::Wait
        );
        rooms.remove(0);
        cast.sync(&rooms, now);
        assert_eq!(cast.actors.len(), 1);
        cast.sync(&[], now);
        assert!(cast.actors.is_empty());
    }
}
