use glam::{Mat3, Quat, Vec2, Vec3};
use serde::{Deserialize, Serialize};
use stardust_xr_asteroids::{
	ClientState, Context, CustomElement, Element, Entity, Migrate, Reify, Tasker, Transformable,
	components::{Derezzable, Poseable},
	elements::{Dial, Handle, LineExt, Lines, Spatial, Text, line_from_points},
};
use stardust_xr_fusion::{
	client::FrameInfo,
	drawable::{Line, LinePoint, XAlign, YAlign},
	fields::Shape,
	types::{Color, Posef, rgba_linear},
};
use std::{collections::VecDeque, f32::consts::FRAC_PI_2};

/// meters per second of history at zero zoom
const SPEED: f32 = 0.025;
/// meters per millisecond of frame time at zero zoom
const MS_SCALE: f32 = 0.0015;
const MAX_SECONDS: f32 = 180.0;
const SECOND_STEPS: &[f32] = &[0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0];
const MS_STEPS: &[f32] = &[
	0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 25.0, 50.0, 100.0, 200.0, 500.0,
];
// strong-ipc drops anything over 8KiB and a LinePoint is 32 bytes, so lines go out in pieces
const CHUNK: u64 = 128;
const PIECE: usize = 200;
// clear of the density dial and the handles' own reach
const MIN_HEIGHT: f32 = 0.04;
const MAX_HEIGHT: f32 = 0.5;
const MIN_WIDTH: f32 = 0.05;

