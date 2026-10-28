use rand::RngExt;

use crate::config::Config;

pub struct Flake {
    pub x: f32,
    pub y: f32,
    pub size: f32,
    pub speed: f32,
    pub drift: f32,
}

pub struct Simulation {
    pub flakes: Vec<Flake>,
    pub width: u32,
    pub height: u32,
}

impl Simulation {
    pub fn new(count: usize, width: u32, height: u32, config: &Config, rng: &mut impl rand::Rng) -> Self {
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
        Simulation { flakes, width, height }
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
            if f.y - f.size > self.height as f32 {
                f.y = -f.size;
                f.x = (f.x * 1.618_034 + f.speed) % self.width as f32;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    fn flake() -> Flake {
        Flake { x: 50.0, y: 50.0, size: 4.0, speed: 100.0, drift: 5.0 }
    }

    #[test]
    fn new_places_requested_count_in_bounds() {
        let cfg = Config::default();
        let mut rng = rand::rngs::StdRng::seed_from_u64(42);
        let sim = Simulation::new(10, 100, 200, &cfg, &mut rng);
        assert_eq!(sim.flakes.len(), 10);
        for f in &sim.flakes {
            assert!(f.x >= 0.0 && f.x <= 100.0);
            assert!(f.y >= 0.0 && f.y <= 200.0);
        }
    }

    #[test]
    fn step_moves_down_and_sideways() {
        let mut sim = Simulation { flakes: vec![flake()], width: 100, height: 200 };
        sim.step(1.0);
        assert_eq!(sim.flakes[0].y, 150.0); // 50 + 100*1
        assert_eq!(sim.flakes[0].x, 55.0);  // 50 + 5*1
    }

    #[test]
    fn step_respawns_below_bottom() {
        let mut sim = Simulation { flakes: vec![Flake { y: 205.0, ..flake() }], width: 100, height: 200 };
        sim.step(0.0);
        assert_eq!(sim.flakes[0].y, -4.0); // -size
    }

    #[test]
    fn step_wraps_horizontally() {
        // drift 10.0 pushes x to 109.0, past width + size (100 + 4), so it wraps to -size.
        let mut sim = Simulation { flakes: vec![Flake { x: 99.0, drift: 10.0, ..flake() }], width: 100, height: 200 };
        sim.step(1.0);
        assert_eq!(sim.flakes[0].x, -4.0);
    }
}
