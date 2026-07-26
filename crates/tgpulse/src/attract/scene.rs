//! The island, built and projected the way the board would have.
//!
//! World space is right-handed with y up. Every surface is a quad, because the
//! Model 1 rasterizer takes quads; a triangle is a quad with two vertices in the
//! same place, which is how the fronds and the gulls are made.
//!
//! There is no lighting pass. Faces carry a flat colour chosen per surface and
//! shaded by hand -- the same thing the artists did, and the reason Model 1
//! scenes read as facets rather than gradients.

use std::f32::consts::TAU;

use tgpulse_core::model1_video::GpuQuad;
use tgpulse_core::tilemap::{SCREEN_H, SCREEN_W};

type Vec3 = [f32; 3];

/// Where the eye is and how it is oriented. The scene needs this before
/// projection, because anything meant to face the viewer -- snow, baubles --
/// has to be built in the camera's plane rather than a world one.
#[derive(Clone, Copy)]
struct Camera {
    eye: Vec3,
    right: Vec3,
    up: Vec3,
    forward: Vec3,
}

/// The eye orbits the island slowly and looks at a point just above it.
fn camera(t: f32) -> Camera {
    let angle = t / ORBIT_PERIOD * TAU;
    let eye = [
        angle.sin() * CAMERA_DISTANCE,
        CAMERA_HEIGHT,
        angle.cos() * CAMERA_DISTANCE,
    ];
    let target = [0.0f32, 2.6, 0.0];
    // A look-at basis: forward, then right and up from the world's up vector.
    let forward = normalize(sub(target, eye));
    let right = normalize(cross(forward, [0.0, 1.0, 0.0]));
    let up = cross(right, forward);
    Camera {
        eye,
        right,
        up,
        forward,
    }
}

/// How near the front of the frame a surface belongs, regardless of its
/// distance.
///
/// Sorting purely on depth puts a large sea tile in front of the island
/// whenever the tile's centre happens to be nearer than the island facet
/// behind it, which paints water across the sand. The games solved this the
/// same way: sort objects into priority bands first, then by depth within a
/// band, so a thing that is conceptually behind another can never be drawn
/// over it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Layer {
    Sky,
    Sea,
    Island,
    Tree,
    Air,
}

/// One quad before projection.
struct Face {
    points: [Vec3; 4],
    color: u32,
}

/// Distance from the eye to the origin, and how high it sits.
const CAMERA_DISTANCE: f32 = 15.0;
const CAMERA_HEIGHT: f32 = 4.2;
/// One full orbit of the island, in seconds.
const ORBIT_PERIOD: f32 = 48.0;
/// Focal length in screen widths; larger is a longer lens.
const FOCAL: f32 = 1.35;
/// Nearest a vertex may come to the eye before its quad is dropped. The
/// hardware clipped against the near plane; dropping is enough for a scene
/// that never puts the camera inside anything.
const NEAR: f32 = 0.35;

// The sea, faceted rather than smooth, in two tones so the wave shows.
const SEA_DARK: u32 = 0xFF17_4E86;
const SEA_LIGHT: u32 = 0xFF23_6FB4;
const SAND_LIGHT: u32 = 0xFFE0_B45C;
const SAND_MID: u32 = 0xFFC8_9A46;
const SAND_DARK: u32 = 0xFFA8_7C34;
/// Four shades, one per side of the trunk, so each facet reads as its own
/// flat surface rather than as part of a smooth cylinder. The logo does the
/// same thing: the trunk is legible as facets, not as a tube.
const TRUNK_SHADES: [u32; 4] = [0xFFD9_A85B, 0xFFB8_8944, 0xFF8F_6B33, 0xFFA5_7B3C];
const FROND_LIGHT: u32 = 0xFF3E_A64B;
const FROND_DARK: u32 = 0xFF25_7A33;
const COCONUT_LIGHT: u32 = 0xFFE0_3A34;
const COCONUT_DARK: u32 = 0xFFA8_1F20;
const GULL_BODY: u32 = 0xFFF2_F2F2;
const GULL_SHADE: u32 = 0xFFC2_C6CE;
const SNOW: u32 = 0xFFF4_F8FF;
const BAUBLE_GOLD: u32 = 0xFFE8_C24A;
const BAUBLE_RED: u32 = 0xFFD8_3A34;
const STAR_GOLD: u32 = 0xFFFF_DD5E;

