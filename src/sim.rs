use rand::RngExt;

use crate::config::Config;

pub struct Flake {
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub speed: f32,
    pub drift: f32,
}

/// Height (in pixels) added to the center of a settling mound.
const SETTLE_INCREMENT: f32 = 2.0;

/// Cosine-profile mound weights; index 4 is the settle column. Spreading the
/// snow over a small neighborhood turns single-column spikes into gentle slopes.
const MOUND_WEIGHTS: [f32; 9] = [0.0, 0.146, 0.5, 0.854, 1.0, 0.854, 0.5, 0.146, 0.0];

pub struct Simulation {
    pub flakes: Vec<Flake>,
    pub width: u32,
    pub height: u32,
    /// Accumulated snow height per pixel column, measured up from the bottom.
    pub ground: Vec<f32>,
}

impl Simulation {
    pub fn new(
        count: usize,
        width: u32,
        height: u32,
        config: &Config,
        rng: &mut impl rand::Rng,
    ) -> Self {
        let mut flakes = Vec::with_capacity(count);
        for _ in 0..count {
            flakes.push(Flake {
                x: rng.random_range(0.0..width as f32),
                y: rng.random_range(0.0..height as f32),
                size: config.size,
                speed: config.speed,
                drift: rng.random_range(-config.drift..=config.drift),
            });
        }
        Simulation {
            flakes,
            width,
            height,
            ground: vec![0.0; width as usize],
        }
    }

    pub fn step(&mut self, dt: f32) {
        for f in &mut self.flakes {
            f.y += f.speed * dt;
            f.x += f.drift * dt;
            if f.x > self.width as f32 + f.size {
                f.x = -f.size;
            }
            if f.x < -f.size {
                f.x = self.width as f32 + f.size;
            }

            // Settle on the accumulated snow (or the bottom of the screen) when
            // the flake's bottom edge reaches the surface, then respawn it at
            // the top.
            let col = f.x.floor().clamp(0.0, self.width as f32 - 1.0) as usize;
            let surface = self.height as f32 - self.ground[col];
            if f.y + f.size * 0.5 >= surface {
                // Add a smooth mound (not a single-column spike) so drifts form
                // gentle slopes.
                let center = f.x.floor() as i64;
                for (i, &w) in MOUND_WEIGHTS.iter().enumerate() {
                    let c = center + i as i64 - 4;
                    if c >= 0 && c < self.width as i64 {
                        self.ground[c as usize] += SETTLE_INCREMENT * w;
                    }
                }
                f.y = -f.size;
                f.x = (f.x * 1.319_573 + f.speed) % self.width as f32;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    fn flake() -> Flake {
        Flake {
            x: 50.0,
            y: 50.0,
            size: 4.0,
            speed: 100.0,
            drift: 5.0,
        }
    }

    fn sim(flakes: Vec<Flake>, width: u32, height: u32) -> Simulation {
        Simulation {
            flakes,
            width,
            height,
            ground: vec![0.0; width as usize],
        }
    }

    #[test]
    fn new_places_requested_count_in_bounds() {
        let cfg = Config::default();
        let mut rng = rand::rngs::StdRng::seed_from_u64(42);
        let sim = Simulation::new(10, 100, 200, &cfg, &mut rng);
        assert_eq!(sim.flakes.len(), 10);
        assert_eq!(sim.ground.len(), 100);
        assert!(sim.ground.iter().all(|&g| g == 0.0));
        for f in &sim.flakes {
            assert!(f.x >= 0.0 && f.x <= 100.0);
            assert!(f.y >= 0.0 && f.y <= 200.0);
        }
    }

    #[test]
    fn step_moves_down_and_sideways() {
        let mut sim = sim(vec![flake()], 100, 200);
        sim.step(1.0);
        assert_eq!(sim.flakes[0].y, 150.0); // 50 + 100*1
        assert_eq!(sim.flakes[0].x, 55.0); // 50 + 5*1
        assert!(sim.ground.iter().all(|&g| g == 0.0)); // too high to settle
    }

    #[test]
    fn step_settles_flake_on_ground() {
        let mut s = sim(
            vec![Flake {
                y: 205.0,
                ..flake()
            }],
            100,
            200,
        );
        s.step(0.0);
        assert_eq!(s.flakes[0].y, -4.0); // respawned at the top
        assert_eq!(s.ground[50], 2.0); // a lump accumulated at column 50
    }

    #[test]
    fn step_settles_on_existing_ground() {
        let mut s = sim(
            vec![Flake {
                y: 188.0,
                ..flake()
            }],
            100,
            200,
        );
        s.ground[50] = 10.0; // surface at y = 200 - 10 = 190
        s.step(0.0);
        // flake bottom (188 + 2 = 190) reaches the surface, so it settles.
        assert_eq!(s.ground[50], 12.0);
    }

    #[test]
    fn settle_spreads_a_mound() {
        let mut s = sim(
            vec![Flake {
                y: 205.0,
                ..flake()
            }],
            100,
            200,
        );
        s.step(0.0);
        assert_eq!(s.ground[50], 2.0); // center column
        assert!(s.ground[49] > 0.0); // neighbor got snow (mound slope)
        assert!(s.ground[51] > 0.0); // neighbor got snow (mound slope)
        assert_eq!(s.ground[46], 0.0); // outside the mound
    }

    #[test]
    fn step_wraps_horizontally() {
        // drift 10.0 pushes x to 109.0, past width + size (100 + 4), so it wraps to -size.
        let mut sim = sim(
            vec![Flake {
                x: 99.0,
                drift: 10.0,
                ..flake()
            }],
            100,
            200,
        );
        sim.step(1.0);
        assert_eq!(sim.flakes[0].x, -4.0);
    }
}