#[tokio::main(flavor = "current_thread")]
async fn main() {
	stardust_xr_asteroids::client::run::<State>(&[])
		.await
		.unwrap();
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct State {
	start: Vec3,
	end: Vec3,
	/// meters from 0ms to the top of the axis
	height: f32,
	/// radians around the time axis, 0 is upright
	tilt: f32,
	/// in turns of the dial, each one doubles the density
	x_zoom: f32,
	y_zoom: f32,

	/// (clock at the end of the frame, frame time) in seconds, oldest first
	#[serde(skip)]
	frames: VecDeque<(f64, f32)>,
	/// sum of every frame time so far
	#[serde(skip)]
	clock: f64,
	/// frames ever recorded, so chunks keep their keys as old frames fall off
	#[serde(skip)]
	count: u64,
}
impl Default for State {
	fn default() -> Self {
		State {
			start: Vec3::new(-0.125, 0.0, -0.5),
			end: Vec3::new(0.125, 0.0, -0.5),
			height: 0.075,
			tilt: 0.0,
			x_zoom: 0.0,
			y_zoom: 0.0,
			frames: VecDeque::new(),
			clock: 0.0,
			count: 0,
		}
	}
}
impl Migrate for State {
	type Old = Self;
}
impl ClientState for State {
	const APP_ID: &'static str = "org.stardustxr.plancktime";

	fn on_frame(&mut self, _context: &Context, info: &FrameInfo) {
		self.clock += info.delta as f64;
		self.count += 1;
		self.frames.push_back((self.clock, info.delta));
		while self
			.frames
			.front()
			.is_some_and(|(t, _)| self.age(*t) > MAX_SECONDS)
		{
			self.frames.pop_front();
		}
	}
}
impl Reify for State {
	fn reify(
		&self,
		context: &Context,
		_tasks: impl Tasker<Self>,
		_props: (),
	) -> impl Element<Self> {
		let (center, rot, size) = self.body();
		Spatial::default()
			.build()
			.child(self.graph())
			.child(
				Entity::new(Shape::Box { size: size.into() })
					.pos(center)
					.rot(rot)
					.component(Poseable::new(|s: &mut Self, p| s.pose(p)))
					.component(Derezzable::program_stopper(context))
					.build(),
			)
			.child(
				Handle::new(self.start, |s: &mut Self, p| {
					s.start = apart(p.into(), s.end, s.start)
				})
				.build(),
			)
			.child(
				Handle::new(self.end, |s: &mut Self, p| {
					s.end = apart(p.into(), s.start, s.end)
				})
				.build(),
			)
	}
}
impl State {
	fn graph(&self) -> impl Element<Self> {
		let w = self.start.distance(self.end);
		let speed = SPEED * self.x_zoom.exp2();
		let per_ms = MS_SCALE * self.y_zoom.exp2();
		let time = Axis::new(speed, (w / speed).min(MAX_SECONDS), SECOND_STEPS, 0.015);
		let ms = Axis::new(per_ms, self.height / per_ms, MS_STEPS, 0.008);
		let top = ms.length();
		let tilt = Quat::from_rotation_x(self.tilt);

		// the height handle stays outside the tilt so it can set it without chasing itself
		Spatial::default()
			.pos(self.start)
			.rot(facing(self.start, self.end))
			.build()
			.child(
				Handle::new(tilt * Vec3::new(w, top, 0.0), |s: &mut Self, p| {
					s.height = Vec2::new(p.y, p.z).length().clamp(MIN_HEIGHT, MAX_HEIGHT);
					s.tilt = p.z.atan2(p.y);
				})
				.build(),
			)
			.child(self.plot(w, time, ms))
	}

	fn plot(&self, w: f32, time: Axis, ms: Axis) -> impl Element<Self> {
		let top = ms.length();
		let current = self.frames.back().map_or(0.0, |(_, d)| d * 1000.0);
		Spatial::default()
			.rot(Quat::from_rotation_x(self.tilt))
			.build()
			.children(pieces(scaffold(w, time, ms)))
			.stable_children(self.chunks(time, ms).into_iter().map(|c| {
				let x = w - self.age(c.t0) * time.scale;
				(c.key, Lines::new([c.line]).pos([x, 0.0, 0.0]).build())
			}))
			.children(time.marks().skip(1).map(|s| {
				Text::new(time.label(-s, "s"))
					.character_height(0.004)
					.align_x(XAlign::Center)
					.align_y(YAlign::Top)
					.pos([w - s * time.scale, -0.006, 0.0])
					.build()
			}))
			// keyed by rate since a text's color only lands when it's created
			.stable_children(rates(ms).map(|(fps, y, c)| {
				(
					fps,
					Text::new(format!("{fps}fps"))
						.character_height(0.004)
						.color(c)
						.align_x(XAlign::Right)
						.align_y(YAlign::Center)
						.pos([-0.006, y, 0.0])
						.build(),
				)
			}))
			.children(ms.marks().map(|v| {
				Text::new(ms.label(v, "ms"))
					.character_height(0.004)
					.align_y(YAlign::Center)
					.pos([w + 0.006, v * ms.scale, 0.0])
					.build()
			}))
			.child(zoom_dial(
				self.x_zoom,
				[0.02, 0.0, 0.0],
				Quat::from_rotation_y(FRAC_PI_2),
				|s, v| s.x_zoom = v,
			))
			.child(zoom_dial(
				self.y_zoom,
				[w, top - 0.02, 0.0],
				Quat::from_rotation_x(-FRAC_PI_2),
				|s, v| s.y_zoom = v,
			))
			.child(
				Text::new(format!("{current:.1}ms"))
					.character_height(0.008)
					.align_x(XAlign::Center)
					.align_y(YAlign::Bottom)
					.pos([w, top + 0.022, 0.0])
					.build(),
			)
			.child(
				Text::new(format!("{:.0}fps", self.fps()))
					.character_height(0.006)
					.align_x(XAlign::Center)
					.align_y(YAlign::Bottom)
					.pos([w, top + 0.014, 0.0])
					.build(),
			)
	}

	/// the middle of the plot clear of the dials and handles, as (center, rotation, size)
	fn body(&self) -> (Vec3, Quat, Vec3) {
		let w = self.start.distance(self.end);
		let (l, r) = (0.035, w - 0.015);
		let rot = facing(self.start, self.end) * Quat::from_rotation_x(self.tilt);
		let center = self.start + rot * Vec3::new((l + r) / 2.0, self.height / 2.0, 0.0);
		let size = Vec3::new(r - l, self.height - 0.01, 0.01).max(Vec3::splat(0.01));
		(center, rot, size)
	}

	/// carries the ends along with the body, any roll goes into the tilt
	fn pose(&mut self, pose: Posef) {
		let (center, rot, _) = self.body();
		let (c, r) = (Vec3::from(pose.position), Quat::from(pose.orientation));
		let d = r * rot.inverse();
		self.start = c + d * (self.start - center);
		self.end = c + d * (self.end - center);
		let up = facing(self.start, self.end).inverse() * r * Vec3::Y;
		self.tilt = up.z.atan2(up.y);
	}

	/// seconds since the frame that ended at `t`
	fn age(&self, t: f64) -> f32 {
		(self.clock - t) as f32
	}

	fn fps(&self) -> f32 {
		let (n, sum) = self
			.frames
			.iter()
			.rev()
			.take_while(|(t, _)| self.age(*t) < 0.5)
			.fold((0.0, 0.0), |(n, sum), (_, d)| (n + 1.0, sum + d));
		if sum > 0.0 { n / sum } else { 0.0 }
	}

	fn chunks(&self, time: Axis, ms: Axis) -> Vec<Chunk> {
		let point = |t: f64, d: f32, t0: f64| {
			let v = d * 1000.0;
			LinePoint {
				point: [
					(t - t0) as f32 * time.scale,
					v.min(ms.extent) * ms.scale,
					0.0,
				]
				.into(),
				thickness: 0.0015,
				color: heat(v),
			}
		};
		let first = self.count - self.frames.len() as u64;
		let mut chunks: Vec<Chunk> = Vec::new();
		for (i, &(t, d)) in (first..)
			.zip(&self.frames)
			.filter(|(_, (t, _))| self.age(*t) <= time.extent)
		{
			let key = i / CHUNK;
			match chunks.last_mut() {
				Some(c) if c.key == key => c.line.points.push(point(t, d, c.t0)),
				last => {
					// the previous chunk reaches over to this point so the line stays joined
					if let Some(c) = last {
						c.line.points.push(point(t, d, c.t0));
					}
					chunks.push(Chunk {
						key,
						t0: t,
						line: Line {
							points: vec![point(t, d, t)],
							cyclic: false,
						},
					});
				}
			}
		}
		chunks
	}
}

/// a run of history drawn relative to its first frame, so it only resends points when it
/// gains or loses one
struct Chunk {
	key: u64,
	t0: f64,
	line: Line,
}

/// units along one side of the graph, marked at the finest step that keeps marks `spacing` apart
#[derive(Clone, Copy)]
struct Axis {
	/// meters per unit
	scale: f32,
	/// units from the origin to the far end
	extent: f32,
	step: f32,
}
impl Axis {
	fn new(scale: f32, extent: f32, steps: &[f32], spacing: f32) -> Self {
		let step = steps
			.iter()
			.copied()
			.find(|s| s * scale >= spacing)
			.unwrap_or(steps[steps.len() - 1]);
		Axis {
			scale,
			extent,
			step,
		}
	}
	fn length(self) -> f32 {
		self.extent * self.scale
	}
	fn marks(self) -> impl Iterator<Item = f32> + Clone {
		(0..=(self.extent / self.step) as u32).map(move |k| k as f32 * self.step)
	}
	fn label(self, v: f32, unit: &str) -> String {
		if self.step < 1.0 {
			format!("{v:.1}{unit}")
		} else {
			format!("{v:.0}{unit}")
		}
	}
}

/// gridlines, tick marks and the axes themselves, time runs backwards from the ms axis on the right
fn scaffold(w: f32, time: Axis, ms: Axis) -> Vec<Line> {
	let rows = ms.marks().skip(1).map(|v| v * ms.scale);
	let cols = time.marks().skip(1).map(|s| w - s * time.scale);
	let grid = rows.clone().map(|y| {
		line_from_points(vec![[0.0, y, 0.0], [w, y, 0.0]])
			.thickness(0.0003)
			.color(rgba_linear!(0.25, 0.25, 0.25, 1.0))
	});
	let y_ticks =
		rows.map(|y| line_from_points(vec![[w, y, 0.0], [w + 0.004, y, 0.0]]).thickness(0.0005));
	let x_ticks =
		cols.map(|x| line_from_points(vec![[x, 0.0, 0.0], [x, -0.004, 0.0]]).thickness(0.0005));
	let axes = line_from_points(vec![[0.0, 0.0, 0.0], [w, 0.0, 0.0], [w, ms.length(), 0.0]])
		.thickness(0.001);
	let refs = rates(ms).flat_map(|(_, y, c)| {
		[
			line_from_points(vec![[0.0, y, 0.0], [w, y, 0.0]])
				.thickness(0.0002)
				.color(c),
			line_from_points(vec![[w, y, 0.0], [w + 0.004, y, 0.0]])
				.thickness(0.0005)
				.color(c),
		]
	});
	grid.chain(y_ticks)
		.chain(x_ticks)
		.chain(refs)
		.chain([axes])
		.collect()
}

/// common refresh rates that fit on the ms axis, as (fps, height, color)
fn rates(ms: Axis) -> impl Iterator<Item = (u32, f32, Color)> {
	[
		(60, rgba_linear!(0.8, 0.3, 1.0, 1.0)),
		(72, rgba_linear!(0.3, 0.5, 1.0, 1.0)),
		(90, rgba_linear!(0.2, 0.9, 1.0, 1.0)),
		(120, rgba_linear!(0.3, 1.0, 0.4, 1.0)),
		(144, rgba_linear!(1.0, 0.9, 0.2, 1.0)),
		(240, rgba_linear!(1.0, 0.55, 0.15, 1.0)),
	]
	.into_iter()
	.map(move |(fps, c)| (fps, 1000.0 / fps as f32, c))
	.filter(move |(_, v, _)| *v <= ms.extent)
	.map(move |(fps, v, c)| (fps, v * ms.scale, c))
}

// keeps each Lines under the 8KiB message cap
fn pieces(lines: Vec<Line>) -> Vec<impl Element<State>> {
	let mut out: Vec<(usize, Vec<Line>)> = Vec::new();
	for l in lines {
		match out.last_mut() {
			Some((n, piece)) if *n + l.points.len() <= PIECE => {
				*n += l.points.len();
				piece.push(l);
			}
			_ => out.push((l.points.len(), vec![l])),
		}
	}
	out.into_iter()
		.map(|(_, piece)| Lines::new(piece).build())
		.collect()
}

/// +X runs start to end, +Y stays as close to world up as it can
fn facing(start: Vec3, end: Vec3) -> Quat {
	let x = (end - start).normalize_or(Vec3::X);
	let z = x.cross(Vec3::Y).normalize_or(Vec3::Z);
	Quat::from_mat3(&Mat3::from_cols(x, z.cross(x), z))
}

/// `p` held at least MIN_WIDTH from `other`, keeping the old direction if they meet
fn apart(p: Vec3, other: Vec3, was: Vec3) -> Vec3 {
	let d = p - other;
	let dir = d.normalize_or((was - other).normalize_or(Vec3::X));
	other + dir * d.length().max(MIN_WIDTH)
}

/// white while under 90fps, warming to red by 30fps
fn heat(ms: f32) -> Color {
	let h = ((ms - 11.1) / 22.2).clamp(0.0, 1.0);
	rgba_linear!(1.0, 1.0 - h * 0.8, 1.0 - h, 1.0)
}

// a dial turns about its +Z, so `rot` aims that down the axis it zooms
fn zoom_dial(
	zoom: f32,
	pos: [f32; 3],
	rot: Quat,
	on_change: fn(&mut State, f32),
) -> impl Element<State> {
	Dial::create(zoom, on_change)
		.range(-2.0..3.0)
		.radius(0.01)
		.thickness(0.01)
		.pos(pos)
		.rot(rot)
		.build()
}