pub struct Scene {
    /// True on 25 December, decided once at startup.
    festive: bool,
    /// Fixed per-gull and per-flake parameters, so the motion is varied without
    /// being random every frame.
    gulls: Vec<Gull>,
    flakes: Vec<Flake>,
}

struct Gull {
    radius: f32,
    height: f32,
    phase: f32,
    speed: f32,
    flap: f32,
    size: f32,
}

struct Flake {
    x: f32,
    z: f32,
    phase: f32,
    speed: f32,
    size: f32,
    drift: f32,
}

impl Scene {
    pub fn new() -> Self {
        let festive = is_christmas();
        if festive {
            log::info!(target: "attract", "merry christmas");
        }

        // A deterministic spread; the scene should look the same every launch.
        let gulls = (0..5)
            .map(|i| {
                let f = i as f32;
                Gull {
                    radius: 5.5 + f * 1.15,
                    height: 5.0 + (f * 0.9).sin() * 1.4,
                    phase: f * 1.27,
                    speed: 0.22 + f * 0.035,
                    flap: 3.1 + f * 0.4,
                    size: 0.5 + (f % 2.0) * 0.12,
                }
            })
            .collect();

        let flakes = (0..140)
            .map(|i| {
                let f = i as f32;
                Flake {
                    // Two incommensurate steps scatter the column positions
                    // without needing a generator.
                    x: ((f * 2.399_963).sin() * 9.0),
                    z: ((f * 1.618_034).cos() * 9.0),
                    phase: (f * 0.618_034).fract(),
                    speed: 0.55 + (f % 7.0) * 0.06,
                    size: 0.055 + (f % 3.0) * 0.02,
                    drift: (f * 0.7).sin() * 0.5,
                }
            })
            .collect();

        Self {
            festive,
            gulls,
            flakes,
        }
    }

    /// Fills the background tile layer with the sky.
    pub fn sky(&self, _t: f32, out: &mut [u32]) {
        // A vertical ramp, banded rather than smooth: the tile layers are
        // paletted, and a clean gradient would look wrong against the facets.
        let (top, bottom) = if self.festive {
            ([0x1E, 0x2A, 0x50], [0x86, 0x9C, 0xC4])
        } else {
            ([0x1B, 0x4F, 0x9C], [0x8F, 0xD2, 0xF0])
        };
        const BANDS: usize = 24;
        for y in 0..SCREEN_H {
            let band = y * BANDS / SCREEN_H;
            let k = band as f32 / (BANDS - 1) as f32;
            let channel = |a: i32, b: i32| (a as f32 + (b - a) as f32 * k) as u32;
            let color = 0xFF00_0000
                | (channel(top[0], bottom[0]) << 16)
                | (channel(top[1], bottom[1]) << 8)
                | channel(top[2], bottom[2]);
            out[y * SCREEN_W..(y + 1) * SCREEN_W].fill(color);
        }
    }

    /// Builds the frame's quads, painter-sorted back to front.
    pub fn build(&self, t: f32, render_width: u32) -> Vec<GpuQuad> {
        let camera = camera(t);
        let mut faces = Vec::with_capacity(1024);
        let mut layers: Vec<Layer> = Vec::with_capacity(1024);
        // Whatever the last builder pushed belongs to this layer. Doing it here
        // keeps the builders free of any notion of draw order.
        let stamp = |faces: &Vec<Face>, layers: &mut Vec<Layer>, layer: Layer| {
            layers.resize(faces.len(), layer);
        };

        self.sky_wall(&mut faces);
        stamp(&faces, &mut layers, Layer::Sky);
        self.sea(t, &mut faces);
        stamp(&faces, &mut layers, Layer::Sea);
        self.island(&mut faces);
        stamp(&faces, &mut layers, Layer::Island);

        self.trunk(t, &mut faces);
        if self.festive {
            self.fir(t, camera, &mut faces);
        } else {
            self.fronds(t, &mut faces);
        }
        self.coconut(t, &mut faces);
        stamp(&faces, &mut layers, Layer::Tree);

        if self.festive {
            self.snow(t, camera, &mut faces);
        }
        self.gulls(t, &mut faces);
        stamp(&faces, &mut layers, Layer::Air);

        project(&faces, &layers, camera, render_width)
    }

    /// The sky, as a ring of quads far enough out to sit behind everything.
    ///
    /// The tile layer behind the 3D is only as wide as the board's frame, so on
    /// a window wider than 4:3 it does not reach the sides. Putting the sky in
    /// the scene instead means it reaches wherever the picture does.
    fn sky_wall(&self, out: &mut Vec<Face>) {
        const SIDES: usize = 16;
        const RADIUS: f32 = 60.0;
        // Bands up the wall, bottom to top, matching the tile layer's ramp.
        let bands: [(f32, f32, u32); 3] = if self.festive {
            [
                (-8.0, 6.0, 0xFF86_9CC4),
                (6.0, 20.0, 0xFF52_6C9C),
                (20.0, 46.0, 0xFF1E_2A50),
            ]
        } else {
            [
                (-8.0, 6.0, 0xFF8F_D2F0),
                (6.0, 20.0, 0xFF4E_92CE),
                (20.0, 46.0, 0xFF1B_4F9C),
            ]
        };

        for i in 0..SIDES {
            let (a0, a1) = (
                i as f32 / SIDES as f32 * TAU,
                (i + 1) as f32 / SIDES as f32 * TAU,
            );
            let (p0, p1) = (
                [a0.cos() * RADIUS, 0.0, a0.sin() * RADIUS],
                [a1.cos() * RADIUS, 0.0, a1.sin() * RADIUS],
            );
            for (y0, y1, color) in bands {
                out.push(Face {
                    points: [
                        [p0[0], y0, p0[2]],
                        [p1[0], y0, p1[2]],
                        [p1[0], y1, p1[2]],
                        [p0[0], y1, p0[2]],
                    ],
                    color,
                });
            }
        }
    }

    /// A grid of sea tiles with a travelling swell. The facets are the point:
    /// each tile is flat, so the wave reads as a fold rather than a curve.
    fn sea(&self, t: f32, out: &mut Vec<Face>) {
        const TILES: i32 = 14;
        const EXTENT: f32 = 26.0;
        let step = EXTENT * 2.0 / TILES as f32;
        let wave =
            |x: f32, z: f32| ((x * 0.35 + t * 1.1).sin() + (z * 0.27 - t * 0.8).sin()) * 0.22;

        for iz in 0..TILES {
            for ix in 0..TILES {
                let x0 = -EXTENT + ix as f32 * step;
                let z0 = -EXTENT + iz as f32 * step;
                let (x1, z1) = (x0 + step, z0 + step);
                let points = [
                    [x0, wave(x0, z0), z0],
                    [x1, wave(x1, z0), z0],
                    [x1, wave(x1, z1), z1],
                    [x0, wave(x0, z1), z1],
                ];
                // Tint by the tile's own height, so the swell is visible
                // without a lighting pass.
                let lift = points.iter().map(|p| p[1]).sum::<f32>() / 4.0;
                out.push(Face {
                    points,
                    color: if lift > 0.0 { SEA_LIGHT } else { SEA_DARK },
                });
            }
        }
    }

    /// A faceted sand dome: concentric rings closing to a cap, like the
    /// logo's. The radii shrink faster than the height rises, which is what
    /// gives a sandbar its low, wide profile.
    fn island(&self, out: &mut Vec<Face>) {
        const SIDES: usize = 14;
        // (radius, height) from the waterline inward.
        const RINGS: [(f32, f32); 4] = [(4.8, -0.06), (3.7, 0.42), (2.5, 0.80), (1.2, 1.02)];
        let ring = |radius: f32, y: f32| -> Vec<Vec3> {
            (0..SIDES)
                .map(|i| {
                    // A half-step twist per ring keeps the facets from lining
                    // up into stripes down the slope.
                    let a = (i as f32 + y * 0.6) / SIDES as f32 * TAU;
                    [a.cos() * radius, y, a.sin() * radius]
                })
                .collect()
        };
        let rings: Vec<Vec<Vec3>> = RINGS.iter().map(|(r, y)| ring(*r, *y)).collect();
        let shades = [SAND_LIGHT, SAND_MID, SAND_DARK];

        for (band, pair) in rings.windows(2).enumerate() {
            for i in 0..SIDES {
                let j = (i + 1) % SIDES;
                out.push(Face {
                    points: [pair[0][i], pair[0][j], pair[1][j], pair[1][i]],
                    color: shades[(band + i) % shades.len()],
                });
            }
        }
        // The cap closes the top ring onto the summit.
        let summit: Vec3 = [0.0, 1.12, 0.0];
        let top = rings.last().expect("rings");
        for i in 0..SIDES {
            let j = (i + 1) % SIDES;
            out.push(Face {
                points: [top[i], top[j], summit, summit],
                color: shades[i % 2],
            });
        }
    }

    /// The trunk: a stack of four-sided segments, leaning, with a slow sway.
    ///
    /// Each segment keeps one radius top to bottom, so it is a prism rather
    /// than a frustum, and the radius steps down between them. That leaves a
    /// visible shoulder at every joint. Tapering each segment continuously
    /// instead reads as a smooth cone, and a smooth cone is the one thing this
    /// hardware could not draw.
    fn trunk(&self, t: f32, out: &mut Vec<Face>) {
        const SEGMENTS: usize = 5;
        for s in 0..SEGMENTS {
            let (a, b) = (s as f32 / SEGMENTS as f32, (s + 1) as f32 / SEGMENTS as f32);
            let (lower, upper) = (trunk_point(a, t), trunk_point(b, t));
            let radius = 0.40 - a * 0.17;
            let above = 0.40 - b * 0.17;
            // An eighth-turn per segment, so the vertical edges of one sit over
            // the flats of the one below rather than running straight up.
            let twist = s as f32 * TAU / 8.0;
            let angles = |face: usize| {
                (
                    face as f32 / 4.0 * TAU + 0.4 + twist,
                    (face + 1) as f32 / 4.0 * TAU + 0.4 + twist,
                )
            };

            for face in 0..4 {
                let (c0, c1) = angles(face);
                out.push(Face {
                    points: [
                        offset(lower, c0, radius),
                        offset(lower, c1, radius),
                        offset(upper, c1, radius),
                        offset(upper, c0, radius),
                    ],
                    color: TRUNK_SHADES[(face + s) % TRUNK_SHADES.len()],
                });
            }

            // The step: the ring of this segment's top that the narrower one
            // above does not cover.
            let next_twist = TAU / 8.0;
            for face in 0..4 {
                let (c0, c1) = angles(face);
                out.push(Face {
                    points: [
                        offset(upper, c0, radius),
                        offset(upper, c1, radius),
                        offset(upper, c1 + next_twist, above),
                        offset(upper, c0 + next_twist, above),
                    ],
                    color: TRUNK_SHADES[(face + s + 2) % TRUNK_SHADES.len()],
                });
            }
        }
    }

    /// Six drooping fronds, each a strip of quads narrowing to a point.
    fn fronds(&self, t: f32, out: &mut Vec<Face>) {
        const FRONDS: usize = 6;
        const JOINTS: usize = 4;
        let crown = trunk_point(1.0, t);

        for f in 0..FRONDS {
            let a = f as f32 / FRONDS as f32 * TAU;
            // Each frond bobs on its own phase, so the crown is never rigid.
            let bob = (t * 1.3 + f as f32 * 1.1).sin() * 0.12;
            let (dx, dz) = (a.cos(), a.sin());

            let mut previous = crown;
            let mut previous_half = 0.10;
            for j in 1..=JOINTS {
                let k = j as f32 / JOINTS as f32;
                // Out and then down: the arc is what makes it a palm.
                let reach = k * 2.3;
                let drop = k * k * 1.25 - bob * k;
                let point = [
                    crown[0] + dx * reach,
                    crown[1] + 0.35 - drop,
                    crown[2] + dz * reach,
                ];
                let half = 0.42 * (1.0 - k * 0.85);
                // The frond's width is across the radial direction.
                let (sx, sz) = (-dz, dx);
                out.push(Face {
                    points: [
                        [
                            previous[0] + sx * previous_half,
                            previous[1],
                            previous[2] + sz * previous_half,
                        ],
                        [point[0] + sx * half, point[1], point[2] + sz * half],
                        [point[0] - sx * half, point[1], point[2] - sz * half],
                        [
                            previous[0] - sx * previous_half,
                            previous[1],
                            previous[2] - sz * previous_half,
                        ],
                    ],
                    color: if (f + j) % 2 == 0 {
                        FROND_LIGHT
                    } else {
                        FROND_DARK
                    },
                });
                previous = point;
                previous_half = half;
            }
        }
    }

    /// The Christmas variant: conical tiers of green, baubles, and a star.
    fn fir(&self, t: f32, camera: Camera, out: &mut Vec<Face>) {
        const TIERS: usize = 4;
        const SIDES: usize = 8;
        let crown = trunk_point(1.0, t);

        for tier in 0..TIERS {
            let k = tier as f32 / TIERS as f32;
            let base_y = crown[1] - 0.35 + k * 0.92;
            let radius = 1.8 * (1.0 - k * 0.72);
            // Short enough that each tier's skirt stays visible below the
            // one above, rather than the stack merging into one cone.
            let peak = base_y + 0.72;
            for s in 0..SIDES {
                let (a0, a1) = (
                    s as f32 / SIDES as f32 * TAU,
                    (s + 1) as f32 / SIDES as f32 * TAU,
                );
                let p0 = [
                    crown[0] + a0.cos() * radius,
                    base_y,
                    crown[2] + a0.sin() * radius,
                ];
                let p1 = [
                    crown[0] + a1.cos() * radius,
                    base_y,
                    crown[2] + a1.sin() * radius,
                ];
                let apex = [crown[0], peak, crown[2]];
                out.push(Face {
                    points: [p0, p1, apex, apex],
                    color: if (s + tier) % 2 == 0 {
                        FROND_LIGHT
                    } else {
                        FROND_DARK
                    },
                });
            }
        }

        // Baubles hung around the tiers, each a small facing quad.
        for i in 0..10 {
            let f = i as f32;
            let tier = (i % TIERS) as f32 / TIERS as f32;
            let a = f * 2.4 + t * 0.15;
            let radius = 1.8 * (1.0 - tier * 0.72) * 0.86;
            let centre = [
                crown[0] + a.cos() * radius,
                crown[1] - 0.2 + tier * 0.92,
                crown[2] + a.sin() * radius,
            ];
            billboard(
                out,
                camera,
                centre,
                0.14,
                if i % 2 == 0 { BAUBLE_RED } else { BAUBLE_GOLD },
            );
        }

        // A star on top, as a cross of two quads so it reads from any angle.
        // Just above the topmost tier's peak.
        let star = [
            crown[0],
            crown[1] - 0.35 + 0.75 * 0.92 + 0.72 + 0.30,
            crown[2],
        ];
        billboard(out, camera, star, 0.26, STAR_GOLD);
        // A second, taller quad across the first, so it reads as a star
        // rather than a lozenge.
        let r = camera.right;
        let u = camera.up;
        let point = |sx: f32, sy: f32| {
            [
                star[0] + r[0] * sx + u[0] * sy,
                star[1] + r[1] * sx + u[1] * sy,
                star[2] + r[2] * sx + u[2] * sy,
            ]
        };
        out.push(Face {
            points: [
                point(-0.09, -0.42),
                point(0.09, -0.42),
                point(0.09, 0.42),
                point(-0.09, 0.42),
            ],
            color: STAR_GOLD,
        });
    }

    /// The red coconut from the logo, as a faceted lump at the crown.
    fn coconut(&self, t: f32, out: &mut Vec<Face>) {
        let crown = trunk_point(1.0, t);
        let centre = [crown[0], crown[1] + 0.18, crown[2]];
        const SIDES: usize = 7;
        let radius = 0.44;
        let ring: Vec<Vec3> = (0..SIDES)
            .map(|i| {
                let a = i as f32 / SIDES as f32 * TAU;
                [
                    centre[0] + a.cos() * radius,
                    centre[1],
                    centre[2] + a.sin() * radius,
                ]
            })
            .collect();
        let top = [centre[0], centre[1] + radius, centre[2]];
        let bottom = [centre[0], centre[1] - radius, centre[2]];
        for i in 0..SIDES {
            let j = (i + 1) % SIDES;
            out.push(Face {
                points: [ring[i], ring[j], top, top],
                color: if i % 2 == 0 {
                    COCONUT_LIGHT
                } else {
                    COCONUT_DARK
                },
            });
            out.push(Face {
                points: [ring[j], ring[i], bottom, bottom],
                color: if i % 2 == 0 {
                    COCONUT_DARK
                } else {
                    COCONUT_LIGHT
                },
            });
        }
    }

    /// Gulls: a body quad and two wings that flap, circling the island.
    fn gulls(&self, t: f32, out: &mut Vec<Face>) {
        for gull in &self.gulls {
            let a = gull.phase + t * gull.speed * TAU / 4.0;
            let centre = [
                a.cos() * gull.radius,
                gull.height + (t * 0.6 + gull.phase).sin() * 0.35,
                a.sin() * gull.radius,
            ];
            // Heading is the tangent of the circle it is flying.
            let (hx, hz) = (-a.sin(), a.cos());
            let (wx, wz) = (-hz, hx);
            let lift = (t * gull.flap + gull.phase).sin() * gull.size * 0.75;
            let s = gull.size;

            // A dark body along the heading, so the silhouette reads against
            // the sky instead of dissolving into the wings.
            let nose = [
                centre[0] + hx * s * 0.9,
                centre[1],
                centre[2] + hz * s * 0.9,
            ];
            let tail = [
                centre[0] - hx * s * 0.8,
                centre[1],
                centre[2] - hz * s * 0.8,
            ];
            let half = s * 0.13;
            out.push(Face {
                points: [
                    nose,
                    [tail[0] + wx * half, tail[1], tail[2] + wz * half],
                    [tail[0] - wx * half, tail[1], tail[2] - wz * half],
                    nose,
                ],
                color: GULL_SHADE,
            });

            // Two wings, swept back from the shoulder and beating together.
            // The sweep is what makes it a bird rather than a dart: the tip
            // trails well behind the shoulder rather than sitting square to it.
            for side in [-1.0f32, 1.0] {
                let shoulder_front = [
                    centre[0] + hx * s * 0.35,
                    centre[1],
                    centre[2] + hz * s * 0.35,
                ];
                let shoulder_back = [
                    centre[0] - hx * s * 0.35,
                    centre[1],
                    centre[2] - hz * s * 0.35,
                ];
                // The crook of the wing, part way out and already rising.
                let elbow = [
                    centre[0] + wx * s * 1.1 * side - hx * s * 0.15,
                    centre[1] + lift * 0.45,
                    centre[2] + wz * s * 1.1 * side - hz * s * 0.15,
                ];
                let tip = [
                    centre[0] + wx * s * 2.2 * side - hx * s * 1.4,
                    centre[1] + lift,
                    centre[2] + wz * s * 2.2 * side - hz * s * 1.4,
                ];
                // Inner panel: shoulder to elbow.
                out.push(Face {
                    points: [shoulder_front, elbow, elbow, shoulder_back],
                    color: GULL_BODY,
                });
                // Outer panel: elbow to the swept tip, shaded a touch darker so
                // the bend in the wing is visible.
                out.push(Face {
                    points: [shoulder_front, elbow, tip, tip],
                    color: if lift > 0.0 { GULL_BODY } else { GULL_SHADE },
                });
            }
        }
    }

    /// Falling snow, as small quads that face the camera.
    fn snow(&self, t: f32, camera: Camera, out: &mut Vec<Face>) {
        const TOP: f32 = 11.0;
        const FLOOR: f32 = -0.2;
        for flake in &self.flakes {
            // Each flake falls on its own loop, wrapping back to the top.
            let fall = (flake.phase + t * flake.speed * 0.09).fract();
            let y = TOP - fall * (TOP - FLOOR);
            let sway = (t * 0.9 + flake.phase * TAU).sin() * flake.drift;
            billboard(out, camera, [flake.x + sway, y, flake.z], flake.size, SNOW);
        }
    }
}

/// A small square quad turned to face the eye.
///
/// Built from the camera's own right and up vectors, so it stays square from
/// every angle. Left in a world-aligned plane it collapses to a bar as the
/// camera comes round -- which is what snow and baubles did before.
fn billboard(out: &mut Vec<Face>, camera: Camera, centre: Vec3, half: f32, color: u32) {
    let r = [
        camera.right[0] * half,
        camera.right[1] * half,
        camera.right[2] * half,
    ];
    let u = [
        camera.up[0] * half,
        camera.up[1] * half,
        camera.up[2] * half,
    ];
    let corner = |sx: f32, sy: f32| {
        [
            centre[0] + r[0] * sx + u[0] * sy,
            centre[1] + r[1] * sx + u[1] * sy,
            centre[2] + r[2] * sx + u[2] * sy,
        ]
    };
    out.push(Face {
        points: [
            corner(-1.0, -1.0),
            corner(1.0, -1.0),
            corner(1.0, 1.0),
            corner(-1.0, 1.0),
        ],
        color,
    });
}

/// A point up the trunk, `k` from base to crown, including the lean and sway.
fn trunk_point(k: f32, t: f32) -> Vec3 {
    let sway = (t * 0.75).sin() * 0.12 + (t * 0.31).sin() * 0.05;
    [
        // The lean is quadratic so the base stays planted.
        k * k * (0.55 + sway),
        1.0 + k * 3.4,
        k * k * -0.18,
    ]
}

/// A point on a circle of `radius` around `centre`, at angle `a`.
fn offset(centre: Vec3, a: f32, radius: f32) -> Vec3 {
    [
        centre[0] + a.cos() * radius,
        centre[1],
        centre[2] + a.sin() * radius,
    ]
}

/// Transforms, projects, sorts and packs the faces for the rasterizer.
fn project(faces: &[Face], layers: &[Layer], camera: Camera, render_width: u32) -> Vec<GpuQuad> {
    let Camera {
        eye,
        right,
        up,
        forward,
    } = camera;

    let width = render_width.max(1) as f32;
    let height = SCREEN_H as f32;
    // The 3D layer may be rendered wider than the tile layers in widescreen;
    // the centre follows it so the island stays in the middle of the picture.
    let (cx, cy) = (width * 0.5, height * 0.5);
    let scale = width * FOCAL;

    let mut sorted: Vec<(Layer, f32, GpuQuad)> = Vec::with_capacity(faces.len());
    for (face, layer) in faces.iter().zip(layers) {
        let mut xs = [0i32; 4];
        let mut ys = [0i32; 4];
        let mut depth = 0.0f32;
        let mut visible = true;

        for (i, point) in face.points.iter().enumerate() {
            let v = sub(*point, eye);
            let camera = [dot(v, right), dot(v, up), dot(v, forward)];
            if camera[2] < NEAR {
                visible = false;
                break;
            }
            xs[i] = (cx + camera[0] * scale / camera[2]) as i32;
            ys[i] = (cy - camera[1] * scale / camera[2]) as i32;
            depth += camera[2];
        }
        if !visible {
            continue;
        }

        sorted.push((
            *layer,
            depth * 0.25,
            GpuQuad {
                xs,
                ys,
                viewport: [0, render_width as i32 - 1, 0, SCREEN_H as i32 - 1],
                color: face.color,
                moire: 0,
                pad: [0; 2],
            },
        ));
    }

    // Painter's algorithm: by layer first, then far to near inside it. The
    // per-layer pass is what stops the sea from washing over the sand.
    sorted.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.total_cmp(&a.1)));
    sorted.into_iter().map(|(_, _, quad)| quad).collect()
}

fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(v: Vec3) -> Vec3 {
    let n = dot(v, v).sqrt();
    if n == 0.0 {
        v
    } else {
        [v[0] / n, v[1] / n, v[2] / n]
    }
}

/// Whether today is Christmas Day, from the system clock.
///
/// Written out rather than taken from a date library: the emulator needs one
/// day of the year and nothing else about calendars.
fn is_christmas() -> bool {
    let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
        return false;
    };
    let (month, day) = civil_month_day(now.as_secs() / 86_400);
    month == 12 && day == 25
}

/// Days since 1970-01-01 to (month, day), by Howard Hinnant's civil_from_days.
fn civil_month_day(days: u64) -> (u32, u32) {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_match_known_days() {
        // 1970-01-01, 2000-02-29 (a leap day), and a Christmas.
        assert_eq!(civil_month_day(0), (1, 1));
        assert_eq!(civil_month_day(11_016), (2, 29));
        assert_eq!(civil_month_day(20_812), (12, 25));
    }

    #[test]
    fn the_scene_builds_sorted_quads() {
        let scene = Scene::new();
        let quads = scene.build(1.0, SCREEN_W as u32);
        assert!(quads.len() > 200, "expected a populated scene");
        // Every quad lands inside the viewport it declares.
        for quad in &quads {
            assert_eq!(quad.viewport[1], SCREEN_W as i32 - 1);
        }
    }
}
